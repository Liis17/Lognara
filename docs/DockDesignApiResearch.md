# Dock: инвентаризация API core и dock для интеграции шести дизайнов

Дата проверки: 2026-10-14. Текущее состояние после реализации этапов 0–3B плана интеграции; серверные контракты проверены по `Lognara-dock/src/query.rs`, `core_client.rs` и исходникам core. Проверка API основана на чтении кода. Проверки реализации описаны в [README dock](../Lognara-dock/README.md).

Dock проксирует поиск, trace, histogram, group-by и summary с полным envelope и query timeout 35 секунд. Live и admin используют timeout 10 секунд. Эталоны сохранены в `docs/design-references/dock/` с контрольными суммами; оригиналы находятся в `/Users/liis/Projects/Lognara/docs/страницы дизайна/`.

## Источники

Obsidian: `Obsidian/LognaraVault/Index.md`, `Core/LognaraCore.md`, `Core/LognaraCore-ProjectMap.md`, `Dock/LognaraDock.md`, `Dock/LognaraDock-ProjectMap.md`, `Dock/DesignIntegration.md`.

Core: `Lognara-core/src/api.rs`, `query.rs`, `analytics.rs`, `columns.rs`, `model.rs`, `wire.rs`, `index.rs`, `access.rs`, `catalog.rs`, `metrics.rs`, `journal.rs`, `storage.rs`, `config.rs`.

Dock: `Lognara-dock/src/api.rs`, `query.rs`, `core_client.rs`, `live.rs`, `access.rs`, `account.rs`, `session.rs`, `store.rs`, `config.rs`; клиент `Lognara-dock/web/src/api.ts`, `pages/Logs.tsx`, `pages/Analytics.tsx`, `pages/Access.tsx`.

Relay и agent: `Lognara-relay/src/ingest.rs`, `agents.rs`; `Lognara-agent/src/ingest.rs`. Проверено отсутствие HTTP-эндпоинтов здоровья и метрик у relay и agent.

## Краткий вывод

Core уже даёт исторический поиск с курсором, trace, одну серию гистограммы и группировку по счётчику. Summary core даёт точный total, текстовые агрегаты, stacked histogram, приблизительный p95/coverage по duration_ms и action-группы в одном снимке. Произвольных числовых агрегатов, атрибутов как измерений, паттернов, проблем, узлов с heartbeat и правил алертов нет. Dock проксирует уже существующие core-эндпоинты и управляет учётными данными relay и agent. События, корреляция Trace и базовая count-аналитика уже используют эти API. Паттерны требуют дальнейшего расширения core; text-aware summary и p95 уже реализованы (см. [контракт](DockSummaryContract.md)). Экраны «Ошибки», «Инфраструктура» и «Алерты» требуют новых серверных моделей. В Dock SQLite сейчас есть таблицы только для сессий, пользователей и passkeys (`Lognara-dock/src/store.rs:14-30`); таблиц правил, паттернов, проблем и узлов нет нигде.

## Матрица возможностей

Статусы: **есть** — доступно через текущий API; **частично** — есть, но без нужной семантики; **нет** — требуется новый backend.

| Функция | Статус | Что есть сейчас | Что требуется | Источник |
|---|---|---|---|---|
| Поиск событий по точным полям | есть | `POST /api/logs/search` → core `POST /v1/logs/search` | — | `api.rs:131`, `query.rs:177`, `core_client.rs:120` |
| Текстовый поиск по `message` | частично | токены AND или последовательная фраза; одно слово — обычный токен, не подстрока | подстрока/prefix, если нужна |`query.rs:191-203`, `query.rs:337-338` |
| Строка запроса `service:x AND level:(...)` | частично | в клиенте `logs.ts` разбирается в `filters` и `text` | серверный разбор не нужен, если оставить клиентский | `Lognara-dock/web/src/logs.ts` |
| Фильтры по `attributes` | нет | поле хранится JSON-строкой, не индексируется и не фильтруется | новый индекс или извлечённые колонки | `columns.rs:60,92,177`, `index.rs:18-29` |
| Числовые фильтры и сравнения | нет | только `in`-списки строк | диапазоны и NOT, если нужны | `query.rs:98-111`, `analytics.rs:292` |
| Пагинация «дальше» по курсору | есть | `next_cursor`, `limit` 1..1000, default 100 | — | `query.rs:188`, `query.rs:248-262` |
| Переход назад / номер страницы | нет | курсор только вперёд; стек курсоров на клиенте | — | `query.rs:259-262` |
| `total` у поиска | есть через summary | summary.total и summary.page используют один Snapshot; старый search envelope не менялся | — | `Lognara-core/src/summary.rs` |
| `total` через legacy агрегаты | есть | сумма `buckets[].count` или сумма `group-by level` | text-фильтр поддерживает summary | `analytics.rs:26-37`, `analytics.rs:133-203` |
| Порядок выдачи | частично | `timestamp, sequence` DESC; trace ASC; произвольной сортировки нет | сортировка по другим полям, если нужна | `query.rs:181-185` |
| Live-лента | частично | polling окна `[now-300s, now+60s)`, до 1000 самых свежих за опрос, без фильтров | потоковый tail с фильтрами и полнотой | `live.rs:28-34`, `live.rs:129-133`, `config.rs:184-188` |
| Trace | частично | логи trace по возрастанию, без модели spans | spans с длительностями | `api.rs:148-166`, `Dock query.rs:186` |
| Гистограмма объёма | есть | summary: count и семь level-серий; legacy histogram: одна серия, до 10 000 корзин | серии по service, если нужны | `analytics.rs:71-131` |
| Группировка (top-k по count) | частично | 1–2 поля из `environment, server, backend, service, service_instance, action, level`; limit 1..1000 | distinct, другие поля, сумма/средние | `analytics.rs:133-203` |
| Facets (значения с счётчиками) | частично | `group-by` с limit; нет `other`, `has_more`, prefix, distinct-total | своя семантика исключения собственного фильтра | `analytics.rs:135-158`, `analytics.rs:193` |
| `stats` (sum/avg/min/max/p95) | частично | summary: p95 t-digest по duration_ms, measured/coverage, по action; другие агрегаты отсутствуют | sum/avg/min/max, если нужны | `Lognara-core/src/summary.rs` |
| Distinct trace/request/span | нет | count — число строк событий | distinct-агрегаты | `analytics.rs:133-203` |
| Произвольный SQL | нет | DataFusion внутри core, наружу не выставлен | не требуется шаблонами | `storage.rs:64-68` |
| Группировка ошибок (fingerprint) | нет | `fingerprint` в core — хеш параметров курсора, не сигнатура ошибки | сигнатура, нормализация, история | `query.rs:204-205` |
| Паттерны сообщений | нет | только сырые `message` и `attributes` | шаблонизация, версии, first/last seen, доли, рост | `model.rs:17-31` |
| Проблемы (жизненный цикл) | нет | — | сущность, статусы, история, связь с паттерном | — |
| Stack trace | частично | если приложение положило его в `attributes`, dock отдаёт JSON | извлечение и поиск | `Dock query.rs:106-125` |
| Реестр relay и agent | есть | list/create/delete через dock → core admin | — | `Dock access.rs:1-117`, `core_client.rs:207-237` |
| Выдача токенов relay/agent | есть | токен один раз в ответе, `Cache-Control: no-store`, dock не хранит | повторный показ и ротация нет | `core access.rs:108,145`, `Dock access.rs:110-117` |
| Heartbeat, последнее событие, версия, спул узла | нет | ни в core, ни в relay, ни в agent | протокол heartbeat и хранение состояния | см. раздел «Инфраструктура» |
| Счётчики потерь relay/agent | частично | только Prometheus-счётчики процесса core | per-source история | `journal.rs:140-162`, `metrics.rs:27-36` |
| Размеры Parquet/индексов/WAL, сегменты | частично | WAL — gauge в `/metrics`; сегменты только внутри catalog | JSON-эндпоинт storage | `metrics.rs:12-14`, `catalog.rs:62` |
| Правила алертов, история, каналы, доставка | нет | — | CRUD, evaluator, состояния, секреты каналов | см. раздел «Алерты» |
| Предпросмотр условия алерта | частично | histogram/group-by по count за окно | evaluator на сервере | `analytics.rs:71-131` |
| Авторизация пользователей | есть | сессии в cookie, passkeys, Origin-проверка небезопасных методов | — | `session.rs:156-196`, `api.rs:151-154` |
| Выдача токенов core пользователям | нет | query и admin токены только в env dock | — | `Lognara-dock/src/config.rs:131-137` |

## Summary (этап 3B)

`POST /v1/stats/summary` и dock `/api/stats/summary`: filters/text с общим PreparedText поиска, interval_seconds и необязательный search_limit. Единый Snapshot даёт total, stacked buckets/levels, top-5 ERROR services, duration p95 t-digest/measured/coverage, top-20 action groups и первую страницу DESC. Курсор страницы фиксирует тот же watermark. Источник duration — существующий JSON, только конечные неотрицательные числа; строка не измерена. Сканирование потоковое, агрегаты последовательно в общем memory pool; summary занимает один analytics slot. Полный контракт и проверки: [DockSummaryContract](DockSummaryContract.md). Старые histogram/group-by сохраняют ограничения, описанные ниже.

## Core: инвентаризация

Маршруты (`Lognara-core/src/api.rs:28-51`):

- `POST /v1/logs/search`, `GET /v1/traces/{trace_id}`, `POST /v1/stats/histogram`, `POST /v1/stats/group-by`, `POST /v1/stats/summary`: query-токен (`api.rs:29-38`).
- `GET|POST /v1/admin/relays`, `DELETE /v1/admin/relays/{id}`, `GET|POST /v1/admin/agents`, `DELETE /v1/admin/agents/{id}`: admin-токен (`api.rs:39-48`).
- `POST /v1/batches` (relay, по токену relay), `GET /v1/agents` (relay получает хеши агентов) (`api.rs:105-112`, `api.rs:474`).
- `GET /metrics` с query-токеном, text Prometheus (`api.rs:113-118`, `api.rs:333-339`).
- `GET /health/live`, `GET /health/ready` без токена (`api.rs:122-124`, `api.rs:325-331`).

Конкурентность и таймауты: `search_slots` = 4, `analytics_slots` = 2 (`storage.rs:87-88`). Занятость возвращает 503 (`api.rs:82`, `api.rs:172-173`). Таймаут запроса по умолчанию 30 секунд (`config.rs:73`). Пул DataFusion: 1 GiB (`LOGNARA_QUERY_MEMORY_BYTES`, `storage.rs:64-68`); память страницы поиска 128 MiB (`LOGNARA_SEARCH_MEMORY_BYTES`). Каждая аналитика берёт свой снимок `core.snapshot()` (`analytics.rs:89`, `analytics.rs:158`), поэтому два ответа могут иметь разный `as_of`.

Ошибки запросов (`api.rs:206-215` и `query.rs:64-75`): 400 invalid, 410 cursor expired или снимок недоступен, 503 busy или storage, 504 timeout.

Модель события (`model.rs:17-31`, `wire.rs:41-57`): `id`, `timestamp`, `ingested_at`, `environment`, `server`, `backend`, `service`, `service_instance`, `action`, `level`, `message`, `trace_id`, `span_id`, `parent_span_id`, `request_id`, `attributes` (объект JSON). Значения `trace_id` 32 hex, `span_id` и `parent_span_id` 16 hex (`query.rs:137-143`). Уровни `trace, debug, info, warn, error, fatal, unknown` (`wire.rs:59-70`).

Схема хранения (`columns.rs:44-60`, `index.rs:18-29`): `attributes` хранится как JSON-строка, но в индексе отсутствует, поэтому фильтр и группировка по атрибутам невозможны. Summary читает duration_ms из существующего JSON и считает p95; материализация и миграция raw-схемы не требуются.

Реестр (`access.rs:38-56`, `access.rs:160-189`): relay — `id, name, created_at`; agent — `id, name, relay_id, created_at`. Полей здоровья, версии, времени последнего контакта нет. Хеш токена хранится в SQLite (`access.rs:73-75`), токен возвращается один раз (`access.rs:108-118`, `access.rs:145-156`). Префиксы: `lnr_` для relay, `lna_` для agent. Ограничения: имя до 64 символов без управляющих (`access.rs:233-239`); удалить relay с агентами нельзя (`access.rs:191-208`, ошибка `relay_has_agents`).

Retention по умолчанию 7 суток (`config.rs:70`: `LOGNARA_RETENTION_SECONDS`, 7 × 86400). Шаблон `infra.dc.html` показывает пример «retention 30 дн»; интерфейс должен показывать фактическую настройку. Для 30-дневного baseline алертов нужна отдельная история или изменение retention.

## Core: поиск, фильтры, курсор

Запрос (`query.rs:26-35`): `from`, `to` (RFC3339), `filters` (поля → массивы), `text` (`query`, `mode` = `all` или `phrase`), `limit` (1..1000, по умолчанию 100), `cursor`. Неизвестные поля отклоняются (`deny_unknown_fields`). Правила (`query.rs:85-130`):

- `from < to`.
- Поля фильтра только из `index::EXACT_FIELDS` плюс `level` (`query.rs:98`; список в `index.rs:18-29`): `id, environment, server, backend, service, service_instance, action, trace_id, span_id, parent_span_id, request_id`.
- Значений от 1 до 64 на поле, каждое до 4096 байт (`query.rs:101-111`).
- `level` проверяется по строгому совпадению `as_str()` в нижнем регистре (`query.rs:120-121`, `query.rs:145-157`). `ERROR` даёт 400. Клиент обязан отправлять `error`.
- Текст до 8192 байт и от 1 до 64 токенов (`query.rs:191-199`). Токены берутся из `message` по `index::tokens`. Режим `phrase` применяется только при 2 и более токенах (`query.rs:337-338`).
- Поиск идёт по `timestamp` события в `[from, to)`, не по `ingested_at` (`query.rs:298-306`).

Ответ (`query.rs:57-62`): `{events, next_cursor, as_of}`. `total` отсутствует. Core запрашивает `limit + 1` записей, чтобы понять наличие следующей страницы (`query.rs:243-249`).

Курсор (`query.rs:159-171`, `query.rs:209-262`) подписан HMAC-SHA256 ключом процесса `core.cursor_key` (`query.rs:540-560`); неверная подпись или строка длиннее 4096 байт дают 410 (`query.rs:548-560`). Внутри курсор содержит `instance`, `generation`, `watermark`, хеш запроса (фильтры, текст и направление; `limit` в хеш не входит), последнюю пару `(timestamp, sequence)`, `issued_at` и `as_of`. Срок жизни 10 минут от первой страницы (`query.rs:219`). Смена параметров запроса даёт 400 (`query.rs:224-225`). Рестарт или смена `generation` даёт 410 (`query.rs:215-223`).

Сортировка: поиск DESC по `(timestamp, sequence)`, trace ASC (`query.rs:181-185`, `api.rs:148-166`).

Видимость: снимок публикуется по `LOGNARA_REFRESH_MS` (default 1000 мс, `config.rs:69`), записи видны по `sequence <= watermark` (`analytics.rs:271`, `query.rs:298-310`).

## Core: гистограмма и группировка

Гистограмма (`analytics.rs:26-37`, `analytics.rs:71-131`): `from`, `to`, `filters`, `interval_seconds` (default 60, 1..86400). Корзины выравниваются по эпохе UTC (`date_bin` от `timestamp(0)`, `analytics.rs:91-98`). Первая корзина может начинаться раньше `from`, но учитываются только события `[from, to)`. Пустые корзины заполняются нулями. Лимит 10 000 корзин (`analytics.rs:84`). Ответ `{buckets: [{timestamp, count}], as_of}`. Текст в запросе не допускается (`deny_unknown_fields`).

Группировка (`analytics.rs:40-49`, `analytics.rs:133-203`): `group_by` 1–2 различных поля из `environment, server, backend, service, service_instance, action, level` (`analytics.rs:135-152`); `limit` 1..1000 (`analytics.rs:155`), default 100 (`analytics.rs:44`). Порядок: `count` DESC, затем значения групп по возрастанию (`analytics.rs:160-165`). Пустые значения приходят как `null` (`analytics.rs:177-206`). `level` возвращается строкой через `LogLevel::from_code` (`analytics.rs:193-195`). Группы не содержат `has_more`, `other_count` и distinct-total.

Ответы аналитики используют `count` строк событий, а не число вызовов или запросов (`analytics.rs:133-203`).

## Dock: инвентаризация

Маршруты (`Lognara-dock/src/api.rs:119-150`), все под `/api`:

- Вход и аккаунт: `POST /auth/login`, `POST /auth/logout`, `POST /auth/passkey/start|finish`, `GET /me`, `POST /account/password`, `GET /account/passkeys`, `POST /account/passkeys/start|finish`, `DELETE /account/passkeys/{id}`.
- Логи: `GET /logs/stream` (SSE), `POST /logs/search`, `GET /traces/{trace_id}`, `POST /stats/histogram`, `POST /stats/group-by`, `POST /stats/summary`.
- Учётные данные: `GET|POST /relays`, `DELETE /relays/{id}`, `GET|POST /agents`, `DELETE /agents/{id}`.
- Здоровье: `GET /health/live`, `GET /health/ready` (вне `/api`, `api.rs:158-161`).

Авторизация: `AuthUser` требует cookie сессии, пользователь должен быть в htpasswd (`session.rs:156-181`). `require_origin` требует точного `Origin` для POST, PUT, DELETE и других небезопасных методов (`session.rs:183-196`). Обработчики поиска, аналитики и реестра принимают `AuthUser` (`Dock query.rs:177-213`, `Lognara-dock/src/access.rs`). Ответы `/api` получают `Cache-Control: no-store` (`api.rs:151-154`). Вход ограничивается `LoginLimiter` с ответом 429 и `Retry-After` (`auth.rs:76-96`, `api.rs:103-108`).

Проксирование поиска и аналитики (`Lognara-dock/src/core_client.rs`):

- `search_page` (`core_client.rs:120-131`) возвращает полный envelope `{events, next_cursor, as_of}` и отклоняет ответ, если `events` больше `limit` (`core_client.rs:127`). Проверки длины на `trace` (`:145`), гистограмму (`:160`, не более 10 000) и группировку (`:172-183`, длина групп не больше `limit`, все поля присутствуют в `values`).
- Таймаут запроса к core переопределяется на 35 секунд (`core_client.rs:21`, `:192`), общий клиент 10 секунд (`core_client.rs:19`, `:73`) не применяется к запросам поиска.
- Ошибки core → ответы dock (`Lognara-dock/src/query.rs:165-173`, преобразование `CoreQueryError`): 400 → `invalid_query`, 410 → `cursor_expired`, 503 → `query_unavailable`, 504 → `query_timeout`, прочее → 502 `core_unavailable`.

Live (`Lognara-dock/src/live.rs`): один опрос core на всех подписчиков (`live.rs:129-133`) с окном `[now - lookback, now + 60 с)`, `lookback` по умолчанию 300 с (`config.rs:184-188`), опрос каждую секунду (`config.rs:184`). Лимит 1000 событий за опрос (`live.rs:28`). Новому подписчику отдаются последние 200 (`live.rs:30`). SSE-события: `logs` (массив событий), `core_error`, `core_ok`, `lagged` (`live.rs:179-240`). Фильтров у потока нет. Дедупликация по `id` (`live.rs:154-159`). Если в окне больше 1000 событий, часть старых теряется без уведомления.

Реестр (`Lognara-dock/src/access.rs`): dock не сохраняет plaintext персональных токенов relay/agent после создания. Центральные query/admin-токены хранятся в окружении и CoreClient на сервере, не выдаются браузеру. Новый relay или agent возвращается браузеру в ответе `201` с `Cache-Control: no-store` (`access.rs:110-117`). Список возвращается как есть из core (`{relays: [...]}`, `{agents: [...]}`).

Хранилище dock (`store.rs:14-30`): таблицы `sessions`, `users`, `passkeys`. Ничего для правил, каналов или состояний не предусмотрено.

Клиент использует summary для Events/Analytics; первая страница и график Events согласованы, прошлое окно Analytics запрашивается последовательно отдельной summary. Каталог сервисов — отдельный group-by. `Lognara-dock/web/src/api.ts` отправляет `credentials: "same-origin"`. Страница «Аналитика» (`Analytics.tsx:91-127`) выбирает интервал так, чтобы корзин было не больше 72 (`Analytics.tsx:67-68`). Запросы выполняются последовательно (`Analytics.tsx:88-89`). Страница «Логи» (`Logs.tsx:139-214`) использует поиск, trace, гистограмму и `EventSource("/api/logs/stream")`.

## Соответствие экранам

| Шаблон | Что доступно сейчас | Что требуется новое |
|---|---|---|
| `Main.dc.html` (события) | поиск с фильтрами, курсор, trace, гистограмма; строка `service:billing AND level:(ERROR OR WARN)` (`Main.dc.html:103`) разбирается клиентом | summary уже даёт exact text total; полнота Live требует нового потока |
| `variant-f.dc.html` (паттерны) | исходные `message` и `attributes` | шаблонизация сообщений, идентичность паттерна, first/last seen, доля, рост за час (`variant-f.dc.html:101-108`), действия «алерт по паттерну» и «скрыть» |
| `analytics.dc.html` (аналитика) | count-гистограмма, group-by по level и service, сравнение периода клиентом | summary уже даёт p95 duration_ms по action и общий снимок; distinct вызовов и паттерны пока отсутствуют |
| `errors.dc.html` (проблемы) | `level` и `trace_id`, сырые атрибуты | группировка по fingerprint, сущность проблемы, статус, повторы, серверы, история (`errors.dc.html:98-105`), stack trace |
| `infra.dc.html` (инфраструктура) | `/metrics` core (Prometheus, query-токен), реестр relay и agent | heartbeat, последнее событие, версия, спул и здоровье каждого узла (`infra.dc.html:129-135`); заполнение диска по сегментам (`infra.dc.html:143-160`); задержка p95 (`infra.dc.html:89`) |
| `alerts.dc.html` (алерты) | count-гистограмма как предпросмотр условия | CRUD правил, фоновый evaluator, состояния, история срабатываний, каналы Telegram и Discord (согласовано пользователем, см. `Dock/DesignIntegration.md`), повтор не чаще cooldown, тихие часы, сообщение о восстановлении (`alerts.dc.html:117-136`) |

## Инфраструктура: что известно из кода

- Relay и agent не предоставляют HTTP-эндпоинтов здоровья или метрик. Relay: только `POST /v1/batches` (`Lognara-relay/src/ingest.rs:33`). Agent: только `POST /v1/logs` (`Lognara-agent/src/ingest.rs:26`).
- Потери relay и agent приходят в пачке как `dropped` (`wire.rs:18`, `wire.rs:26`) и суммируются в процессных счётчиках `relay_dropped_events_total` и `agent_dropped_events_total` (`journal.rs:140-162`, `metrics.rs:35-36`). Потери не сохраняются по источнику и не запрашиваются через API.
- Prometheus gauge `wal_bytes`, `visible_sequence`, `visibility_lag_milliseconds` доступны на `/metrics` core (`metrics.rs:12-14`). Dock их не проксирует.

## Алерты: что известно из кода

- Нет таблиц, маршрутов, фоновых задач или каналов доставки. Поиск по `alert|webhook|telegram|discord` в `Lognara-dock/src` и `Lognara-core/src` не находит реализации.
- Для вычисления условия можно использовать histogram/group-by, но они выполняются в аналитических слотах (2 на core, `storage.rs:88`). Фоновый evaluator будет конкурировать с интерактивными запросами и получать 503 при занятости (`api.rs:82`).
- Секреты каналов (токены Telegram и Discord) негде хранить. Dock не содержит хранилища секретов, а токены core не должны логироваться и кэшироваться (`access.rs:1-5`, `api.rs:151-154`).

## Пробелы и риски (по убыванию важности)

1. **Согласованность ограничена одной summary.** Summary возвращает exact total, stacked histogram, duration/actions и первую страницу из одного Snapshot. Старые отдельные endpoints и сравниваемые окна не используют общий снимок. Паттерны пока не реализованы.
2. **Произвольные атрибуты не являются измерениями.** Нет произвольного индекса, фильтра и распределений по `attributes` (`columns.rs:92`, `analytics.rs:135-152`). P95 реализован чтением утверждённого JSON-атрибута duration_ms; отдельная материализованная колонка при подтверждённой необходимости потребует решения о чтении/миграции старых сегментов. Stack trace можно оформить из существующих message/attributes без миграции. Fingerprint и паттерны требуют нового алгоритма, API и состояния, но не обязательно изменения сырых событий.
3. **Нет устойчивого идентификатора паттерна и ошибки.** Сигнатуры не существуют. Версионирование шаблона и миграция потребуют отдельной модели и пересчёта.
4. **Инфраструктура без состояния узлов.** Ни relay, ни agent не отправляют heartbeat или версию. Таблица «Узел / Heartbeat / Спул» невозможна без нового протокола и хранения.
5. **Alerts без backend.** Нужны таблицы правил, evaluator с идемпотентными состояниями, cooldown, история, отдельный пул для фоновых запросов и хранение секретов каналов. Пункт затрагивает безопасность.
6. **Retention 7 суток по умолчанию** (`config.rs:70`) отличается от примера шаблона (30 дней). Для правил с 30-дневным baseline требуется отдельное состояние/агрегаты или иная настройка retention.
7. **Общий SSE не полон и не имеет серверных фильтров.** Клиент применяет условия локально, это не обеспечивает полноту выбранного сервиса. Окно 300 секунд, лимит 1000 за опрос, дедупликация по `id`, выбор по `timestamp` события (`live.rs:28-34`, `:129-153`). Поздние события относятся к окну по времени события, а не приёма.
8. **Строгий регистр уровней.** `ERROR` даёт 400 (`query.rs:120-121`, `:145-157`). Любой новый UI должен отправлять нижний регистр.
9. **`group-by` не даёт полноты значений.** Лимит до 1000, порядок по count, нет `other` и `has_more`. Для списка сервисов на большом объёме часть значений пропадает. Значение `null` нельзя отфильтровать существующими `string[]`-фильтрами (`analytics.rs:177-206`).
10. **Нет поиска по подстроке и regexp.** Текст токенизирован (`index::tokens`). Для части шаблонов (например поиск `ERROR` внутри `message`) семантика будет отличаться от ожиданий.
11. **Курсор только вперёд и живёт 10 минут** от первой страницы (`query.rs:219`). Долгая работа оператора потеряет переход дальше.
12. **Токены core не выдаются пользователям, но управление учётными данными зависит от admin-токена dock.** Ротации и повторного показа токена нет. Удалить relay с агентами нельзя (`access.rs:191-208`).
13. **Лимит 35 секунд у dock и 30 секунд у core** (`core_client.rs:21`, `config.rs:73`). Это допустимо, но клиент не имеет собственного таймаута на `fetch` в `api.ts`.
14. **Очереди аналитики.** Два слота на весь core (`storage.rs:88`). Одновременное открытие «Аналитики» несколькими пользователями и фоновый evaluator дадут 503.

## Что можно сделать без нового backend

- Поиск, курсор, trace, live с текущими ограничениями, гистограмма объёма, group-by по level и service, count по уровням и топ сервисов по ERROR.
- Управление relay и agent, включая выдачу токена один раз.
- Доля ERROR по summary.levels с учётом всех семи уровней в знаменателе; legacy group-by level тоже доступен.
- Сравнение с предыдущим периодом двумя последовательными summary.
- Точный count на клиенте в пределах одного ответа гистограммы и сумма по `group-by level`. Для структурных фильтров без текста.

## Что нужно добавить (в порядке зависимостей)

1. Реализовано: summary с общим снимком для total, гистограммы по level, top-k, duration/actions и первой страницы, общий as_of.
2. Реализовано: числовой duration_ms и приблизительный p95 t-digest/coverage. Материализация/индекс — только по подтверждённой необходимости; stack trace требует согласованного формата.
3. Модель паттернов: шаблон, версия, first/last seen, счётчики по окнам.
4. Модель проблем: fingerprint, статус, история, связь с паттерном.
5. Heartbeat и состояние узлов: протокол агента и relay, хранение последнего контакта, версии, спула, потерь по источнику.
6. Правила алертов: таблицы, evaluator, состояния, cooldown, журнал, каналы с хранением секретов.
7. Решение по retention и холодному хранению.

## Проверки и ограничения

- Инвентаризация контрактов основана на исходниках. Проверки Rust, frontend, браузерных сценариев и запуска настоящего dock с тестовым core выполнялись отдельно в ходе реализации.
- Нагрузочная проверка и приёмка на production-данных не выполнялись.
- Вывод о Prometheus-метриках и отсутствии heartbeat основан на отсутствии таких полей в исходниках. Внешняя документация не проверялась.
