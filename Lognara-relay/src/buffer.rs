//! События в оперативной памяти, сгруппированные по источнику, до отправки в core.

use std::collections::BTreeMap;
use std::sync::Mutex;

use tokio::sync::Notify;

use crate::core_wire::{Event, Group, Source};

pub struct Buffer {
    state: Mutex<State>,
    full: Notify,
    batch_size: usize,
    max_len: usize,
}

#[derive(Default)]
struct State {
    groups: BTreeMap<Source, Pending>,
    /// Сколько событий во всех группах.
    len: usize,
}

#[derive(Default)]
struct Pending {
    dropped: u64,
    events: Vec<Event>,
}

/// В буфере нет места; агенту стоит повторить отправку позже.
#[derive(Debug, PartialEq)]
pub struct Full;

impl Buffer {
    pub fn new(batch_size: usize, max_len: usize) -> Self {
        Self {
            state: Mutex::default(),
            full: Notify::new(),
            batch_size,
            max_len,
        }
    }

    /// Добавляет события источника целиком или, если они не помещаются в лимит, ничего.
    /// Пачку больше лимита принимает в пустой буфер, иначе агент повторял бы её бесконечно.
    pub fn push(&self, source: Source, dropped: u64, events: Vec<Event>) -> Result<(), Full> {
        let mut state = self.state.lock().unwrap();
        if state.len > 0 && state.len + events.len() > self.max_len {
            return Err(Full);
        }

        state.len += events.len();
        let group = state.groups.entry(source).or_default();
        group.dropped += dropped;
        group.events.extend(events);
        if state.len >= self.batch_size {
            self.full.notify_one();
        }
        Ok(())
    }

    /// Забирает все группы и число событий в них.
    /// При `partial = false` забирает, только если набрано `batch_size` событий.
    pub fn take(&self, partial: bool) -> Option<(Vec<Group>, usize)> {
        let mut state = self.state.lock().unwrap();
        if state.len == 0 || (!partial && state.len < self.batch_size) {
            return None;
        }

        let groups = std::mem::take(&mut state.groups)
            .into_iter()
            .map(|(source, pending)| Group {
                source,
                dropped: pending.dropped,
                events: pending.events,
            })
            .collect();
        Some((groups, std::mem::take(&mut state.len)))
    }

    /// Ждёт, пока в буфере наберётся `batch_size` событий.
    pub async fn full(&self) {
        self.full.notified().await
    }
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

        let (groups, len) = buffer.take(true).unwrap();

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
        let (_, len) = buffer.take(false).unwrap();
        assert_eq!(len, 3);

        buffer.push(source("api"), 0, events(&["4"])).unwrap();
        let (_, len) = buffer.take(true).unwrap();
        assert_eq!(len, 1);
    }

    #[test]
    fn rejects_events_over_limit() {
        let buffer = Buffer::new(2, 3);
        buffer.push(source("api"), 0, events(&["1", "2"])).unwrap();

        assert_eq!(
            buffer.push(source("api"), 0, events(&["3", "4"])),
            Err(Full)
        );
        buffer.push(source("api"), 0, events(&["3"])).unwrap();

        let (_, len) = buffer.take(true).unwrap();
        assert_eq!(len, 3);
    }

    #[test]
    fn accepts_oversized_push_into_empty_buffer() {
        let buffer = Buffer::new(2, 3);

        buffer
            .push(source("api"), 0, events(&["1", "2", "3", "4"]))
            .unwrap();

        let (_, len) = buffer.take(true).unwrap();
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
