# lognara-dock: карта проекта

Parent: [[Dock/LognaraDock]]

## Дерево

```text
Lognara-dock/
  Cargo.toml                  отдельный crate, Rust 2024, MSRV 1.94
  Dockerfile                  node (web) → rust (бинарник) → debian-slim с libssl3
  docker-compose.example.yml  образ registry с LOGNARA_IMAGE_TAG, все переменные, тома ./auth и ./data
  nginx.example.conf          TLS и отдельный location для SSE
  src/
    main.rs                   tracing, конфиг, App::open, SIGTERM/SIGINT
    lib.rs                    run(): axum::serve, наблюдатель проблем, отмена App::shutdown
    config.rs                 LOGNARA_DOCK_* и проверки
    api.rs                    App/AppState, ApiError, router, health, SPA
    htpasswd.rs               разбор, кэш по stamp, verify, атомарная перезапись
    store.rs                  SQLite, миграции, lock каталога данных, скрытые паттерны, статусы проблем
    patterns.rs               DTO паттернов, proxy search/details и персональное скрытие
    issues.rs                 DTO проблем, proxy search/details, resolve/reopen, наблюдатель возвратов
    session.rs                Sessions, cookie, AuthUser, require_origin
    auth.rs                   login/logout, LoginLimiter, check_password
    access.rs                 /api/relays, /api/agents: сессия и proxy admin core
    account.rs                /api/me, смена пароля, check_current_password
    passkey.rs                Webauthn, Challenges, вход и регистрация, список, удаление
    infra.rs                  авторизованная сводка core/dock/узлов, свежесть и пороги
    query.rs                  typed DTO и авторизованные proxy search/trace/histogram/group-by/summary
    core_client.rs            клиент core: история/Trace/аналитика/паттерны/ошибки/status, серии/определения, admin CRUD; таймаут запросов 35 с
    series.rs                 прокси /api/series*: серии, запрос, определения, предпросмотр
    live.rs                   Live (poller + broadcast) на общем CoreClient, SSE stream
  tests/
    common/mod.rs             TestApp, запросы, htpasswd с cost 4, поддельный core с серийными маршрутами и поддельный admin API core
    access.rs                 proxy admin API, сессия, Origin, no-store, ошибки core
    auth.rs                   вход, перечитывание htpasswd, TTL, Origin, блокировка
    password.rs               смена пароля и сохранение остальных строк
    passkey.rs                церемония с SoftPasskey, отказ challenge
    live.rs                   поддельный core: дедупликация, ошибки, остановка опроса
    query.rs                  query Bearer, DTO/cursor, Origin/сессия, no-store, ошибки, timeout
    patterns.rs               скрытие по пользователю, лимит 1000, exclude_patterns, ошибки паттернов
    issues.rs                 общий статус, история, Origin, проверка ответов core, наблюдатель
    infra.rs                  session/no-store, частичные ошибки, TTL/idle/spool и relay_silent
    series.rs                 proxy метрик, ошибки core, сессия, Origin и таймаут 35 с
    static.rs                 SPA fallback и 404 для /api
  web/
    public/fonts/             локальные Commissioner / IBM Plex Mono, OFL, provenance
    src/index.css             общая тёмная палитра, формы, панели и адаптивные таблицы
    src/api.ts                fetch-обёртка, AbortSignal и тексты ошибок
    src/types.ts              wire-типы событий, запросов и агрегатов
    src/logs.ts               ограниченный query parser, Live merge, контекст и CSV
    src/logs.test.ts          query/history/live/CSV unit-тесты Node
    src/components/Histogram.tsx count SVG, сравнение, доступные числа и drilldown
    src/components/Patterns.tsx  режим паттернов: таблица, sparkline, детали, скрытие, переход к событиям
    src/webauthn.ts           base64url ↔ ArrayBuffer для navigator.credentials
    src/App.tsx               проверка /api/me, маршруты /login, /logs, /errors, /analytics, /infrastructure, /metrics, /access, /settings
    src/pages/Login.tsx       пароль и кнопка passkey
    src/pages/Logs.tsx        история/cursor/Trace, URL, отдельный Live, детали, экспорт
    src/pages/Errors.tsx      проблемы: вкладки статусов, stack trace, история, решение и переходы
    src/pages/Analytics.tsx   единая сводка, stacked levels, p95/coverage/actions, сравнение и переход в логи
    src/pages/Infrastructure.tsx цепочка, узлы и диск, последовательный polling и stale-снимок
    src/pages/Access.tsx      профили relay/агентов, создание, одноразовый токен, удаление
    src/pages/Metrics.tsx     список метрик, диапазон, разбиение и окружение, график
    src/pages/MetricEditor.tsx определение метрики, предпросмотр regex, сохранение и удаление
    src/components/Chart.tsx  uPlot с цветами осей из общей тёмной палитры
    src/pages/Settings.tsx    смена пароля, список и добавление passkey
    playwright.config.ts     Chromium, Vite webServer и сценарии tests/dock.spec.ts и tests/errors.spec.ts
    tests/metrics.spec.ts     окружения/разбиение, regex preview/save, навигация и mobile
    tests/infrastructure.spec.ts состояния/переходы, polling/error recovery, mobile и overflow
    tests/dock.spec.ts        вход, аккаунт, поиск, cursor, Trace/ссылка, аналитика, Live, mobile
```

## Связи модулей

Публикация Dockerfile в `docker.barkfluff.com:443` из `master`, права каталогов данных и общие теги компонентов описаны в [[Repository/Images]].

- `api::router` собирает обработчики `auth`, `account`, `access`, `passkey`, `live`, `query`; слой `session::require_origin` применяется ко всем `/api/*`.
- `session::AuthUser` (extractor) использует `Sessions` и `Htpasswd::contains`.
- `account::change_password` и `passkey::register_start/delete` подтверждают действие через `account::check_current_password` → `auth::check_password` (общая блокировка логина).
- `passkey::login_finish` и `auth::login` создают сессию через `auth::start_session`.
- `live::Live` использует `CoreClient`; `live::stream` перепроверяет сессию через `Sessions::lookup_hash`.
- `series::*` (обработчики `/api/series*`) и `access::*` вызывают общий `App.core: Arc<CoreClient>`; ошибки превращаются в `ApiError` через единый `From<CoreError>` в `api.rs`. Все маршруты требуют `session::AuthUser`; запросы не GET/HEAD/OPTIONS проверяет `session::require_origin` (нужен `Origin` равный `LOGNARA_DOCK_PUBLIC_URL`).
- `Metrics.tsx` вызывает `api.ts` (`api`, `SeriesReply`, `Definition`), график строит `components/Chart.tsx`, редактор открывается из `Metrics.tsx` и сообщает о сохранении через `onDone`.
- Admin-токен берётся из Config; Live использует тот же клиент с query-токеном. Реестр relay/агентов хранится только в core.
