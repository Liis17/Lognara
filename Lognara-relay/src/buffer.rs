//! События в оперативной памяти, сгруппированные по источнику, до отправки в core.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::core_wire::{CoreBatch, Event, Group, Source};
use crate::memory::{Pool, Reservation, ReserveError};

pub struct Buffer {
    state: Mutex<State>,
    full: Notify,
    batch_size: usize,
    max_len: usize,
    memory: Arc<Pool>,
}

#[derive(Default)]
struct State {
    groups: BTreeMap<Source, Pending>,
    /// Сколько событий во всех группах.
    len: usize,
    reservations: Vec<Reservation>,
    pressure: bool,
}

#[derive(Default)]
struct Pending {
    dropped: u64,
    events: Vec<Event>,
}

/// В буфере нет места; агенту стоит повторить отправку позже.
#[derive(Debug, PartialEq)]
pub enum Full {
    Busy,
    TooLarge,
}

/// Сначала уничтожаются модели, затем возвращаются их резервы.
pub struct Taken {
    pub batch: CoreBatch,
    pub reservations: Vec<Reservation>,
    pub len: usize,
}

impl Buffer {
    #[cfg(test)]
    pub fn new(batch_size: usize, max_len: usize) -> Self {
        Self::with_memory(
            batch_size,
            max_len,
            Pool::new(crate::memory::DEFAULT_BUFFER),
        )
    }

    pub fn with_memory(batch_size: usize, max_len: usize, memory: Arc<Pool>) -> Self {
        Self {
            state: Mutex::default(),
            full: Notify::new(),
            batch_size,
            max_len,
            memory,
        }
    }

    /// До тела отсекает уже известное заполнение; точный размер проверяется в push.
    pub fn check_admission(&self) -> Result<(), Full> {
        let mut state = self.state.lock().unwrap();
        let error = if self.memory.limit() < 4096 {
            Some(Full::TooLarge)
        } else if (state.len > 0 && state.len >= self.max_len)
            || self.memory.limit() - self.memory.used() < 4096
        {
            Some(Full::Busy)
        } else {
            None
        };
        if let Some(error) = error {
            state.pressure = true;
            self.full.notify_one();
            Err(error)
        } else {
            Ok(())
        }
    }

    /// Добавляет события источника целиком или, если они не помещаются в лимит, ничего.
    /// Пачку больше лимита принимает в пустой буфер, иначе агент повторял бы её бесконечно.
    pub fn push(&self, source: Source, dropped: u64, events: Vec<Event>) -> Result<(), Full> {
        let mut state = self.state.lock().unwrap();
        if state.len > 0 && events.len() > self.max_len.saturating_sub(state.len) {
            return Err(Full::Busy);
        }

        // Четыре capacity событий покрывают старый и новый Vec при realloc.
        // 4096 на push покрывают BTreeMap nodes, Group и Vec<Reservation>.
        let bytes = buffered_bytes(&source, &events, events.capacity());
        let reservation = self.memory.reserve(bytes).map_err(|error| {
            state.pressure = true;
            self.full.notify_one();
            match error {
                ReserveError::TooLarge => Full::TooLarge,
                ReserveError::Full => Full::Busy,
            }
        })?;

        state.len += events.len();
        state.reservations.push(reservation);
        let group = state.groups.entry(source).or_default();
        group.dropped = group.dropped.saturating_add(dropped);
        group.events.extend(events);
        state.pressure |= self.memory.used() >= self.memory.limit() / 2;
        if state.len >= self.batch_size || state.pressure {
            self.full.notify_one();
        }
        Ok(())
    }

    /// Забирает все группы и число событий в них.
    /// При `partial = false` забирает, только если набрано `batch_size` событий.
    pub fn take(&self, partial: bool) -> Option<Taken> {
        let mut state = self.state.lock().unwrap();
        if state.groups.is_empty() || (!partial && state.len < self.batch_size && !state.pressure) {
            return None;
        }

        let old = std::mem::take(&mut *state);
        let mut groups = Vec::with_capacity(old.groups.len());
        for (source, pending) in old.groups {
            groups.push(Group {
                source,
                dropped: pending.dropped,
                events: pending.events,
            });
        }
        Some(Taken {
            batch: CoreBatch { dropped: 0, groups },
            reservations: old.reservations,
            len: old.len,
        })
    }

    /// Ждёт, пока в буфере наберётся `batch_size` событий.
    pub async fn full(&self) {
        self.full.notified().await
    }
}

fn buffered_bytes(source: &Source, events: &[Event], capacity: usize) -> usize {
    let source_bytes = [&source.server, &source.backend, &source.service]
        .into_iter()
        .chain(source.environment.iter())
        .chain(source.service_instance.iter())
        .fold(0usize, |sum, text| sum.saturating_add(text.capacity()));
    events.iter().fold(
        4096usize
            .saturating_add(source_bytes)
            .saturating_add(capacity.saturating_mul(4 * std::mem::size_of::<Event>())),
        |sum, event| {
            let strings = [&event.message]
                .into_iter()
                .chain(event.action.iter())
                .chain(event.request_id.iter())
                .fold(0usize, |n, text| {
                    n.saturating_add(text.capacity().saturating_mul(2))
                });
            event.attributes.iter().fold(
                sum.saturating_add(strings)
                    .saturating_add(event.attributes.capacity().saturating_mul(256)),
                |n, (key, value)| {
                    n.saturating_add(key.capacity().saturating_mul(2))
                        .saturating_add(value_bytes(value))
                },
            )
        },
    )
}

fn value_bytes(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    let heap = match value {
        Value::String(text) => text.capacity().saturating_mul(2),
        Value::Array(values) => values
            .iter()
            .fold(values.capacity().saturating_mul(256), |n, v| {
                n.saturating_add(value_bytes(v))
            }),
        Value::Object(values) => values.iter().fold(0usize, |n, (key, v)| {
            n.saturating_add(256)
                .saturating_add(key.capacity().saturating_mul(2))
                .saturating_add(value_bytes(v))
        }),
        _ => 0,
    };
    256usize.saturating_add(heap)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;
    use crate::agent_wire::{Payload, Record};
    use crate::normalize;

    fn source(service: &str) -> Source {
        Source {
            environment: Some("production".into()),
            server: "eu-prod-01".into(),
            backend: "barkcloud".into(),
            service: service.into(),
            service_instance: None,
        }
    }

    #[test]
    fn byte_overflow_is_atomic_and_taken_models_keep_the_reservation() {
        let pool = Pool::new(12_000);
        let buffer = Buffer::with_memory(100, 100, pool.clone());
        buffer.push(source("api"), 7, events(&["first"])).unwrap();
        let used = pool.used();
        let large = "x".repeat(3000);
        assert_eq!(
            buffer.push(source("api"), 99, events(&[&large])),
            Err(Full::Busy)
        );
        let taken = buffer.take(false).unwrap();
        assert_eq!(taken.batch.groups[0].dropped, 7);
        assert_eq!(messages(&taken.batch.groups[0]), ["first"]);
        assert_eq!(pool.used(), used);
        assert_eq!(
            buffer.push(source("api"), 0, events(&[&large])),
            Err(Full::Busy)
        );
        drop(taken);
        assert_eq!(pool.used(), 0);
        buffer.push(source("api"), 0, events(&[&large])).unwrap();
    }

    #[test]
    fn empty_buffer_does_not_bypass_bytes_or_spare_capacity() {
        let buffer = Buffer::with_memory(1, 1, Pool::new(12_000));
        assert_eq!(
            buffer.push(source("api"), 0, events(&[&"x".repeat(10_000)])),
            Err(Full::TooLarge)
        );
        let mut spare = Vec::with_capacity(1000);
        spare.extend(events(&["small"]));
        assert_eq!(buffer.push(source("api"), 0, spare), Err(Full::TooLarge));
        assert!(buffer.take(true).is_none());
    }

    #[test]
    fn dropped_only_groups_are_flushed_and_release_memory() {
        let pool = Pool::new(12_000);
        let buffer = Buffer::with_memory(100, 100, pool.clone());
        buffer.push(source("api"), 7, Vec::new()).unwrap();
        let taken = buffer.take(true).unwrap();
        assert_eq!(taken.len, 0);
        assert_eq!(taken.batch.groups[0].dropped, 7);
        assert!(pool.used() > 0);
        drop(taken);
        assert_eq!(pool.used(), 0);
    }

    fn events(messages: &[&str]) -> Vec<Event> {
        messages
            .iter()
            .map(|message| {
                normalize::event(Record {
                    received_at: 1,
                    payload: Payload::Text(message.to_string()),
                })
            })
            .collect()
    }

    fn messages(group: &Group) -> Vec<&str> {
        group
            .events
            .iter()
            .map(|event| event.message.as_str())
            .collect()
    }

    #[test]
    fn groups_events_by_source() {
        let buffer = Buffer::new(100, 100);
        buffer.push(source("worker"), 0, events(&["w1"])).unwrap();
        buffer
            .push(source("api"), 3, events(&["a1", "a2"]))
            .unwrap();
        buffer.push(source("api"), 2, events(&["a3"])).unwrap();

        let taken = buffer.take(true).unwrap();
        let groups = &taken.batch.groups;
        let len = taken.len;

        assert_eq!(len, 4);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].source, source("api"));
        assert_eq!(groups[0].dropped, 5);
        assert_eq!(messages(&groups[0]), ["a1", "a2", "a3"]);
        assert_eq!(groups[1].source, source("worker"));
        assert_eq!(messages(&groups[1]), ["w1"]);
        assert!(buffer.take(true).is_none());
    }

    #[test]
    fn takes_only_full_batch_unless_partial() {
        let buffer = Buffer::new(3, 10);
        buffer.push(source("api"), 0, events(&["1", "2"])).unwrap();

        assert!(buffer.take(false).is_none());

        buffer.push(source("api"), 0, events(&["3"])).unwrap();
        let taken = buffer.take(false).unwrap();
        let len = taken.len;
        assert_eq!(len, 3);

        buffer.push(source("api"), 0, events(&["4"])).unwrap();
        let taken = buffer.take(true).unwrap();
        let len = taken.len;
        assert_eq!(len, 1);
    }

    #[test]
    fn rejects_events_over_limit() {
        let buffer = Buffer::new(2, 3);
        buffer.push(source("api"), 0, events(&["1", "2"])).unwrap();

        assert_eq!(
            buffer.push(source("api"), 0, events(&["3", "4"])),
            Err(Full::Busy)
        );
        buffer.push(source("api"), 0, events(&["3"])).unwrap();

        let taken = buffer.take(true).unwrap();
        let len = taken.len;
        assert_eq!(len, 3);
    }

    #[test]
    fn accepts_oversized_push_into_empty_buffer() {
        let buffer = Buffer::new(2, 3);

        buffer
            .push(source("api"), 0, events(&["1", "2", "3", "4"]))
            .unwrap();

        let taken = buffer.take(true).unwrap();
        let len = taken.len;
        assert_eq!(len, 4);
    }

    #[tokio::test]
    async fn signals_when_batch_is_full() {
        let buffer = Buffer::new(3, 10);

        buffer.push(source("api"), 0, events(&["1", "2"])).unwrap();
        let waited = timeout(Duration::from_millis(20), buffer.full()).await;
        assert!(waited.is_err());

        buffer.push(source("worker"), 0, events(&["3"])).unwrap();
        let waited = timeout(Duration::from_millis(20), buffer.full()).await;
        assert!(waited.is_ok());
    }
}
