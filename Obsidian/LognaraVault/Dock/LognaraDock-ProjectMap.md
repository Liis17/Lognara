# lognara-dock: карта проекта

Parent: [[Dock/LognaraDock]]

## Дерево

```text
Lognara-dock/
  Cargo.toml                  отдельный crate, Rust 2024, MSRV 1.94
  Dockerfile                  node (web) → rust (бинарник) → debian-slim с libssl3
  docker-compose.example.yml  все переменные, тома ./auth и ./data
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
    account.rs                /api/me (с series_admin), смена пароля, check_current_password
    passkey.rs                Webauthn, Challenges, вход и регистрация, список, удаление
    core_client.rs            клиент core: поиск, серии, определения, предпросмотр; таймауты 10 с и 35 с для серий
    series.rs                 прокси /api/series*: серии, запрос, определения, предпросмотр
    live.rs                   Live (poller + broadcast) на общем CoreClient, SSE stream
  tests/
    common/mod.rs             TestApp, запросы, htpasswd с cost 4, поддельный core с серийными маршрутами
    auth.rs                   вход, перечитывание htpasswd, TTL, Origin, блокировка
    password.rs               смена пароля и сохранение остальных строк
    passkey.rs                церемония с SoftPasskey, отказ challenge
    live.rs                   поддельный core: дедупликация, ошибки, остановка опроса
    series.rs                 прокси метрик, admin_disabled без токена, invalid и internal от core, 503 при недоступности, сессия и Origin
    static.rs                 SPA fallback и 404 для /api
  web/
    src/api.ts                fetch-обёртка, типы метрик и определений, тексты ошибок
    src/webauthn.ts           base64url ↔ ArrayBuffer для navigator.credentials
    src/App.tsx               проверка /api/me, маршруты /login, /logs, /metrics, /settings
    src/pages/Login.tsx       пароль и кнопка passkey
    src/pages/Logs.tsx        EventSource, 2000 строк, пауза, раскрытие события
    src/pages/Metrics.tsx     список метрик, диапазон и разбиение, график, кнопки редактора при series_admin
    src/pages/MetricEditor.tsx форма определения, предпросмотр правила, сохранение и удаление
    src/components/Chart.tsx  график uPlot: восемь категориальных цветов, остальные серые
    src/pages/Settings.tsx    смена пароля, список и добавление passkey
    src/index.css             стили страницы метрик и редактора
    package.json              React, react-router и uplot (график метрик)
```

## Связи модулей

- `api::router` собирает обработчики `auth`, `account`, `passkey`, `live`; слой `session::require_origin` применяется ко всем `/api/*`.
- `session::AuthUser` (extractor) использует `Sessions` и `Htpasswd::contains`.
- `account::change_password` и `passkey::register_start/delete` подтверждают действие через `account::check_current_password` → `auth::check_password` (общая блокировка логина).
- `passkey::login_finish` и `auth::login` создают сессию через `auth::start_session`.
- `live::Live` использует `CoreClient`; `live::stream` перепроверяет сессию через `Sessions::lookup_hash`.
- `series::*` (обработчики `/api/series*`) вызывают `CoreClient`; ошибки превращаются в `ApiError` через `From<CoreError>`. Все маршруты требуют `session::AuthUser`; запросы не GET/HEAD/OPTIONS проверяет `session::require_origin` (нужен `Origin` равный `LOGNARA_DOCK_PUBLIC_URL`).
- `account::me` отдаёт `series_admin = CoreClient::admin_enabled()`; фронтенд по нему включает кнопки редактора в `Metrics.tsx`.
- `Metrics.tsx` вызывает `api.ts` (`api`, `SeriesReply`, `Definition`), график строит `components/Chart.tsx`, редактор открывается из `Metrics.tsx` и сообщает о сохранении через `onDone`.
