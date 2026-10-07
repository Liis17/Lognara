//! Консервативный бюджет модели до serde: без аллокаций и доверия к size_hint.
//! Паритет с общим scanner agent/relay проверяется кодированием и декодированием частей.
pub const DEFAULT_MODEL_BYTES: usize = 256 << 20;

#[derive(Debug, PartialEq)]
pub enum BudgetError {
    Invalid,
    TooLarge,
}

/// На каждый MessagePack-узел резервируется 128 байт плюс длина string/bin.
/// Запас покрывает Value, Vec capacity, ключи HashMap/BTreeMap и структуры wire.
pub fn validate(bytes: &[u8], budget: usize) -> Result<(), BudgetError> {
    let mut scan = Scan {
        bytes,
        position: 0,
        remaining: budget,
    };
    scan.value(0)?;
    if scan.position != bytes.len() {
        return Err(BudgetError::Invalid);
    }
    Ok(())
}

struct Scan<'a> {
    bytes: &'a [u8],
    position: usize,
    remaining: usize,
}

impl Scan<'_> {
    fn take(&mut self, count: usize) -> Result<&[u8], BudgetError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(BudgetError::Invalid)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(BudgetError::Invalid)?;
        self.position = end;
        Ok(value)
    }

    fn length(&mut self, width: usize) -> Result<usize, BudgetError> {
        self.take(width)?.iter().try_fold(0usize, |value, byte| {
            value
                .checked_mul(256)
                .and_then(|value| value.checked_add(*byte as usize))
                .ok_or(BudgetError::Invalid)
        })
    }

    fn charge(&mut self, bytes: usize) -> Result<(), BudgetError> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or(BudgetError::TooLarge)?;
        Ok(())
    }

    fn data(&mut self, length: usize) -> Result<(), BudgetError> {
        self.charge(length)?;
        self.take(length)?;
        Ok(())
    }

    fn children(&mut self, count: usize, depth: usize) -> Result<(), BudgetError> {
        if count > self.bytes.len() - self.position {
            return Err(BudgetError::Invalid);
        }
        for _ in 0..count {
            self.value(depth + 1)?;
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<(), BudgetError> {
        if depth > 64 {
            return Err(BudgetError::Invalid);
        }
        self.charge(128)?;
        let marker = self.take(1)?[0];
        match marker {
            0x00..=0x7f | 0xc0 | 0xc2 | 0xc3 | 0xe0..=0xff => Ok(()),
            0x80..=0x8f => self.children((marker & 15) as usize * 2, depth),
            0x90..=0x9f => self.children((marker & 15) as usize, depth),
            0xa0..=0xbf => self.data((marker & 31) as usize),
            0xc4 | 0xd9 => {
                let n = self.length(1)?;
                self.data(n)
            }
            0xc5 | 0xda => {
                let n = self.length(2)?;
                self.data(n)
            }
            0xc6 | 0xdb => {
                let n = self.length(4)?;
                self.data(n)
            }
            0xcc | 0xd0 => {
                self.take(1)?;
                Ok(())
            }
            0xcd | 0xd1 => {
                self.take(2)?;
                Ok(())
            }
            0xce | 0xd2 | 0xca => {
                self.take(4)?;
                Ok(())
            }
            0xcf | 0xd3 | 0xcb => {
                self.take(8)?;
                Ok(())
            }
            0xdc | 0xdd => {
                let count = self.length(if marker == 0xdc { 2 } else { 4 })?;
                self.children(count, depth)
            }
            0xde | 0xdf => {
                let count = self.length(if marker == 0xde { 2 } else { 4 })?;
                self.children(count.checked_mul(2).ok_or(BudgetError::Invalid)?, depth)
            }
            // Extension types не входят в контракт relay (UUID/trace/span — bin).
            _ => Err(BudgetError::Invalid),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_amplification_and_forged_container_lengths_before_allocation() {
        let mut nulls = vec![0xdc, 0x10, 0x00];
        nulls.extend(std::iter::repeat_n(0xc0, 4096));
        assert_eq!(validate(&nulls, 16 << 10), Err(BudgetError::TooLarge));
        assert_eq!(
            validate(&[0xdd, 0xff, 0xff, 0xff, 0xff], usize::MAX),
            Err(BudgetError::Invalid)
        );
        assert_eq!(
            validate(&[0xc0, 0xc0], usize::MAX),
            Err(BudgetError::Invalid)
        );
        assert_eq!(validate(&nulls, 128 * 4097), Ok(()));
    }
}
