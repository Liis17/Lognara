# lognara-dock: карта проекта

Parent: [[Dock/LognaraDock]]

## Дерево

```text
Lognara-dock/
  Cargo.toml                  отдельный crate, Rust 2024, MSRV 1.94
  Dockerfile                  node (web) → rust (бинарник) → debian-slim с libssl3
  docker-compose.example.yml  образ registry с LOGNARA_IMAGE_TAG, все переменные, тома ./auth и ./data
  nginx.example.conf          TLS, SSE и Streamable HTTP MCP/OAuth locations
  src/
    main.rs                   tracing, конфиг, App::open, SIGTERM/SIGINT
    lib.rs                    run(): axum::serve, наблюдатель проблем, отмена App::shutdown
    config.rs                 LOGNARA_DOCK_* и проверки
    api.rs                    App/AppState, ApiError, router, health, SPA
    mcp.rs                    stateless Streamable HTTP `/mcp`, tools по правам токена, скоуп узла, алерты, Bearer auth, cursors и limits
    token_access.rs           права (логи, метрики, запись алертов) и скоуп узла MCP-токена
    oauth.rs                  OAuth metadata, restricted ChatGPT CIMD, PKCE, token/refresh endpoints
    mcp_access.rs             API страницы MCP-доступа, PAT и OAuth grants/consent
    alerts.rs                 алерты метрик: общие настройки, правила, состояние и durable-доставка Telegram, `alerts::watch`
    log_alerts.rs             алерты на логи: правила запроса страницы Logs, проверка по watermark `sequence`
    version.rs                release_version: LOGNARA_RELEASE_VERSION или версия с коротким commit
    htpasswd.rs               разбор, кэш по stamp, verify, атомарная перезапись
    store.rs                  SQLite, миграции, PAT/OAuth hashes and grants, сессии, passkey, паттерны, проблемы, избранное
    patterns.rs               DTO паттернов, proxy search/details и персональное скрытие
    issues.rs                 DTO проблем, proxy search/details, resolve/reopen, наблюдатель возвратов
    session.rs                Sessions, cookie, AuthUser, require_origin
    auth.rs                   login/logout, LoginLimiter, check_password
    access.rs                 /api/relays, /api/agents: сессия и proxy admin core
    account.rs                /api/me, смена пароля, check_current_password
    passkey.rs                Webauthn, Challenges, вход и регистрация, список, удаление
    infra.rs                  авторизованная сводка core/dock/узлов, relay_id, свежесть и пороги
    query.rs                  typed DTO и авторизованные proxy search/trace/histogram/group-by/summary
    core_client.rs            клиент core: история/Trace/аналитика/паттерны/ошибки/status, серии/определения, admin CRUD; таймаут запросов 35 с
    series.rs                 /api/series*: прокси каталога/lookup/графиков и редактирования, API общего избранного
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
    infra.rs                  session/no-store, частичные ошибки, TTL/idle/spool, relay_id и relay_silent
    series.rs                 proxy метрик, ошибки core, сессия, Origin и таймаут 35 с
    mcp_scope.rs              права и скоуп MCP: tools/list, forbidden, скоуп в запросах core, курсоры, алерты, OAuth consent
    mcp_protocol.rs           stateless `/mcp`: initialize, tools/list и вызов get_versions только с Bearer-токеном
    static.rs                 SPA fallback и 404 для /api
  web/
    public/fonts/             локальные Commissioner / IBM Plex Mono, OFL, provenance
    src/index.css             общая тёмная палитра, формы, панели и адаптивные таблицы
    src/api.ts                fetch-обёртка, AbortSignal и тексты ошибок
    src/types.ts              wire-типы событий, запросов и агрегатов
    src/logs.ts               ограниченный query parser, Live merge, контекст и CSV
    src/logs.test.ts          query/history/live/CSV unit-тесты Node
    src/components/Histogram.tsx count SVG, сравнение, доступные числа и drilldown (аналитика)
    src/components/EventsChart.tsx график событий/Trace: выделение диапазона, масштаб, уровни
    src/components/PeriodChips.tsx границы периода, часовой пояс и момент снимка
    src/components/Patterns.tsx  режим паттернов: таблица, sparkline, детали, скрытие, переход к событиям
    src/webauthn.ts           base64url ↔ ArrayBuffer для navigator.credentials
    src/App.tsx               проверка /api/me, верхняя панель и lazy routes включая дерево инфраструктуры, `/alerts`, `/hosts`, OAuth consent; `/mcp-access` перенаправляет в `/settings/mcp`, `/log-alerts` в `/alerts`
    src/pages/Login.tsx       пароль, показ пароля, кнопка passkey и анимированный фон
    src/pages/Logs.tsx        история/cursor/Trace, URL, автодогрузка, отдельный Live, детали, экспорт
    src/pages/Errors.tsx      проблемы: вкладки статусов, stack trace, история, решение и переходы
    src/pages/Analytics.tsx   единая сводка, stacked levels, p95/coverage/actions, сравнение и переход в логи
    src/pages/Infrastructure.tsx цепочка, узлы и диск, кнопка перехода к дереву
    src/pages/InfrastructureTree.tsx lazy-страница дерева, выбор пути, sticky-подсказка и измерение карточек
    src/infraTree.ts            чистая группировка по ID, геометрия и правила выбора дерева
    src/infraPresentation.ts    общие подписи статусов, времени, размеров и групп
    src/infrastructureVersion.ts подпись версии узла: release_version или версия пакета с коротким commit
    src/useInfrastructure.ts    общий опрос сводки, отмена, 401 и stale-снимок
    src/pages/Access.tsx      профили relay/агентов, создание, одноразовый токен, удаление
    src/pages/McpAccess.tsx   вкладка `/settings/mcp`: MCP URL, одноразовый PAT с правами и скоупом, срок/last-use и отзыв PAT/grants
    src/components/AccessFields.tsx выбор прав и скоупа узла для PAT и OAuth consent
    src/components/ServiceCombobox.tsx поиск сервиса с подсказками (Errors, Analytics, MetricCreate, AccessFields); логика в serviceComboboxLogic.ts
    src/components/DateRangePicker.tsx календарь диапазона в часовом поясе отображения (Logs, Hosts); логика в dateRange.ts
    src/components/LineChart.tsx многосерийный график с hover, легендой и выбором диапазона (Analytics, MetricDetail, Hosts, EventsChart); вычисления в lineChartUtils.ts
    src/components/HostIcon.tsx иконка хоста для Access и дерева инфраструктуры
    src/pages/Alerts.tsx      `/alerts`: вкладки «Правила» (метрики и логи), «Получатели и группы», «Telegram»
    src/pages/MetricAlertEditor.tsx редактор правила метрики; MetricAlertSettings.tsx получатели и Telegram-настройки, встроены в Alerts
    src/pages/Hosts.tsx       `/hosts?host=...`: выбор хоста, ряды hwagent, LineChart и DateRangePicker
    src/pages/OAuthConsent.tsx OAuth client/scope/resource и подтверждение пользователя
    src/pages/Metrics.tsx     плитки по серверам/сервисам и очередь миниграфиков
    src/pages/MetricDetail.tsx отдельная страница метрики с серверной областью
    src/pages/MetricCreate.tsx модалка создания, импорт JSON, конфликты и nginx
    src/metricBundles.ts      переносимые наборы, примеры и шаблоны nginx
    src/infraTree.test.ts     группировка, orphan, геометрия и связанные пути
    src/dateRange.test.ts, serviceCombobox.test.ts, lineChart.test.ts, infrastructureVersion.test.ts, metricBundles.test.ts  чистая логика соответствующих модулей
    src/pages/MetricEditor.tsx определение метрики, предпросмотр regex, сохранение и удаление
    src/components/Chart.tsx  uPlot с цветами осей из общей тёмной палитры
    src/pages/Settings.tsx    `/settings/<tab>`: вкладки account (пароль), passkeys и mcp (встраивает McpAccess)
    playwright.config.ts     Chromium, Vite webServer, testDir `tests/` (все сценарии)
    tests/metrics.spec.ts     окружения/разбиение, regex preview/save, навигация и mobile
    tests/infrastructure.spec.ts сводка и дерево: навигация, выбор, polling, ошибка, mobile и 500 узлов
    tests/mcp-access.spec.ts выпуск/копирование PAT с правами и скоупом, отсутствие localStorage, expiry/revoke, OAuth consent и возврат после login
    tests/dock.spec.ts        вход, аккаунт, поиск, cursor, Trace (пустой ID — подсказка, неверный или без событий — отдельные состояния), аналитика, Live, mobile
    tests/access.spec.ts      профили relay/агентов и одноразовые токены
    tests/alerts.spec.ts      `/alerts`: правила, получатели и Telegram
    tests/log-alerts.spec.ts  алерты на логи и редирект `/log-alerts`
    tests/hosts.spec.ts       `/hosts`: список хостов и выбор
    tests/host-metrics.spec.ts графики хоста из hwagent
```

## Связи модулей

Публикация Dockerfile в `docker.barkfluff.com` из `master`, права каталогов данных и общие теги компонентов описаны в [[Repository/Images]].

- `api::router` собирает обработчики `auth`, `account`, `access`, `passkey`, `live`, `query`; слой `session::require_origin` применяется ко всем `/api/*`.
- `session::AuthUser` (extractor) использует `Sessions` и `Htpasswd::contains`.
- `account::change_password` и `passkey::register_start/delete` подтверждают действие через `account::check_current_password` → `auth::check_password` (общая блокировка логина).
- `passkey::login_finish` и `auth::login` создают сессию через `auth::start_session`.
- `live::Live` использует `CoreClient`; `live::stream` перепроверяет сессию через `Sessions::lookup_hash`.
- `series::*` (кроме локального избранного через `App.store`) и `access::*` вызывают общий `App.core: Arc<CoreClient>`; ошибки превращаются в `ApiError` через единый `From<CoreError>` в `api.rs`. Все маршруты требуют `session::AuthUser`; запросы не GET/HEAD/OPTIONS проверяет `session::require_origin` (нужен `Origin` равный `LOGNARA_DOCK_PUBLIC_URL`).
- `Metrics.tsx` вызывает `api.ts` (`api`, `SeriesReply`, `Definition`), график строит `components/Chart.tsx`, редактор открывается из `MetricDetail.tsx` и `MetricCreate.tsx` и сообщает о сохранении через `onDone`.
- Admin-токен берётся из Config; Live использует тот же клиент с query-токеном. Реестр relay/агентов хранится только в core.
