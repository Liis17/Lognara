# lognara-dock: карта проекта

Parent: [[Dock/LognaraDock]]

## Дерево

```text
Lognara-dock/
  Cargo.toml                  отдельный crate, Rust 2024, MSRV 1.94
  Dockerfile                  node (web) → rust (бинарник) → debian-slim с libssl3
  docker-compose.example.yml  образ GHCR с LOGNARA_IMAGE_TAG, все переменные, тома ./auth и ./data
  nginx.example.conf          TLS и отдельный location для SSE
  src/
    main.rs                   tracing, конфиг, App::open, SIGTERM/SIGINT
    lib.rs                    run(): axum::serve + отмена App::shutdown
    config.rs                 LOGNARA_DOCK_* и проверки
    api.rs                    App/AppState, ApiError, router, health, SPA
    htpasswd.rs               разбор, кэш по stamp, verify, атомарная перезапись
    store.rs                  SQLite, миграции, lock каталога данных
    session.rs                Sessions, cookie, AuthUser, require_origin
    auth.rs                   login/logout, LoginLimiter, check_password
    access.rs                 /api/relays, /api/agents: сессия и proxy admin core
    account.rs                /api/me, смена пароля, check_current_password
    passkey.rs                Webauthn, Challenges, вход и регистрация, список, удаление
    core_client.rs            query история/Trace/аналитика и admin CRUD relay/агентов
    query.rs                  typed DTO и авторизованные proxy search/trace/histogram/group-by
    live.rs                   Live (poller + broadcast), SSE stream
  tests/
    common/mod.rs             TestApp, запросы, htpasswd с cost 4
    access.rs                 proxy admin API, сессия, Origin, no-store, ошибки core
    auth.rs                   вход, перечитывание htpasswd, TTL, Origin, блокировка
    password.rs               смена пароля и сохранение остальных строк
    passkey.rs                церемония с SoftPasskey, отказ challenge
    live.rs                   поддельный core: дедупликация, ошибки, остановка опроса
    query.rs                  query Bearer, DTO/cursor, Origin/сессия, no-store, ошибки, timeout
    static.rs                 SPA fallback и 404 для /api
  web/
    public/fonts/             локальные Commissioner / IBM Plex Mono, OFL, provenance
    src/index.css             общая тёмная палитра, формы, панели и адаптивные таблицы
    src/api.ts                fetch-обёртка и тексты ошибок
    src/webauthn.ts           base64url ↔ ArrayBuffer для navigator.credentials
    src/App.tsx               проверка /api/me, маршруты /login, /logs, /access, /settings
    src/pages/Login.tsx       пароль и кнопка passkey
    src/pages/Logs.tsx        EventSource, 2000 строк, пауза, раскрытие события
    src/pages/Access.tsx      профили relay/агентов, создание, одноразовый токен, удаление
    src/pages/Settings.tsx    смена пароля, список и добавление passkey
```

## Связи модулей

Публикация Dockerfile в GHCR из `master`, права каталогов данных и общие теги компонентов описаны в [[Repository/Images]].

- `api::router` собирает обработчики `auth`, `account`, `access`, `passkey`, `live`, `query`; слой `session::require_origin` применяется ко всем `/api/*`.
- `session::AuthUser` (extractor) использует `Sessions` и `Htpasswd::contains`.
- `account::change_password` и `passkey::register_start/delete` подтверждают действие через `account::check_current_password` → `auth::check_password` (общая блокировка логина).
- `passkey::login_finish` и `auth::login` создают сессию через `auth::start_session`.
- `live::Live` использует `CoreClient`; `live::stream` перепроверяет сессию через `Sessions::lookup_hash`.

- `access` → `App.core: CoreClient`: admin-токен из Config; Live использует query-клиент. Реестр хранится только в core.
