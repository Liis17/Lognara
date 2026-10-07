//! Консервативный бюджет объектов до десериализации и нормализации.

use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};

#[derive(Debug, PartialEq)]
pub struct TooLarge;

#[derive(Debug)]
pub struct ModelBudget {
    remaining: usize,
}

impl ModelBudget {
    pub fn new(bytes: usize) -> Self {
        Self { remaining: bytes }
    }
    pub fn remaining(&self) -> usize {
        self.remaining
    }
    pub fn charge(&mut self, bytes: usize) -> Result<(), TooLarge> {
        self.remaining = self.remaining.checked_sub(bytes).ok_or(TooLarge)?;
        Ok(())
    }
}

/// Без дерева Value. Scratch serde для escaped strings покрывается до разбора.
pub(crate) fn validate_json(json: &str, budget: &mut ModelBudget) -> Result<bool, TooLarge> {
    budget.charge(json.len().checked_mul(2).ok_or(TooLarge)?)?;
    // Пиковое сосуществование корневого BTreeMap и HashMap attributes.
    budget.charge(4096)?;
    let mut exceeded = false;
    let mut decoder = serde_json::Deserializer::from_str(json);
    let valid = JsonScan {
        budget,
        depth: 0,
        exceeded: &mut exceeded,
    }
    .deserialize(&mut decoder)
    .and_then(|()| decoder.end())
    .is_ok();
    if exceeded { Err(TooLarge) } else { Ok(valid) }
}

struct JsonScan<'a> {
    budget: &'a mut ModelBudget,
    depth: usize,
    exceeded: &'a mut bool,
}

impl JsonScan<'_> {
    fn charge<E: Error>(&mut self, bytes: usize) -> Result<(), E> {
        self.budget.charge(bytes).map_err(|_| {
            *self.exceeded = true;
            E::custom("JSON exceeds model budget")
        })
    }
}

impl<'de> DeserializeSeed<'de> for JsonScan<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(mut self, decoder: D) -> Result<(), D::Error> {
        if self.depth > 64 {
            *self.exceeded = true;
            return Err(D::Error::custom("JSON exceeds depth budget"));
        }
        self.charge(256)?;
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for JsonScan<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }
    fn visit_unit<E: Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_bool<E: Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: Error>(mut self, text: &str) -> Result<(), E> {
        self.charge(text.len().saturating_mul(2))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        while sequence
            .next_element_seed(JsonScan {
                budget: &mut *self.budget,
                depth: self.depth + 1,
                exceeded: &mut *self.exceeded,
            })?
            .is_some()
        {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map
            .next_key_seed(JsonScan {
                budget: &mut *self.budget,
                depth: self.depth + 1,
                exceeded: &mut *self.exceeded,
            })?
            .is_some()
        {
            map.next_value_seed(JsonScan {
                budget: &mut *self.budget,
                depth: self.depth + 1,
                exceeded: &mut *self.exceeded,
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scans_json_without_conflating_syntax_and_resource_errors() {
        assert_eq!(
            validate_json(
                r#"{"a":[1,true,null,"\u0041"]}"#,
                &mut ModelBudget::new(8192)
            ),
            Ok(true)
        );
        assert_eq!(validate_json("[1,", &mut ModelBudget::new(8192)), Ok(false));
        let dense = format!("[{}null]", "null,".repeat(1000));
        assert_eq!(
            validate_json(&dense, &mut ModelBudget::new(32 << 10)),
            Err(TooLarge)
        );
        let deep = format!("{}0{}", "[".repeat(65), "]".repeat(65));
        assert_eq!(
            validate_json(&deep, &mut ModelBudget::new(1 << 20)),
            Err(TooLarge)
        );
    }
}
