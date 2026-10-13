//! Ограниченный разбор строк; позиции всегда относятся к исходным байтам.
use std::io;

use regex::bytes::Regex;
use serde::{Deserialize, Serialize};

use crate::sources::{Format, Source, SourceKind};
use crate::wire::{self, Origin, Payload, Record};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Continuation {
    pub fragment: u64,
    pub line_continued: bool,
}

pub struct Framed {
    pub payload: Payload,
    pub offset: u64,
    pub end: u64,
    pub fragment: u64,
    pub continued: bool,
    pub continuation: Option<Continuation>,
    pub invalid_json: bool,
}

impl Framed {
    pub(crate) fn into_record(self, source: &Source, path: Option<&str>) -> Record {
        Record {
            received_at: wire::unix_nanos(),
            payload: self.payload,
            origin: Some(Origin {
                source: source.id.clone(),
                path: path.map(str::to_owned),
                stream: match source.kind {
                    SourceKind::Stdout => Some("stdout".into()),
                    SourceKind::Stderr => Some("stderr".into()),
                    SourceKind::File => None,
                },
                offset: self.offset,
                end: self.end,
                fragment: self.fragment,
                continued: self.continued,
            }),
        }
    }
}

pub struct Framer {
    data: Vec<u8>,
    line: Vec<u8>,
    offset: u64,
    line_start: usize,
    line_continued: bool,
    fragment: u64,
    format: Format,
    pattern: Option<Regex>,
    limit: usize,
}

impl Framer {
    pub fn new(
        source: &Source,
        limit: usize,
        offset: u64,
        continuation: Option<Continuation>,
    ) -> io::Result<Self> {
        if limit < 4 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source record limit must be >= 4",
            ));
        }
        Ok(Self {
            data: Vec::with_capacity(limit + 4),
            line: Vec::with_capacity(limit + 4),
            offset,
            line_start: 0,
            line_continued: continuation.as_ref().is_some_and(|c| c.line_continued),
            fragment: continuation.map_or(0, |c| c.fragment),
            format: source.format,
            pattern: source.multiline.as_ref().map(|m| m.compile()).transpose()?,
            limit,
        })
    }

    /// Вызывается с ограниченной порцией (сборщики используют не больше 4 KiB).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Framed> {
        let mut records = Vec::new();
        for &byte in bytes {
            self.line.push(byte);
            if byte == b'\n' {
                let line = std::mem::take(&mut self.line);
                self.accept(line, true, &mut records);
            } else if self.line.len() > self.limit + 3 {
                let count = utf8_cut(&self.line, self.limit);
                let tail = self.line.split_off(count);
                let piece = std::mem::replace(&mut self.line, tail);
                self.accept(piece, false, &mut records);
            }
        }
        records
    }

    fn accept(&mut self, line: Vec<u8>, complete: bool, records: &mut Vec<Framed>) {
        let header = !self.line_continued
            && self
                .pattern
                .as_ref()
                .is_some_and(|pattern| pattern.is_match(&line[..trimmed_len(&line)]));
        if header && (!self.data.is_empty() || self.fragment > 0) {
            records.push(self.take(self.data.len(), false, true));
        }
        if !self.line_continued {
            self.line_start = self.data.len();
        }
        self.data.extend_from_slice(&line);
        self.line_continued = !complete;
        if complete {
            self.line_start = self.data.len();
        }
        while self.data.len() > self.limit + 3 || (complete && trimmed_len(&self.data) > self.limit)
        {
            let count = utf8_cut(&self.data, self.limit);
            records.push(self.take(count, true, false));
        }
        if self.pattern.is_none() && complete {
            records.push(self.take(self.data.len(), false, true));
        }
        if !complete {
            while !self.data.is_empty() {
                let count = utf8_cut(&self.data, self.data.len().min(self.limit));
                records.push(self.take(count, true, false));
            }
        }
    }

    /// Idle завершает только полные физические строки многострочного события.
    pub fn idle(&mut self) -> Option<Framed> {
        if self.pattern.is_some() && self.line.is_empty() && self.line_start > 0 {
            Some(self.take(self.line_start, false, true))
        } else {
            None
        }
    }

    /// EOF потока/закрытого файла завершает также строку без перевода строки.
    pub fn finish_all(&mut self) -> Vec<Framed> {
        let mut records = Vec::new();
        if !self.line.is_empty() {
            let line = std::mem::take(&mut self.line);
            self.accept(line, true, &mut records);
        }
        if !self.data.is_empty() || self.fragment > 0 {
            records.push(self.take(self.data.len(), false, true));
        }
        records
    }

    pub fn pending_bytes(&self) -> usize {
        self.data.len() + self.line.len()
    }

    fn take(&mut self, count: usize, continued: bool, trim: bool) -> Framed {
        let tail = self.data.split_off(count);
        let mut bytes = std::mem::replace(&mut self.data, tail);
        let offset = self.offset;
        self.offset += count as u64;
        let fragment = self.fragment;
        let cut_line = count > 0 && bytes.last() != Some(&b'\n');
        self.line_start = self.line_start.saturating_sub(count);
        if continued {
            self.fragment += 1;
        } else {
            self.fragment = 0;
        }
        if trim && bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        let mut invalid_json = false;
        let payload = match String::from_utf8(bytes) {
            Ok(text) => {
                if self.format == Format::Ndjson && fragment == 0 && !continued {
                    if serde_json::from_str::<&serde_json::value::RawValue>(&text).is_ok() {
                        Payload::Json(text)
                    } else {
                        invalid_json = true;
                        Payload::Text(text)
                    }
                } else {
                    Payload::Text(text)
                }
            }
            Err(error) => Payload::Binary(error.into_bytes()),
        };
        Framed {
            payload,
            offset,
            end: self.offset,
            fragment,
            continued,
            invalid_json,
            continuation: continued.then_some(Continuation {
                fragment: self.fragment,
                line_continued: cut_line,
            }),
        }
    }
}

fn utf8_cut(bytes: &[u8], limit: usize) -> usize {
    match std::str::from_utf8(&bytes[..limit]) {
        Err(error)
            if error.error_len().is_none() && error.valid_up_to() >= limit.saturating_sub(3) =>
        {
            error.valid_up_to().max(1)
        }
        _ => limit,
    }
}

fn trimmed_len(bytes: &[u8]) -> usize {
    let mut length = bytes.len();
    if bytes.last() == Some(&b'\n') {
        length -= 1;
        if bytes.get(length.wrapping_sub(1)) == Some(&b'\r') {
            length -= 1;
        }
    }
    length
}
