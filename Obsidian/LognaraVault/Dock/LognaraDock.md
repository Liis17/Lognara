# lognara-dock

Parent: [[Index]]

## Назначение

Веб-интерфейс Lognara за nginx (TLS). Rust-сервис в `Lognara-dock/` отдаёт React-приложение из `web/`, проверяет вход и показывает логи [[Core/LognaraCore]] в реальном времени. Query-токен core хранится только в dock.

Поток: браузер → nginx :443 → dock :7403 → `POST /v1/logs/search` core с Bearer query-токеном.

Файлы и связи модулей описаны в [[Dock/LognaraDock-ProjectMap]].

## Вход и данные

- Пользователей задаёт htpasswd (только bcrypt), как у docker registry. Файл перечитывается при изменении (mtime, размер, inode). Удалённый пользователь сразу теряет сессии и passkey-вход. Некорректный файл запрещает вход и снимает readiness.
- Смена пароля атомарно заменяет строку пользователя: временный файл в той же папке, fsync, rename, fsync каталога. Поэтому с хоста монтируется папка `auth/`, а не файл (rename поверх bind-mount файла даёт EBUSY).
- SQLite `dock.sqlite3` в `LOGNARA_DOCK_DATA_DIR`: `sessions` (SHA-256 токена), `users` (стабильный WebAuthn user handle), `passkeys` (сериализованный `Passkey`). Миграции через `user_version`, каталог блокируется `dock.lock`.
- Cookie `lognara_dock_session`: `HttpOnly; Secure; SameSite=Strict`. Изменяющие запросы требуют `Origin` равный `LOGNARA_DOCK_PUBLIC_URL`. После 5 неверных паролей логин блокируется на 5 минут.
- Passkey через webauthn-rs (feature `conditional-ui`): вход без логина по user handle. Регистрация запрашивает `residentKey: required`, добавление и удаление ключа требуют текущий пароль. Состояние церемоний хранится в памяти 5 минут под cookie `lognara_dock_challenge`.

## Логи в реальном времени

Общий poller работает, пока есть подписчики SSE. Раз в `LOGNARA_DOCK_LIVE_POLL_MS` он ищет в окне `[now - LOOKBACK, now + 60s)` с `limit=1000` и рассылает только новые id через `tokio::broadcast`. Первый опрос заполняет ленту 200 последними событиями. Ограничение: при более чем 1000 новых событий за опрос лента показывает самые свежие; для полноты нужен tail-endpoint core по порядку публикации.

SSE-поток каждые 60 секунд перепроверяет сессию и закрывается при остановке процесса (`App::shutdown`), иначе graceful shutdown ждал бы его вечно.

## Параметры запуска

| Переменная | Обяз. | По умолчанию | Назначение |
|---|---|---|---|
| `LOGNARA_DOCK_PUBLIC_URL` | да | - | внешний адрес; origin для passkey и проверки Origin; http только для localhost |
| `LOGNARA_DOCK_CORE_URL` | да | - | базовый адрес core |
| `LOGNARA_DOCK_CORE_TOKEN` | да | - | query-токен core, скрыт в Debug |
| `LOGNARA_DOCK_LISTEN_ADDR` | нет | `127.0.0.1:7403` | в образе `0.0.0.0:7403` |
| `LOGNARA_DOCK_HTPASSWD_FILE` | нет | `/etc/lognara-dock/auth/htpasswd` | файл пользователей |
| `LOGNARA_DOCK_DATA_DIR` | нет | `/var/lib/lognara-dock` | SQLite и lock |
| `LOGNARA_DOCK_RP_ID` | нет | хост PUBLIC_URL | RP ID passkey |
| `LOGNARA_DOCK_SESSION_TTL_SECONDS` | нет | `43200` | абсолютный срок сессии |
| `LOGNARA_DOCK_SESSION_IDLE_SECONDS` | нет | `3600` | срок бездействия |
| `LOGNARA_DOCK_BCRYPT_COST` | нет | `12` | 10..=14 |
| `LOGNARA_DOCK_PASSWORD_MIN_LENGTH` | нет | `12` | минимум символов нового пароля |
| `LOGNARA_DOCK_LIVE_POLL_MS` | нет | `1000` | частота опроса core |
| `LOGNARA_DOCK_LIVE_LOOKBACK_SECONDS` | нет | `300` | окно для поздних событий |
| `LOGNARA_DOCK_WEB_DIR` | нет | `/usr/share/lognara-dock/web` | собранный фронтенд |

Пример Compose, nginx и Dockerfile лежат в `Lognara-dock/`; подробности в `Lognara-dock/README.md`.

## Ключевые методы/функции

- `Config::from_lookup(lookup: impl Fn(&str) -> Option<String>): Result<Config, ConfigError>` — проверяет `LOGNARA_DOCK_*`, вычисляет origin и RP ID
- `App::open(config: Config): Result<AppState, Box<dyn Error>>` — читает htpasswd, открывает SQLite, строит Webauthn и Live
- `api::router(app: AppState): Router` — `/api/*`, health, SPA-fallback и защитные заголовки
- `run(app: AppState, listener: TcpListener, shutdown: CancellationToken): io::Result<()>` — HTTP до отмены, затем закрывает SSE
- `Htpasswd::users(&self): Option<Arc<HashMap<String, String>>>` — актуальные пользователи, `None` при некорректном файле
- `Htpasswd::verify(&self, username: &str, password: &str): bool` — bcrypt, для неизвестного логина сравнение с фиктивным хэшем
- `Htpasswd::set_password(&self, username: &str, password: &str, cost: u32): Result<(), UpdateError>` — атомарная замена строки
- `Store::call(&self, f: FnOnce(&mut Connection)): Result<T, StoreError>` — запрос SQLite в blocking-потоке
- `Sessions::create(&self, username: &str): Result<String, StoreError>` — новая сессия, чистит истёкшие
- `Sessions::lookup_hash(&self, hash: TokenHash): Result<Option<(String, TokenHash)>, StoreError>` — проверка TTL/idle и продление
- `Sessions::revoke_others(&self, username: &str, keep: TokenHash): Result<(), StoreError>` — завершить другие сессии
- `auth::check_password(app: &AppState, username: &str, password: String): Result<(), ApiError>` — пароль с учётом блокировки
- `passkey::login_finish(...)` — вход по discoverable passkey и обновление счётчика
- `passkey::register_start(...)` — options регистрации с resident key после проверки пароля
- `Live::subscribe(self: &Arc<Self>): (Vec<Value>, Option<String>, Receiver<Message>)` — снимок и подписка, запускает опрос
- `CoreClient::search(&self, from, to, limit: usize): Result<Vec<Value>, CoreError>` — поиск core
- `live::stream(State, AuthUser): Sse<...>` — SSE `logs`, `core_error`, `core_ok`, `lagged`

## Зависимости

- Использует: [[Core/LognaraCore]] (`POST /v1/logs/search`).
- Используется в: браузер через nginx.
