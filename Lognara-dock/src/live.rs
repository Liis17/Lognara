//! Логи в реальном времени: общий опрос core и рассылка подписчикам через SSE.
//!
//! Core ищет только по времени события, поэтому каждый опрос смотрит окно
//! `[now - lookback, now + 60s)` и отбрасывает уже показанные id. Так в ленту попадают и
//! события, которые relay доставил с опозданием. Если в окне больше `BATCH_LIMIT` новых
//! событий за опрос, лента покажет самые свежие из них.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::broadcast;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::api::AppState;
use crate::config::Config;
use crate::core_client::CoreClient;
use crate::session::AuthUser;

/// Максимум `limit` в поиске core.
const BATCH_LIMIT: usize = 1000;
/// Столько последних событий получает новый подписчик.
const RECENT: usize = 200;
/// Окно поиска заходит вперёд: часы приложений могут спешить.
const FUTURE_SKEW: Duration = Duration::from_secs(60);
const CHANNEL: usize = 64;
const KEEPALIVE: Duration = Duration::from_secs(15);
/// Как часто поток проверяет, что сессия ещё действует.
const SESSION_RECHECK: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub enum Message {
    /// Новые события по возрастанию времени.
    Events(Arc<Vec<Value>>),
    CoreError(String),
    CoreOk,
}

pub struct Live {
    core: Arc<CoreClient>,
    poll: Duration,
    lookback: Duration,
    tx: broadcast::Sender<Message>,
    state: Mutex<LiveState>,
    shutdown: CancellationToken,
}

#[derive(Default)]
struct LiveState {
    running: bool,
    /// Первый опрос заполняет ленту последними событиями, не считая всё окно новым.
    primed: bool,
    /// id из окна и момент, когда их увидели.
    seen: HashMap<String, Instant>,
    /// Последние события по возрастанию времени.
    recent: VecDeque<Value>,
    core_error: Option<String>,
}

impl Live {
    pub fn new(config: &Config, core: Arc<CoreClient>, shutdown: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            core,
            poll: config.live_poll,
            lookback: config.live_lookback,
            tx: broadcast::channel(CHANNEL).0,
            state: Mutex::new(LiveState::default()),
            shutdown,
        })
    }

    /// Подписывает клиента и запускает опрос, если он не идёт.
    /// Возвращает уже накопленные события и текущую ошибку core.
    pub fn subscribe(
        self: &Arc<Self>,
    ) -> (Vec<Value>, Option<String>, broadcast::Receiver<Message>) {
        let mut state = self.state.lock().unwrap();
        let rx = self.tx.subscribe();
        if !state.running {
            state.running = true;
            tokio::spawn(self.clone().run());
        }
        (
            state.recent.iter().cloned().collect(),
            state.core_error.clone(),
            rx,
        )
    }

    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap().running
    }

    async fn run(self: Arc<Self>) {
        tracing::debug!("live polling started");
        let mut interval = tokio::time::interval(self.poll);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = self.shutdown.cancelled() => break,
            }
            if self.stop_if_unused() {
                tracing::debug!("live polling stopped");
                return;
            }
            self.poll_once().await;
        }
        self.state.lock().unwrap().running = false;
    }

    /// Без подписчиков опрос останавливается и забывает состояние.
    fn stop_if_unused(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if self.tx.receiver_count() > 0 {
            return false;
        }
        *state = LiveState::default();
        true
    }

    async fn poll_once(&self) {
        let now = OffsetDateTime::now_utc();
        let result = self
            .core
            .search(now - self.lookback, now + FUTURE_SKEW, BATCH_LIMIT)
            .await;
        let mut state = self.state.lock().unwrap();
        let events = match result {
            Ok(events) => events,
            Err(err) => {
                let message = err.to_string();
                if state.core_error.as_ref() != Some(&message) {
                    tracing::warn!("live polling failed: {message}");
                    state.core_error = Some(message.clone());
                    let _ = self.tx.send(Message::CoreError(message));
                }
                return;
            }
        };
        if state.core_error.take().is_some() {
            let _ = self.tx.send(Message::CoreOk);
        }

        // Событие с меткой T выходит из окна не позже чем через lookback + skew после показа.
        let forget = self.lookback + FUTURE_SKEW + self.poll;
        state.seen.retain(|_, seen| seen.elapsed() < forget);
        let now = Instant::now();
        let mut fresh: Vec<Value> = events
            .into_iter()
            .filter(|event| match event.get("id").and_then(Value::as_str) {
                Some(id) => state.seen.insert(id.to_owned(), now).is_none(),
                None => false,
            })
            .collect();
        if !state.primed {
            state.primed = true;
            fresh.truncate(RECENT);
        }
        if fresh.is_empty() {
            return;
        }
        fresh.reverse();
        state.recent.extend(fresh.iter().cloned());
        let excess = state.recent.len().saturating_sub(RECENT);
        state.recent.drain(..excess);
        let _ = self.tx.send(Message::Events(Arc::new(fresh)));
    }
}

/// `GET /api/logs/stream`: SSE с событиями `logs`, `core_error`, `core_ok` и `lagged`.
pub async fn stream(
    State(app): State<AppState>,
    user: AuthUser,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (recent, core_error, rx) = app.live.subscribe();
    let mut initial = vec![logs_event(&recent)];
    if let Some(message) = core_error {
        initial.push(core_error_event(&message));
    }
    initial.reverse();

    let mut recheck = tokio::time::interval(SESSION_RECHECK);
    recheck.reset();
    let stream = futures_util::stream::unfold(
        (initial, rx, recheck, app, user),
        |(mut initial, mut rx, mut recheck, app, user)| async move {
            if let Some(event) = initial.pop() {
                return Some((Ok(event), (initial, rx, recheck, app, user)));
            }
            loop {
                let event = tokio::select! {
                    message = rx.recv() => match message {
                        Ok(Message::Events(events)) => logs_event(&events),
                        Ok(Message::CoreError(message)) => core_error_event(&message),
                        Ok(Message::CoreOk) => Event::default().event("core_ok").data("{}"),
                        Err(broadcast::error::RecvError::Lagged(skipped)) => Event::default()
                            .event("lagged")
                            .data(json!({ "skipped_batches": skipped }).to_string()),
                        Err(broadcast::error::RecvError::Closed) => return None,
                    },
                    _ = recheck.tick() => {
                        if session_alive(&app, &user).await {
                            continue;
                        }
                        return None;
                    }
                    _ = app.shutdown.cancelled() => return None,
                };
                return Some((Ok(event), (initial, rx, recheck, app, user)));
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::new().interval(KEEPALIVE))
}

/// Поток закрывается, если сессию отозвали или пользователя убрали из htpasswd.
async fn session_alive(app: &AppState, user: &AuthUser) -> bool {
    matches!(app.sessions.lookup_hash(user.token_hash).await, Ok(Some(_)))
        && app.htpasswd.contains(&user.username)
}

fn logs_event(events: &[Value]) -> Event {
    Event::default()
        .event("logs")
        .data(serde_json::to_string(events).expect("events serialize"))
}

fn core_error_event(message: &str) -> Event {
    Event::default()
        .event("core_error")
        .data(json!({ "message": message }).to_string())
}
