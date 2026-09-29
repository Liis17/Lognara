//! Записи в оперативной памяти между приёмом и отправкой.

use std::collections::VecDeque;
use std::sync::Mutex;

use tokio::sync::Notify;

use crate::wire::Record;

pub struct Buffer {
    state: Mutex<State>,
    full: Notify,
    batch_size: usize,
    max_len: usize,
}

#[derive(Default)]
struct State {
    records: VecDeque<Record>,
    dropped: u64,
}

impl Buffer {
    pub fn new(batch_size: usize, max_len: usize) -> Self {
        Self {
            state: Mutex::default(),
            full: Notify::new(),
            batch_size,
            max_len,
        }
    }

    /// Добавляет записи; при переполнении вытесняет самые старые.
    pub fn push(&self, records: impl IntoIterator<Item = Record>) {
        let mut state = self.state.lock().unwrap();
        state.records.extend(records);

        let overflow = state.records.len().saturating_sub(self.max_len);
        if overflow > 0 {
            state.records.drain(..overflow);
            state.dropped += overflow as u64;
        }
        if state.records.len() >= self.batch_size {
            self.full.notify_one();
        }
    }

    /// Забирает до `batch_size` самых старых записей и счётчик вытесненных.
    /// При `partial = false` забирает только полную пачку.
    pub fn take(&self, partial: bool) -> Option<(Vec<Record>, u64)> {
        let mut state = self.state.lock().unwrap();
        let len = state.records.len();
        if len == 0 || (!partial && len < self.batch_size) {
            return None;
        }

        let records = state.records.drain(..len.min(self.batch_size)).collect();
        Some((records, std::mem::take(&mut state.dropped)))
    }

    pub fn len(&self) -> usize {
        self.state.lock().unwrap().records.len()
    }

    /// Ждёт, пока в буфере наберётся `batch_size` записей.
    pub async fn full(&self) {
        self.full.notified().await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;
    use crate::wire::Payload;

    fn records(range: std::ops::Range<i64>) -> Vec<Record> {
        range
            .map(|n| Record {
                received_at: n,
                payload: Payload::Text(n.to_string()),
            })
            .collect()
    }

    fn times(records: &[Record]) -> Vec<i64> {
        records.iter().map(|record| record.received_at).collect()
    }

    #[test]
    fn take_is_limited_by_batch_size() {
        let buffer = Buffer::new(3, 10);
        buffer.push(records(0..5));

        let (batch, dropped) = buffer.take(false).unwrap();
        assert_eq!(times(&batch), [0, 1, 2]);
        assert_eq!(dropped, 0);

        assert!(buffer.take(false).is_none());

        let (batch, _) = buffer.take(true).unwrap();
        assert_eq!(times(&batch), [3, 4]);
        assert!(buffer.take(true).is_none());
    }

    #[test]
    fn evicts_oldest_and_counts_dropped() {
        let buffer = Buffer::new(2, 4);
        buffer.push(records(0..3));
        buffer.push(records(3..6));

        assert_eq!(buffer.len(), 4);
        let (batch, dropped) = buffer.take(false).unwrap();
        assert_eq!(times(&batch), [2, 3]);
        assert_eq!(dropped, 2);

        let (_, dropped) = buffer.take(false).unwrap();
        assert_eq!(dropped, 0);
    }

    #[tokio::test]
    async fn signals_when_batch_is_full() {
        let buffer = Buffer::new(3, 10);

        buffer.push(records(0..2));
        let waited = timeout(Duration::from_millis(20), buffer.full()).await;
        assert!(waited.is_err());

        buffer.push(records(2..3));
        let waited = timeout(Duration::from_millis(20), buffer.full()).await;
        assert!(waited.is_ok());
    }
}
