//! Настройки источников одной программы; читаются до запуска приложения.
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use regex::bytes::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

pub const MAX_SOURCES: usize = 32;
pub const MAX_FILES: usize = 256;
pub const MAX_OPEN_FILES: usize = 64;
pub const MAX_CONFIG_BYTES: usize = 256 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    File,
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    #[default]
    Text,
    Ndjson,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StartAt {
    #[default]
    Beginning,
    End,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Multiline {
    pub start_pattern: String,
    #[serde(default = "default_timeout")]
    pub flush_timeout_ms: u64,
}
fn default_timeout() -> u64 {
    1000
}

impl Multiline {
    pub(crate) fn compile(&self) -> io::Result<Regex> {
        RegexBuilder::new(&self.start_pattern)
            .unicode(false)
            .size_limit(64 << 10)
            .dfa_size_limit(64 << 10)
            .build()
            .map_err(io::Error::other)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: SourceKind,
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub start_at: StartAt,
    pub multiline: Option<Multiline>,
}

impl Source {
    pub fn stream(kind: SourceKind) -> Self {
        Self {
            id: match kind {
                SourceKind::Stdout => "stdout",
                _ => "stderr",
            }
            .into(),
            kind,
            format: Format::Text,
            include: Vec::new(),
            exclude: Vec::new(),
            start_at: StartAt::Beginning,
            multiline: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Sources {
    pub sources: Vec<Source>,
}

impl Sources {
    pub fn load(path: Option<&Path>, capture: bool) -> io::Result<Self> {
        let mut bytes = Vec::new();
        if let Some(path) = path {
            File::open(path)?
                .take(MAX_CONFIG_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_CONFIG_BYTES {
                return Err(invalid("sources file exceeds 256 KiB"));
            }
        } else {
            bytes.extend_from_slice(b"{\"sources\":[]}");
        }
        Self::parse(&bytes, capture)
    }

    pub fn parse(bytes: &[u8], capture: bool) -> io::Result<Self> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(invalid("sources file exceeds 256 KiB"));
        }
        let mut config: Self = serde_json::from_slice(bytes).map_err(invalid)?;
        if capture {
            for kind in [SourceKind::Stdout, SourceKind::Stderr] {
                if config.stream(kind).is_none() {
                    config.sources.push(Source::stream(kind));
                }
            }
        }
        config.validate(capture)?;
        Ok(config)
    }

    pub fn stream(&self, kind: SourceKind) -> Option<&Source> {
        self.sources.iter().find(|source| source.kind == kind)
    }

    pub fn validate(&self, capture: bool) -> io::Result<()> {
        if self.sources.len() > MAX_SOURCES {
            return Err(invalid("at most 32 sources are supported"));
        }
        let mut ids = HashSet::new();
        let mut streams = HashSet::new();
        for source in &self.sources {
            if source.id.is_empty()
                || source.id.len() > 64
                || !source
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                || !ids.insert(&source.id)
            {
                return Err(invalid(
                    "source id must be unique, 1..64 ASCII letters/digits/-_.",
                ));
            }
            if source.kind == SourceKind::File {
                if source.include.is_empty() {
                    return Err(invalid("file source requires include"));
                }
                if source.include.len() + source.exclude.len() > 64 {
                    return Err(invalid("at most 64 patterns per source"));
                }
                for pattern in source.include.iter().chain(&source.exclude) {
                    validate_pattern(pattern)?;
                }
            } else if !capture
                || !source.include.is_empty()
                || !source.exclude.is_empty()
                || source.start_at != StartAt::Beginning
                || !streams.insert(source.kind as u8)
            {
                return Err(invalid(
                    "stdout/stderr require run, one definition per stream, and no file options",
                ));
            }
            if let Some(multiline) = &source.multiline {
                if source.format != Format::Text
                    || multiline.flush_timeout_ms == 0
                    || multiline.start_pattern.len() > 4096
                {
                    return Err(invalid(
                        "multiline requires text, a positive timeout and pattern <= 4096 bytes",
                    ));
                }
                multiline.compile()?;
            }
        }
        Ok(())
    }

    /// Все дополнительные буферы, regex и реестр ограничены этим резервом.
    pub fn reserved_bytes(&self, max_record: usize) -> usize {
        if self.sources.is_empty() {
            512 << 10
        } else {
            (6 << 20) + 3 * max_record
        }
    }
}

pub(crate) fn validate_pattern(pattern: &str) -> io::Result<PathBuf> {
    let path = Path::new(pattern);
    if pattern.len() > 4096
        || !path.is_absolute()
        || pattern.contains('\0')
        || pattern.contains("**")
    {
        return Err(invalid(
            "file patterns must be absolute, <= 4096 bytes and non-recursive",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("file pattern requires a directory"))?;
    if parent.to_string_lossy().contains(['*', '?', '[', ']']) || path.file_name().is_none() {
        return Err(invalid("wildcards are allowed only in the filename"));
    }
    glob::Pattern::new(pattern).map_err(invalid)?;
    Ok(parent.to_path_buf())
}

fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}
