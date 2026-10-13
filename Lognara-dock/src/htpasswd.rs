//! Файл пользователей в формате htpasswd, как у docker registry: `user:bcrypt-hash`.
//!
//! Файл перечитывается при изменении, поэтому правки с хоста применяются без рестарта.
//! Смена пароля атомарно переписывает только строку пользователя.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::SystemTime;

use bcrypt::Version;
use rand::RngCore;

/// Хэш для неизвестного логина: время ответа не выдаёт, есть ли такой пользователь.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| bcrypt::hash("lognara-dock-dummy", 12).expect("bcrypt dummy hash"));

#[derive(Debug, thiserror::Error)]
pub enum HtpasswdError {
    #[error("failed to read {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path}:{line}: {reason}")]
    Format {
        path: PathBuf,
        line: usize,
        reason: &'static str,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("user is not in htpasswd")]
    UnknownUser,
    #[error(transparent)]
    Htpasswd(#[from] HtpasswdError),
    #[error("failed to write htpasswd: {0}")]
    Io(#[from] io::Error),
}

type Users = Arc<HashMap<String, String>>;

pub struct Htpasswd {
    path: PathBuf,
    cache: Mutex<Cache>,
    /// Последовательность изменений файла внутри процесса.
    write: Mutex<()>,
}

struct Cache {
    stamp: Option<Stamp>,
    /// `None`, если последнее чтение не удалось: вход запрещён до исправления файла.
    users: Option<Users>,
}

#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
    inode: u64,
}

impl Htpasswd {
    /// Читает файл; ошибка формата или чтения останавливает запуск.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, HtpasswdError> {
        let path = path.into();
        let (stamp, users) = load(&path)?;
        LazyLock::force(&DUMMY_HASH);
        Ok(Self {
            path,
            cache: Mutex::new(Cache {
                stamp: Some(stamp),
                users: Some(Arc::new(users)),
            }),
            write: Mutex::new(()),
        })
    }

    /// Актуальные пользователи. Если файл стал некорректным, возвращает `None`
    /// и пишет ошибку в лог один раз на изменение файла.
    pub fn users(&self) -> Option<Users> {
        let mut cache = self.cache.lock().unwrap();
        let current = stamp(&self.path).ok();
        if current.is_none() || current != cache.stamp {
            cache.stamp = current;
            cache.users = match load(&self.path) {
                Ok((_, users)) => Some(Arc::new(users)),
                Err(err) => {
                    tracing::error!("htpasswd rejected, logins disabled: {err}");
                    None
                }
            };
        }
        cache.users.clone()
    }

    pub fn contains(&self, username: &str) -> bool {
        self.users()
            .is_some_and(|users| users.contains_key(username))
    }

    /// Проверяет пароль. Блокирующая операция: вызывайте из `spawn_blocking`.
    pub fn verify(&self, username: &str, password: &str) -> bool {
        let users = self.users();
        let hash = users.as_ref().and_then(|users| users.get(username));
        let matches = bcrypt::verify(password, hash.unwrap_or(&DUMMY_HASH)).unwrap_or(false);
        matches && hash.is_some()
    }

    /// Атомарно заменяет хэш пользователя, сохраняя остальные строки байт в байт.
    /// Блокирующая операция: вызывайте из `spawn_blocking`.
    pub fn set_password(
        &self,
        username: &str,
        password: &str,
        cost: u32,
    ) -> Result<(), UpdateError> {
        let hash = bcrypt::non_truncating_hash_with_result(password, cost)
            .map_err(|err| io::Error::other(err.to_string()))?
            .format_for_version(Version::TwoY);
        let _write = self.write.lock().unwrap();
        let text = fs::read_to_string(&self.path).map_err(|source| HtpasswdError::Io {
            path: self.path.clone(),
            source,
        })?;
        parse(&self.path, &text)?;

        let mut replaced = false;
        let mut output = String::with_capacity(text.len() + hash.len());
        for line in text.split_inclusive('\n') {
            let (content, ending) = split_ending(line);
            match content.split_once(':') {
                Some((user, _)) if user == username => {
                    output.push_str(username);
                    output.push(':');
                    output.push_str(&hash);
                    output.push_str(ending);
                    replaced = true;
                }
                _ => output.push_str(line),
            }
        }
        if !replaced {
            return Err(UpdateError::UnknownUser);
        }
        replace_file(&self.path, output.as_bytes())?;
        Ok(())
    }
}

fn split_ending(line: &str) -> (&str, &str) {
    let content = line.trim_end_matches(['\r', '\n']);
    (content, &line[content.len()..])
}

fn stamp(path: &Path) -> io::Result<Stamp> {
    let meta = fs::metadata(path)?;
    Ok(Stamp {
        modified: meta.modified()?,
        len: meta.len(),
        inode: meta.ino(),
    })
}

fn load(path: &Path) -> Result<(Stamp, HashMap<String, String>), HtpasswdError> {
    let io_error = |source| HtpasswdError::Io {
        path: path.to_owned(),
        source,
    };
    let stamp = stamp(path).map_err(io_error)?;
    let text = fs::read_to_string(path).map_err(io_error)?;
    Ok((stamp, parse(path, &text)?))
}

/// Разбирает строки `user:hash`. Пустые строки и `#`-комментарии пропускаются.
/// Ошибка называет строку, но не содержит хэш.
pub fn parse(path: &Path, text: &str) -> Result<HashMap<String, String>, HtpasswdError> {
    let mut users = HashMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let error = |reason| HtpasswdError::Format {
            path: path.to_owned(),
            line: index + 1,
            reason,
        };
        let Some((user, hash)) = line.split_once(':') else {
            return Err(error("expected user:hash"));
        };
        if user.is_empty() || user.trim() != user {
            return Err(error("invalid user name"));
        }
        if !["$2a$", "$2b$", "$2y$"].iter().any(|p| hash.starts_with(p)) {
            return Err(error(
                "unsupported hash, only bcrypt (htpasswd -B) is allowed",
            ));
        }
        if users.insert(user.to_owned(), hash.to_owned()).is_some() {
            return Err(error("duplicate user"));
        }
    }
    Ok(users)
}

/// Пишет временный файл рядом с `path`, синхронизирует и переименовывает поверх.
/// Каталог должен быть смонтирован целиком: rename поверх bind-mount файла даёт EBUSY.
fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{:016x}.tmp", rand::rng().next_u64()));
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.set_permissions(fs::metadata(path)?.permissions())?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        File::open(dir)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_bcrypt_lines_without_hash_in_message() {
        let path = Path::new("htpasswd");
        let err = parse(path, "ok:$2y$04$abc\nmd5:$apr1$secrethash\n").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("htpasswd:2"), "{message}");
        assert!(!message.contains("secrethash"));
        assert!(parse(path, "nocolon\n").is_err());
        assert!(parse(path, ":$2y$04$abc\n").is_err());
        assert!(parse(path, "a:$2y$04$x\na:$2y$04$y\n").is_err());
    }

    #[test]
    fn skips_blank_lines_and_comments() {
        let users = parse(Path::new("h"), "# admins\n\na:$2y$04$x\r\nb:$2b$04$y").unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users["a"], "$2y$04$x");
    }
}
