//! Список зарегистрированных агентов: SHA-256 их токенов из core и дисковый кеш `.agents`.

use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use reqwest::redirect::Policy;
use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;
#[cfg(test)]
use sha2::{Digest, Sha256};
use tokio::time::{self, Instant, MissedTickBehavior};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub const SYNC_INTERVAL: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LIST_BYTES: usize = 8 << 20;
const CACHE_FILE: &str = ".agents";

pub(crate) type Hash = [u8; 32];

/// Токены агентов хранятся только хешами; неизвестный токен не принимается.
#[derive(Default)]
pub struct Agents {
    hashes: RwLock<HashSet<Hash>>,
}

impl Agents {
    #[cfg(test)]
    pub fn contains(&self, token: &str) -> bool {
        self.contains_hash(&Sha256::digest(token.as_bytes()).into())
    }

    pub fn contains_hash(&self, hash: &Hash) -> bool {
        self.hashes.read().unwrap().contains(hash)
    }

    fn replace(&self, hashes: HashSet<Hash>) {
        *self.hashes.write().unwrap() = hashes;
    }

    #[cfg(test)]
    pub fn with_tokens(tokens: &[&str]) -> Arc<Self> {
        let agents = Arc::new(Self::default());
        agents.replace(
            tokens
                .iter()
                .map(|token| Sha256::digest(token.as_bytes()).into())
                .collect(),
        );
        agents
    }
}

#[derive(Deserialize)]
struct AgentList {
    agents: Vec<String>,
}

/// Любая невалидная запись делает невалидным весь список: частичный список опаснее старого.
fn parse_list(bytes: &[u8]) -> Option<HashSet<Hash>> {
    let list: AgentList = serde_json::from_slice(bytes).ok()?;
    list.agents.iter().map(|hash| parse_hash(hash)).collect()
}

fn parse_hash(text: &str) -> Option<Hash> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let (pairs, _) = bytes.as_chunks::<2>();
    let mut hash = [0; 32];
    for (byte, pair) in hash.iter_mut().zip(pairs) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        *byte = (high * 16 + low) as u8;
    }
    Some(hash)
}

/// Подтягивает список агентов у core и поддерживает кеш в каталоге spool.
pub struct AgentSync {
    client: Client,
    url: Url,
    token: String,
    dir: PathBuf,
    agents: Arc<Agents>,
    /// `Some` только пока файл `.agents` совпадает со списком в памяти.
    persisted: Option<HashSet<Hash>>,
}

impl AgentSync {
    pub fn new(core_url: &Url, core_token: &str, dir: &Path, agents: Arc<Agents>) -> Self {
        Self {
            client: Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .redirect(Policy::none())
                .build()
                .expect("HTTP client builds"),
            // Core URL проверен конфигурацией как http(s), он всегда может быть базой.
            url: core_url.join("agents").expect("core URL is a base URL"),
            token: core_token.to_owned(),
            dir: dir.to_owned(),
            agents,
            persisted: None,
        }
    }

    /// Восстанавливает список из кеша; отсутствующий или повреждённый кеш не ошибка.
    pub async fn load_cache(&mut self) {
        let path = self.dir.join(CACHE_FILE);
        match tokio::task::spawn_blocking(move || read_cache(&path)).await {
            Ok(Ok(bytes)) => match parse_list(&bytes) {
                Some(hashes) => {
                    info!(agents = hashes.len(), "agent list restored from cache");
                    self.agents.replace(hashes.clone());
                    self.persisted = Some(hashes);
                }
                None => warn!("agent cache is invalid and was ignored"),
            },
            Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(Err(error)) => warn!(%error, "agent cache is unreadable"),
            Err(error) => warn!(%error, "agent cache read task failed"),
        }
    }

    /// Любая ошибка оставляет прежний список; кеш перезаписывается только при изменении.
    pub async fn sync(&mut self) {
        let (body, hashes) = match self.fetch().await {
            Ok(list) => list,
            Err(reason) => {
                warn!(%reason, "agent list sync failed; previous list kept");
                return;
            }
        };
        self.agents.replace(hashes.clone());
        if self.persisted.as_ref() == Some(&hashes) {
            return;
        }
        info!(agents = hashes.len(), "agent list updated");
        let dir = self.dir.clone();
        match tokio::task::spawn_blocking(move || {
            lognara_spool::atomic_file(&dir, CACHE_FILE, &body)
        })
        .await
        {
            Ok(Ok(())) => self.persisted = Some(hashes),
            Ok(Err(error)) => warn!(%error, "agent cache write failed"),
            Err(error) => warn!(%error, "agent cache write task failed"),
        }
    }

    /// Синхронизирует каждые `interval`, пока не отменён `stop`.
    pub async fn run(mut self, interval: Duration, stop: CancellationToken) {
        let mut ticker = time::interval_at(Instant::now() + interval, interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = ticker.tick() => {}
            }
            tokio::select! {
                _ = stop.cancelled() => return,
                _ = self.sync() => {}
            }
        }
    }

    async fn fetch(&self) -> Result<(Vec<u8>, HashSet<Hash>), String> {
        let mut response = self
            .client
            .get(self.url.clone())
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|error| format!("core unavailable: {}", error.without_url()))?;
        if response.status() != StatusCode::OK {
            return Err(format!("core returned {}", response.status()));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("core response failed: {}", error.without_url()))?
        {
            if chunk.len() > MAX_LIST_BYTES - body.len() {
                return Err("agent list is too large".into());
            }
            body.extend_from_slice(&chunk);
        }
        let hashes = parse_list(&body).ok_or_else(|| "agent list is invalid".to_owned())?;
        Ok((body, hashes))
    }
}

fn read_cache(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_LIST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_LIST_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "agent cache is too large",
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use axum::Router;
    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode as Status};
    use axum::routing::get;
    use tokio::net::TcpListener;

    fn hex(token: &str) -> String {
        Sha256::digest(token.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    type Reply = Arc<Mutex<(Status, String)>>;

    async fn core(tokens: &[&str]) -> (Url, Reply) {
        let reply: Reply = Arc::new(Mutex::new((Status::OK, list_body(tokens))));
        async fn handler(State(reply): State<Reply>, headers: HeaderMap) -> (Status, String) {
            assert_eq!(headers["authorization"], "Bearer relay-core-token");
            reply.lock().unwrap().clone()
        }
        let app = Router::new()
            .route("/v1/agents", get(handler))
            .with_state(reply.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1/batches", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url.parse().unwrap(), reply)
    }

    fn list_body(tokens: &[&str]) -> String {
        let hashes: Vec<_> = tokens.iter().map(|token| hex(token)).collect();
        serde_json::json!({ "agents": hashes }).to_string()
    }

    fn sync_for(url: &Url, dir: &Path, agents: &Arc<Agents>) -> AgentSync {
        AgentSync::new(url, "relay-core-token", dir, agents.clone())
    }

    #[test]
    fn parses_only_complete_lowercase_or_uppercase_sha256_lists() {
        let valid = hex("a");
        assert!(parse_list(format!("{{\"agents\":[\"{valid}\"]}}").as_bytes()).is_some());
        let upper = valid.to_uppercase();
        assert_eq!(
            parse_list(format!("{{\"agents\":[\"{upper}\"]}}").as_bytes()),
            parse_list(format!("{{\"agents\":[\"{valid}\"]}}").as_bytes())
        );
        assert_eq!(parse_list(br#"{"agents":[]}"#), Some(HashSet::new()));
        for bad in [
            r#"{"agents":["short"]}"#.to_owned(),
            format!("{{\"agents\":[\"{valid}\",\"{}zz\"]}}", &valid[..62]),
            r#"{"agents":[1]}"#.to_owned(),
            r#"{"relays":[]}"#.to_owned(),
            "not json".to_owned(),
        ] {
            assert!(parse_list(bad.as_bytes()).is_none(), "{bad}");
        }
    }

    #[tokio::test]
    async fn sync_replaces_list_and_keeps_previous_one_on_any_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (url, reply) = core(&["first"]).await;
        let agents = Arc::new(Agents::default());
        let mut sync = sync_for(&url, dir.path(), &agents);
        sync.sync().await;
        assert!(agents.contains("first"));
        assert!(!agents.contains("second"));

        *reply.lock().unwrap() = (Status::OK, list_body(&["second"]));
        sync.sync().await;
        assert!(!agents.contains("first"), "revoked token must be rejected");
        assert!(agents.contains("second"));

        for broken in [
            (Status::UNAUTHORIZED, list_body(&["third"])),
            (Status::INTERNAL_SERVER_ERROR, String::new()),
            (Status::OK, "not json".to_owned()),
            (Status::OK, r#"{"agents":["bad"]}"#.to_owned()),
        ] {
            *reply.lock().unwrap() = broken;
            sync.sync().await;
            assert!(agents.contains("second"));
            assert!(!agents.contains("third"));
        }
        let cached = std::fs::read(dir.path().join(CACHE_FILE)).unwrap();
        assert_eq!(
            parse_list(&cached),
            parse_list(list_body(&["second"]).as_bytes())
        );
    }

    #[tokio::test]
    async fn cache_is_written_in_spool_dir_only_when_the_list_changes() {
        let dir = tempfile::tempdir().unwrap();
        let spool = crate::spool::Spool::open(dir.path(), 1 << 20)
            .await
            .unwrap();
        let (url, reply) = core(&["first"]).await;
        let agents = Arc::new(Agents::default());
        let mut sync = sync_for(&url, spool.dir(), &agents);
        sync.sync().await;
        let cache = spool.dir().join(CACHE_FILE);
        assert!(cache.exists());
        // Подмена содержимого выдаст перезапись; тот же список в другом порядке не должен её вызвать.
        std::fs::write(&cache, b"marker").unwrap();
        let (first, second) = (hex("a"), hex("b"));
        *reply.lock().unwrap() = (
            Status::OK,
            format!("{{\"agents\":[\"{first}\",\"{second}\"]}}"),
        );
        sync.sync().await;
        assert_ne!(std::fs::read(&cache).unwrap(), b"marker");
        std::fs::write(&cache, b"marker").unwrap();
        *reply.lock().unwrap() = (
            Status::OK,
            format!("{{\"agents\":[\"{second}\",\"{first}\"]}}"),
        );
        sync.sync().await;
        assert_eq!(std::fs::read(&cache).unwrap(), b"marker");
        assert!(agents.contains("a") && agents.contains("b"));
    }

    #[tokio::test]
    async fn restart_uses_cache_while_core_is_down_and_core_refreshes_it() {
        let dir = tempfile::tempdir().unwrap();
        let (url, reply) = core(&["cached"]).await;
        sync_for(&url, dir.path(), &Arc::default()).sync().await;

        *reply.lock().unwrap() = (Status::SERVICE_UNAVAILABLE, String::new());
        let agents = Arc::new(Agents::default());
        let mut restarted = sync_for(&url, dir.path(), &agents);
        restarted.load_cache().await;
        restarted.sync().await;
        assert!(agents.contains("cached"));
        assert!(!agents.contains("unknown"));

        *reply.lock().unwrap() = (Status::OK, list_body(&["fresh"]));
        restarted.sync().await;
        assert!(agents.contains("fresh") && !agents.contains("cached"));
    }

    #[tokio::test]
    async fn failed_cache_write_does_not_prevent_restoring_the_persisted_list() {
        let dir = tempfile::tempdir().unwrap();
        let (url, reply) = core(&["first"]).await;
        let agents = Arc::new(Agents::default());
        let mut sync = sync_for(&url, dir.path(), &agents);
        sync.sync().await;
        // atomic_file cannot create its temp file over a directory.
        let temp = dir.path().join(".agents.tmp");
        std::fs::create_dir(&temp).unwrap();
        *reply.lock().unwrap() = (Status::OK, list_body(&["second"]));
        sync.sync().await;
        assert!(agents.contains("second") && !agents.contains("first"));
        std::fs::remove_dir(temp).unwrap();
        // Core returns the list already on disk; memory must still be replaced.
        *reply.lock().unwrap() = (Status::OK, list_body(&["first"]));
        sync.sync().await;
        assert!(agents.contains("first"));
        assert!(
            !agents.contains("second"),
            "revoked token must not survive cache failure"
        );
    }

    #[tokio::test]
    async fn missing_or_corrupt_cache_and_unreachable_core_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let unreachable: Url = "http://127.0.0.1:1/v1/batches".parse().unwrap();
        let agents = Arc::new(Agents::default());
        let mut sync = sync_for(&unreachable, dir.path(), &agents);
        sync.load_cache().await;
        sync.sync().await;
        assert!(!agents.contains("anyone"));
        std::fs::write(dir.path().join(CACHE_FILE), b"{\"agents\":[\"bad\"]}").unwrap();
        sync.load_cache().await;
        assert!(!agents.contains("anyone"));
    }

    #[tokio::test]
    async fn background_loop_applies_changes_and_stops_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let (url, reply) = core(&[]).await;
        let agents = Arc::new(Agents::default());
        let stop = CancellationToken::new();
        let task = tokio::spawn(
            sync_for(&url, dir.path(), &agents).run(Duration::from_millis(10), stop.clone()),
        );
        let wait_for = |expected: bool| {
            let agents = agents.clone();
            async move {
                tokio::time::timeout(Duration::from_secs(5), async {
                    while agents.contains("late") != expected {
                        time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .expect("agent list was not applied");
            }
        };
        *reply.lock().unwrap() = (Status::OK, list_body(&["late"]));
        wait_for(true).await;
        *reply.lock().unwrap() = (Status::OK, list_body(&[]));
        wait_for(false).await;
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }
}
