# lognara-dock

Parent: [[Index]]

## Назначение

Веб-интерфейс Lognara за nginx (TLS). Rust-сервис в `Lognara-dock/` отдаёт React-приложение из `web/`, проверяет вход, регистрирует relay и агентов, показывает логи [[Core/LognaraCore]] в реальном времени и графики метрик (раздел «Метрики»). Query- и admin-токены core хранятся только в dock.

Поток: браузер → nginx :443 → dock :7403 → `POST /v1/logs/search` core с Bearer query-токеном. Метрики: `/api/series*` → `/v1/series*` с тем же query-токеном; определения и предпросмотр → `/v1/admin/series/*` с admin-токеном.

Файлы и связи модулей описаны в [[Dock/LognaraDock-ProjectMap]].

## Оформление

Общий React-интерфейс использует тёмную терракотовую палитру (акцент `#c07a52`) из пользовательских HTML-макетов: скруглённые панели, таблицы, единая верхняя панель с подтверждением выхода, вход с анимированным фоном. Цвета уровней логов заданы отдельными токенами `--lvl-*`, чтобы TRACE не совпадал с акцентом; статусы используют `--err`/`--warn`/`--ok`. Шрифты Commissioner и IBM Plex Mono раздаются локально из `web/public/fonts/`; лицензии OFL и ссылки на исходные файлы сохранены рядом. Шрифт Outfit из макетов не используется: в нём нет кириллицы. Адаптивные стили находятся в `web/src/index.css`.

Разделы и страница входа подключаются через `React.lazy` и грузятся отдельными чанками при первом переходе; `Suspense` находится внутри `<main>`, поэтому верхняя панель остаётся на экране. `ChunkBoundary` в `web/src/App.tsx` предлагает перезагрузку, если после обновления Dock открытая вкладка запросила удалённый чанк: `ServeDir` отдаёт вместо него `index.html`.

На вкладке логов `EventsChart` показывает отдельные линии уровней с синхронным наведением; выделение мышью или пальцем записывает точные `from`/`to` в URL, колесо масштабирует выбранный диапазон. Переключатели уровней хранятся в `levels` URL и ограничивают запрос, строки, счётчики и Live пересечением с уровнями запроса; пустое пересечение не отправляет запрос. `DateRangePicker` задаёт календарный диапазон в часовом поясе интерфейса на вкладках событий, паттернов и Trace. Таблица догружает следующую страницу при прокрутке к концу; кнопка «Загрузить ещё 50» остаётся запасным путём. Паттерны скрывают Live и экспорт. Последовательность дальнейшей интеграции описана в [[Dock/DesignIntegration]].

## Вход и данные

- Пользователей задаёт htpasswd (только bcrypt), как у docker registry. Файл перечитывается при изменении (mtime, размер, inode). Удалённый пользователь сразу теряет сессии и passkey-вход. Некорректный файл запрещает вход и снимает readiness.
- Смена пароля атомарно заменяет строку пользователя: временный файл в той же папке, fsync, rename, fsync каталога. Поэтому с хоста монтируется папка `auth/`, а не файл (rename поверх bind-mount файла даёт EBUSY).
- SQLite `dock.sqlite3` в `LOGNARA_DOCK_DATA_DIR`: `sessions` (SHA-256 токена), `users` (стабильный WebAuthn user handle), `passkeys` (сериализованный `Passkey`). Общее избранное метрик хранится в `favorite_metrics` по составному ключу источника и origin. Миграции через `user_version`, каталог блокируется `dock.lock`.
- Cookie `lognara_dock_session`: `HttpOnly; Secure; SameSite=Strict`. Изменяющие запросы требуют `Origin` равный `LOGNARA_DOCK_PUBLIC_URL`. После 5 неверных паролей логин блокируется на 5 минут.
- Passkey через webauthn-rs (feature `conditional-ui`): вход без логина по user handle. Регистрация запрашивает `residentKey: required`, добавление и удаление ключа требуют текущий пароль. Состояние церемоний хранится в памяти 5 минут под cookie `lognara_dock_challenge`.

## Логи в реальном времени

Общий poller работает, пока есть подписчики SSE. Раз в `LOGNARA_DOCK_LIVE_POLL_MS` он ищет в окне `[now - LOOKBACK, now + 60s)` с `limit=1000` и рассылает только новые id через `tokio::broadcast`. Первый опрос заполняет ленту 200 последними событиями. Ограничение: при более чем 1000 новых событий за опрос лента показывает самые свежие; для полноты нужен tail-endpoint core по порядку публикации.

SSE-поток каждые 60 секунд перепроверяет сессию и закрывается при остановке процесса (`App::shutdown`), иначе graceful shutdown ждал бы его вечно.

## API истории и аналитики

Dock проксирует query API core через сессию браузера: `POST /api/logs/search`, `GET /api/traces/{trace_id}` с from/to/limit/cursor, `POST /api/stats/histogram`, `POST /api/stats/group-by` и `POST /api/stats/summary`. Входные DTO повторяют типизированный контракт core; неизвестные поля JSON отклоняются. История и Trace сохраняют `events`, `next_cursor`, `as_of` и серверный порядок. Trace возвращает связанные логи, не spans.

## MCP

`LOGNARA_DOCK_MCP_ENABLED` по умолчанию `false`; при включении `api::router` монтирует отдельный `mcp::router` на `/mcp` и `oauth::router` на metadata/authorization endpoints. MCP работает через `rmcp::transport::streamable_http_server`, stateless JSON ответы, Bearer PAT/OAuth; browser cookie и MCP-токены не дают доступ к другому набору API. Middleware ограничивает Origin/Host, размер запроса 1 MiB, результат 2 MiB, 60 запросов/минуту и 2 in-flight на пользователя, 8 на процесс; cancellation привязан к shutdown. Чтение: `list_applications`, `list_servers`, `list_environments`, `search_logs`, `get_trace`, `summarize_logs`, `list_metrics`, `query_metrics`, `list_alert_groups`, `list_alerts`, `list_log_alerts`, `get_versions` (`readOnlyHint=true`). Запись метрик: `create_alert`, `update_alert`, `delete_alert`; запись логовых правил: `create_log_alert`, `update_log_alert`, `delete_log_alert` (`readOnlyHint=false`, удаления `destructiveHint=true`). `alerts_write` управляет обоими типами алертов согласно правам чтения: логовые инструменты требуют `logs`, metric tools — `metrics`; writer с обоими правами чтения получает оба набора. Это намеренно включает log-alert tools для ранее выданных dual-read writer tokens. `list_alert_groups` доступен writer-токенам с правом чтения логов или метрик. SQL, admin proxy и live subscription не зарегистрированы.

### Права и скоуп токенов

`token_access.rs`: у каждого PAT и OAuth-гранта есть `Rights { logs, metrics, alerts_write }` и `Scope { All | Relay(id) | Agent(id) }`; для выдачи `alerts_write` нужны права чтения логов или метрик. `alerts_write` разрешает log-alert tools при `logs` и metric-alert tools при `metrics`; dual-read writer token может управлять обоими типами. Ранее выданные токены с обоими read rights и `alerts_write` получают log-alert tools без изменения сохранённых прав. Список групп доступен обоим типам writer-токенов. Миграция store 4 добавляет колонки в `mcp_personal_tokens`, `oauth_grants`, `oauth_codes` с DEFAULT «логи + метрики, без записи, all», поэтому старые credentials ведут себя как раньше; токены гранта и refresh наследуют права гранта. Relay или агент проверяются через admin list core при создании PAT и на согласии OAuth (несуществующий → 400 `invalid_scope`). Удалённый узел в скоупе даёт пустые данные: фильтрует core.

`authenticate` кладёт `TokenAccess` в extensions; `tools/list` показывает только разрешённые инструменты (логи, метрики и группы/алерты по правам; `get_versions` всем), вызов скрытого инструмента возвращает `forbidden` без обращения к core. Скоуп подставляется в каждый вызов core: `filters.relay_id`/`agent_id` у логов, источников и summary, query-параметры у trace и series, поля тела у series query. Пользовательский фильтр того же ключа отклоняется `invalid_argument`. Курсор пагинации версии 2 хранит скоуп; курсор другого скоупа или старой версии даёт `invalid_cursor`. Идентичность узла в данных и данные до раскатки: [[Core/LognaraCore]].

Metric alert tools через MCP переиспользуют валидацию `alerts.rs`. Log alert tools используют `LogAlertInput`, validation и `Store::save_log_alert` из `log_alerts.rs`; отдельная схема/миграция для MCP не нужна. Получатели задаются только ID существующих групп (`list_alert_groups` отдаёт id, тип и имя, без chat ID и bot token); создать группу или получателя через MCP нельзя. Metric alert получает скоуп токена; `update_alert` скоуп не меняет. Log alert получает фильтр скоупа только при создании. При обновлении Dock сохраняет существующие `relay_id` и `agent_id` для всех токенов, включая `All`: отсутствующие в запросе поля восстанавливаются, а смена существующего или добавление нового node filter возвращает `invalid_argument`. Update/delete сохраняют видимую revision до записи и проверяют её внутри SQLite-транзакции; конкурентное браузерное изменение возвращает `conflict` без применения изменений и побочных эффектов. Видимость log alerts ограничена: all видит все правила; relay(N) видит правила с `relay_id=N` и правила с agent filters только для агентов relay N (принадлежность по `/v1/admin/agents`, при недоступности core — ошибка, а не полный список); agent(N) видит только правила с единственным `agent_id=N`. Update/delete возвращают `not_found` для чужого правила. Миграция store 5 пересобирает `metric_alerts` с уникальностью `(server, service, name, origin, scope_kind, scope_id)` и проверяет FK; `create_alert` на существующий ключ и скоуп даёт `conflict`. Сообщение Telegram скоупленного metric alert содержит строку узла; управляющие символы в ключе метрики и окружении запрещены, чтобы не подделать строки уведомления. В UI скоупленное metric правило не редактируется, но человек может удалить его (`DELETE /api/alerts/rules/{id}`), в том числе после отзыва токена. Отдельного лимита правил на токен нет: правила токена делят общий лимит включённых алертов. Миграции 4 и 5 проверяют внешние ключи до commit каждой миграции. Раскатка и откат: [[Architecture]].

Source lists используют `CoreClient::log_sources` и query-auth `POST /v1/logs/sources`: точные source filters, активные и закрытые хранимые log-сегменты, signed pagination и snapshot `as_of`. Поиск сохраняет существующие exact filter и all/phrase семантики; поиск без периода начинает с последних 24 часов и 100 событий, cursor фиксирует resolved bounds и живёт 10 минут. Search возвращает DESC, trace ASC. Core ошибки timeout, exhausted capacity, expired snapshot и unavailable не преобразуются в пустой ответ. Метрики используют существующий query и возвращают kind/unit/aggregation/null gaps плюс `truncated` и `truncated_count` при cutoff в 20 рядов.

OAuth — Authorization Code + PKCE S256, scopes `lognara.read` и `lognara.write`, exact resource `/mcp`, согласие после активной htpasswd/passkey сессии. На согласии пользователь выбирает права и скоуп (по умолчанию логи и метрики, без записи, все узлы) и может включить запись, даже если клиент запросил только `lognara.read`; поле `scope` ответа token endpoint отражает фактически выданное. Разрешены только ChatGPT CIMD URI и callback URI из проверенного metadata. Metadata fetch закрепляет только публичные DNS IP, запрещает redirects и ограничен по времени/размеру. DCR выключен. Access token живёт 1 час, refresh 30 дней с rotation и family revoke при replay; PAT живёт 30 дней. Store migration добавляет `mcp_personal_tokens`, `oauth_pending`, `oauth_codes`, `oauth_grants`, `oauth_tokens`, сохраняет только SHA-256 секретов и вызывается через blocking worker. Удаление пользователя из htpasswd отзывает его MCP credentials при следующем запросе.

Страница `/mcp-access` показывает endpoint, одноразово выданный PAT (в памяти страницы, без localStorage), срок, последнее использование, права и скоуп и отзывает PAT/grants. Форма создания PAT и `/oauth/consent` используют общий `web/src/components/AccessFields.tsx`: право «Управление алертами логов и метрик» явно объясняет, что log alert tools требуют чтения логов, metric alert tools — чтения метрик; выбор узла — «все узлы / relay / агент relay» по `/api/relays` и `/api/agents`. Плитки и редактор metric alerts работают с правилом скоупа all; скоупленные metric правила из MCP показываются без редактирования, с кнопкой удаления в Settings. `/oauth/consent` показывает client/scope/resource; изменения защищены `session::require_origin`. Публичные metadata и MCP маршруты проксируются nginx отдельно; `/mcp` использует отключённую request/response buffering.

Новые query-запросы имеют HTTP timeout 35 секунд при default timeout core 30 секунд; Live и admin остаются на 10 секундах. Ответы `/api/*` получают `Cache-Control: no-store`. Без сессии запрос даёт 401; POST требует прежний Origin. Ответы core разбираются строго (`deny_unknown_fields`), поэтому новое поле события core ломает поиск, trace и примеры паттернов/проблем, пока его не добавят в `query::LogEvent`; необязательные `relay_id`/`agent_id` пропускаются как есть. Ошибки core переводятся в безопасные коды: 400 `invalid_query`, 410 `cursor_expired`, 503 `query_unavailable`, 504 `query_timeout`; неожиданный формат/статус или upstream 401 даёт 502 `core_unavailable`. Тексты ошибок и query/admin-токены core браузеру не выдаются.

## Страницы логов и аналитики

[Контракт summary](../../../docs/DockSummaryContract.md): один запрос на текущее окно объединяет total, семь уровней, top-5 ERROR services, duration и top-20 action groups. `p95_ms` приблизительный t-digest, измерения — числовой конечный неотрицательный `attributes.duration_ms`; UI показывает measured и coverage. Нет измерений — пустое состояние. Action count подписан как события; переход сохраняет action/service/time. Список сервисов отдельно запрашивается через group-by; прошлое окно — отдельная последовательная summary.

`/logs` открывает историю: режим events/patterns/trace, q, range, фиксированные from/to, trace_id, selected и видимые уровни сохраняются в URL. Диапазон можно задать календарём в часовом поясе интерфейса. Выбор строки дополняется selected_timestamp и selected_occurrence: одинаковые пользовательские id не раскрывают разные события одновременно. Ссылка на позднюю страницу ищет id в точном наносекундном окне события с текущими условиями, максимум 1000 совпадений. Если совпадают id и timestamp, интерфейс предупреждает о зависимости выбора от порядка; core пока не выдаёт уникальный sequence в JSON. Поиск использует 50 событий на страницу, сохраняет порядок core и повторяющиеся id; 410 предлагает повторный поиск с теми же границами. Доступны контекст источника ±5 минут, связанные логи Trace, ссылка на выбранное событие и экспорт загруженных строк в JSON/CSV. CSV экранирует потенциальные формулы, JSON сохраняет исходные значения. В режиме events summary возвращает count, stacked buckets и первую страницу в одном снимке с теми же filters/text; отключённые легендой уровни добавляются в серверный фильтр. Для диапазона свыше 10000 UTC-корзин поиск/cursor работает без сводки с явным объяснением; шаг диаграммы ограничен 86400 секундами. Догрузка cursor сохраняет сводку; обновление запускает новый summary. Trace продолжает использовать отдельный ASC API и независимый histogram.

При переходе в Trace или контекст выделение сбрасывается: ordinal нельзя переносить между DESC/ASC и разными условиями. Восстановление отдельной Trace-ссылки учитывает ASC; если точное окно содержит больше 1000 совпадений, интерфейс объясняет ограничение и не выбирает потенциально другую запись.

Query parser поддерживает точные поля, AND между условиями, OR внутри значений одного поля и token/phrase поиск message. NOT, OR между полями и смешение phrase/all отклоняются. Уровни преобразуются в lowercase для API. Время строки показано локально с миллисекундами, исходная точность сохранена в title/JSON и серверном порядке.

Live включается отдельно от истории, имеет паузу с буфером до 2000, обработку lagged и предупреждения о возможных пропусках. Space не переключает паузу внутри контролов. Фильтрация общей SSE-ленты не обещает полноту конкретного сервиса. Переподключение дедуплицирует по id; это ограничение существующего потока, а не истории.

`/logs?view=patterns` показывает серверные паттерны того же q/range/from/to: количество событий, паттернов и новых, долю от всех совпавших событий (включая скрытые), рост к прошлому окну той же длины, sparkline, покрытие истории и отставание индекса. Сортировка и `show_hidden` хранятся в URL. Раскрытие строки запрашивает детали: top-5 значений переменных и остаток, до 3 реальных примеров и ссылку «Все события» с точным `pattern_id`. Live для паттернов и для запроса с `pattern_id` отключён: идентичность вычисляет core. Действие «Алерт» скрыто до этапа 7. Проблемы группируются собственным fingerprint на странице `/errors`, поэтому строка паттерна на них не ссылается.

API: `POST /api/patterns/search?show_hidden=bool` (добавляет персонально скрытые ID в `exclude_patterns`), `POST /api/patterns/{id}/details`, `GET /api/patterns/hidden`, `POST`/`DELETE /api/patterns/{id}/hide` (204, идемпотентно, лимит 1000 → 409 `pattern_limit`). SQLite-таблица `hidden_patterns(username, pattern_id, created_at)` изолирует пользователей. Ошибки: 404 `pattern_not_found`, 503 `patterns_not_ready`/`query_unavailable`.

`/errors` показывает проблемы error/fatal по `errors.dc.html`: вкладки «Открытые» (включают «Новые» и «Возвращаются») и «Решённые» со счётчиками, сервис, период 1 час/24 часа/7 дней, «N ошибок → M проблем», статус, повторы, distinct серверы, последний случай и sparkline. Раскрытие: сообщение и stack trace последнего случая, фреймы fingerprint, серверы, история действий, «Все N событий» (`fingerprint:` в /logs, Live для него отключён), «Поиск первопричины» (Trace последнего случая или логи источника ±5 минут), «Отметить решённой»/«Открыть снова». Статус общий для всех пользователей.

API: `POST /api/issues/search`, `POST /api/issues/{fingerprint}/details`, `POST /api/issues/{fingerprint}/resolve` и `/reopen` (204, Origin). SQLite: `issue_states`, `issue_history`, `issue_watch`; до 10000 строк статусов, иначе 409 `issue_limit`. Наблюдатель `issues::watch` каждые 5 секунд читает `recurrences` core для решённых проблем и переводит в «Возвращается» только при случае после checkpoint решения и со временем не раньше решения. Ошибки: 404 `issue_not_found`, 503 `issues_not_ready`. Контракт: [DockIssuesContract](../../../docs/DockIssuesContract.md).

`/analytics` показывает периоды 1 час/24 часа/7 дней, общий count, среднее событий в минуту за окно, долю ERROR с учётом всех семи уровней в знаменателе, распределение уровней и до 5 сервисов по ERROR. Список сервисов ограничен 100 активными значениями. Сравнение использует предыдущее окно той же длины; запросы идут последовательно и отменяются при смене параметров. Основные блоки объединены одной summary, её as_of показан явно; предыдущее окно имеет отдельный снимок. График и сервис/level ведут к поиску с теми же границами/фильтрами. P95 duration_ms и action-группы доступны; алерты остаются следующим этапом [[Dock/DesignIntegration]].

| Функция | Назначение |
|---|---|
| `parseQuery(query: string): ParsedQuery` | Проверяет ограниченную строку поиска и строит filters/text. |
| `appendHistory(current: LogEvent[], page: LogEvent[]): LogEvent[]` | Добавляет страницу без потери серверного порядка и повторяющихся id. |
| `prependLive(current: LogEvent[], batch: LogEvent[], limit: number): {rows, trimmed}` | Добавляет новые поступления с ограничением памяти и Live-дедупликацией. |
| `matchesQuery(event: LogEvent, query: ParsedQuery): boolean` | Проверяет структурные/текстовые условия для ограниченной Live-ленты. |
| `withFilter(query: string, field: "pattern_id" \| "fingerprint", id: string): string` | Заменяет точный фильтр ID с сохранением остального запроса. |
| `eventBounds(timestamp: string): {from: string, to: string}` | Строит точное 1 ns окно события для восстановления ссылки без округления Date. |
| `sourceQuery(event: LogEvent): string` | Строит точные условия источника для контекста события. |
| `contextBounds(timestamp: string): {from: string, to: string}` | Строит окно контекста ±5 минут. |
| `eventsToCsv(events: LogEvent[]): string` | Формирует CSV загруженных событий с BOM, multiline и защитой формул. |
| `Histogram(props: HistogramProps): ReactElement` | Отображает count, сравнение, доступную таблицу и выбор временной корзины (аналитика). |
| `EventsChart(props): ReactElement` | График событий и Trace: выделение диапазона, масштаб колесом, фильтр уровней графика, таблица чисел. |

## Инфраструктура

`/infrastructure` использует `GET /api/infrastructure` с сессией и no-store. `infra::overview(State<AppState>, AuthUser): Json<Infrastructure>` параллельно объединяет query status core, admin-реестр и активные сессии Dock. `CoreClient::status(&self): Result<CoreStatus, CoreQueryError>` получает типизированный status; `Live::clients(): usize` считает открытые SSE receivers, `Sessions::active(): Result<u64, StoreError>` — записи сессий в пределах TTL/idle (ошибка → null). В узле агента additive `relay_id` сохраняет числовую принадлежность из реестра даже без heartbeat или найденного профиля; у узла relay поле `null`. Полный wire- и UI-контракт: [контракт инфраструктуры](../../../docs/DockInfrastructureContract.md).

Свежесть относительно as_of core: TTL 60 секунд, idle 300 секунд, spool degraded от 80%; недавняя ошибка после последнего успеха тоже degraded. До первого heartbeat unknown; устаревший агент при молчащем relay unknown/relay_silent. Узлы сортируются по приоритету offline/degraded/unknown/idle/online. Опрос UI через 10 секунд после завершения предыдущего запроса, ручное обновление; при ошибке последний снимок помечается устаревшим. Core ready=false и недоступность/частичный отказ реестра показываются отдельно.

`/infrastructure/tree` — отдельная lazy-loaded страница от кнопки в сводке; раздел навигации остаётся активным. Она группирует узлы по ID, показывает orphan-группы без выдуманного relay, рисует рёбра реестра и выбираемые пути (двойной клик по агенту открывает его логи; анимация потока на связях online-узлов декоративная), используя общий `useInfrastructure` poller и `infraPresentation` helpers. Геометрия и правила выбора лежат в `web/src/infraTree.ts`; подробная семантика — в [контракте инфраструктуры](../../../docs/DockInfrastructureContract.md).

Хранилище: filesystem total/free, Parquet/индексы (включая активный)/WAL, oldest event и retention. Source-ссылки агента строят фильтры по конфигурации за час, не подтверждают agent_id события. История телеметрии, per-node dropped, p95 доставки и прогноз диска отсутствуют.
Опрос `Live` использует общий `CoreClient` из `App`, а не собственный клиент.

## Метрики

Страница `/metrics` сначала получает только общее избранное и группирует его по серверу и сервису. Остальные метрики закрыты: после раскрытия раздела загружается список источников, после раскрытия сервера и сервиса - каталог этого сервиса. Избранное не дублируется среди остальных; звёздочка доступна при наведении, фокусе и на сенсорных устройствах. Миниграфики за 24 часа загружаются по видимости, до двух запросов одновременно, с переиспользованием результатов и отменой при сворачивании. Обновление перечитывает только избранное и раскрытые каталоги. API, составной ключ, хранение и поведение отсутствующих метрик: [контракт метрик](../../../docs/MetricsBundle.md#выборочные-метаданные-и-избранное).

- Плитка открывает `/metrics/detail?server=...&service=...&name=...&origin=...`. Метаданные получает lookup одного ключа; страница показывает большой uPlot-график, диапазоны от часа до всего времени, окружение и разбиение по экземплярам/меткам. Запрос всегда передаёт сервер и origin.
- Настройки отдельны для каждой пары сервер/сервис. Редактор в модальном окне проверяет regex, сохраняет, отключает и удаляет определение; исторические точки сохраняются.
- Создание: модалка с режимами «Вручную», «Импорт JSON», «Шаблоны». Сервер выбирается перед сервисом из `GET /api/series/sources`; смена сервера сбрасывает сервис. Источники включают историю и heartbeat.
- Импорт `lognara-metrics` v1 переносим между источниками. Проверка и применение проксируются в Core; конфликты разрешаются пропуском, переименованием или заменой. Набор сохраняется одной транзакцией с проверкой revision. Стандарт, схема, примеры и ограничения: [контракт метрик](../../../docs/MetricsBundle.md).
- Шесть шаблонов nginx: запросы, статусы, 4xx, 5xx, body_bytes_sent, error severity. Формат access — combined. Шаблоны используют тот же импорт и предпросмотр; сбор логов должен уже работать.
- Единица измерения большого графика выводится одной подписью с переносом над графиком; ширина числовой оси подбирается автоматически. Модалки используют native dialog с удержанием и возвратом фокуса. Графики и адаптивная вёрстка сохраняют общую палитру Dock.

`series.rs` проксирует обращения к Core; API избранного обращается к `Store`. Тела проксируемых запросов проверяет Core. Запрос серий ждёт core 35 секунд (`SERIES_QUERY_TIMEOUT`), остальные вызовы 10 секунд (`REQUEST_TIMEOUT`). Core отрезает запрос по `LOGNARA_QUERY_TIMEOUT_SECONDS` (30 секунд по умолчанию), то есть таймаут dock на 5 секунд больше. Ответы core переводятся в коды ошибок API: `invalid` (с `detail`), `not_found`, `core_unavailable` (502: сеть, таймаут, 5xx), `internal` (core отклонил токен). Таблица кодов: [[Dock/LognaraDock-ProjectMap]] и `Lognara-dock/README.md`.

Ограничения: диапазон «1 год» получает шаг 12 часов (730 корзин), такой шаг не измерялся: замер года выполнен для шага 1 день (см. [[Design/Metrics]], раздел «Суточные агрегаты»).

### Алерты метрик

Алерт уникален по `(server, service, name, origin)` и не зависит от избранного или успешности Core lookup. `GET /api/alerts/summary` отдаёт глобальное состояние и все ключи одним ответом; экран `/metrics` делает один общий запрос и показывает глаз у включённых правил. При глобальной паузе глаз приглушён и поясняет причину. `/metrics/detail` отдельно загружает и редактирует правило даже если метрика больше не находится в каталоге.

`GET /api/alerts` возвращает общие настройки, состояние Core и доставки, всех получателей, групп и правил, но никогда не возвращает токен. `/api/alerts/token` проверяет Bot API `getMe` до сохранения. `/api/alerts/global` включает систему только после успешной проверки токена. Получатели — личные чаты или каналы с numeric chat ID; группы содержат получателей одного типа, а правило может выбрать группы обоих типов. Удаление используемой группы блокируется и перечисляет привязанные ключи. Удаление получателя отменяет его ожидающие доставки и доставки в процессе отправки.

Правило выбирает одно окружение или все, группы получателей, `and`/`or`, 1..20 триггеров и обработку отсутствующих данных `normal`/`loss`. Окна 60..604800 секунд, агрегаты `avg|min|max|sum|count|increase`, строгие `>`/`<` и конечный порог. Максимум 100 включённых правил Dock. При обычном режиме пустые `sum`/`count` равны нулю; другие пустые агрегаты не срабатывают. Для `loss` threshold state заморожен, событие потери создаётся после 5 минут без свежей точки от включения правила или последней точки; Dock сохраняет последнюю известную точку, когда Core ограничивает ответ freshness последними пятью минутами. Возврат отдельный и только после зарегистрированной потери. Срабатывание и восстановление условий тоже отдельные события; первое состояние OK молчит. AND срабатывает, если все триггеры истинны; OR — если истин хотя бы один; отсутствие данных не создаёт переход условия.

`alerts::watch` работает на Dock сервере без открытого браузера, один последовательный проход раз в минуту; пропущенные тики не накапливаются. Он запрашивает Core напрямую через query-токен и `/v1/series/alerts/query`; этот API не проксируется браузеру. Для правил без `increase` Dock просит Core пропустить baseline history; `last_point_at` старше пяти минут возвращается как null. Busy/overload задерживает проверку, incompatibility виден в Settings. Первый Unavailable/Timeout прерывает проход, сразу фиксирует начало outage и запускает отправку outbox; непрерывная недоступность Core 5 минут создаёт одно общее событие уникальным текущим получателям включённых правил, успешный ответ Core создаёт recovery. Таймер недоступности Core монотонный и находится в памяти процесса: простой самого Dock не считается отказом Core.

SQLite миграция создаёт `metric_alert_global`, recipients/groups, definitions/triggers/states и outbox/deliveries. Все вызовы SQLite идут через `Store::call` в blocking worker. Оценка состояния, смена перехода и снимок адресатов пишутся одной транзакцией; revision глобальных настроек и правила отбрасывает результат Core-запроса, если его конфигурация устарела. Отключение/редактирование отменяет старые pending события, новые адресаты не получают историю. Восстановление Core использует снимок адресатов события потери, пропуская удалённых адресатов и изменённые chat ID. После перезапуска статус `sending` возвращается в pending, поэтому неопределённый сетевой исход допускает дубль; подтверждённая отправка не повторяется. FIFO действует для пары rule/recipient.

Telegram получает plain text `sendMessage`, не более 4096 символов: источник, окружение, условия, значения, время Core и URL метрики. Временные ошибки повторяются с backoff от 30 секунд до часа, `429` задаёт общую паузу `retry_after`; одна отправка ограничена числом получателей и бюджетом времени прохода. Постоянная ошибка показана у получателя, invalid token приостанавливает глобальную отправку; успешная проверка токена при сохранении или включении снимает паузу. Ошибки проверки Telegram отдельно сообщают временную недоступность и `429`. Pending живут до 24 часов, завершённые записи — 7 дней. Неизвестный исход сетевого запроса может привести к дублю. Token хранится только в SQLite Dock и входит в резервную копию `dock.sqlite3`; GET, Debug, журналы и ошибки его не раскрывают. Клиент Telegram использует фиксированный `https://api.telegram.org/`, отдельный reqwest client и запрет redirects. Записи защищены сессией и `Origin`.

Перед включением пользователь создаёт/запускает Telegram-бота через BotFather. Боту нужно хотя бы раз написать в личном чате; для канала его добавляют с правом публикации и указывают numeric chat ID. Раскатка выполняется сначала для Core, затем для Dock: при старом Core правила остаются настраиваемыми, статус показывает incompatible и события потери Core не отправляются.

## Параметры запуска

| Переменная | Обяз. | По умолчанию | Назначение |
|---|---|---|---|
| `LOGNARA_DOCK_PUBLIC_URL` | да | - | внешний адрес; origin для passkey и проверки Origin; http только для localhost |
| `LOGNARA_DOCK_CORE_URL` | да | - | базовый адрес core |
| `LOGNARA_DOCK_CORE_TOKEN` | да | - | query-токен core, скрыт в Debug |
| `LOGNARA_DOCK_CORE_ADMIN_TOKEN` | да | - | отдельный admin-токен core для relay, агентов и определений метрик, ASCII без пробелов/control, скрыт в Debug |
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
| `LOGNARA_DOCK_MCP_ENABLED` | нет | `false` | включает отдельные MCP и OAuth endpoints |
| `LOGNARA_DOCK_WEB_DIR` | нет | `/usr/share/lognara-dock/web` | собранный фронтенд |

Пример Compose, nginx и Dockerfile лежат в `Lognara-dock/`; подробности в `Lognara-dock/README.md`.

## Ключевые методы/функции

- `Config::from_lookup(lookup: impl Fn(&str) -> Option<String>): Result<Config, ConfigError>` — проверяет `LOGNARA_DOCK_*`, вычисляет origin и RP ID
- `App::open(config: Config): Result<AppState, Box<dyn Error>>` — читает htpasswd, открывает SQLite, строит Webauthn и Live
- `api::router(app: AppState): Router` — browser `/api/*`, MCP/OAuth routers, health, SPA-fallback и защитные заголовки
- `mcp::router(app: AppState): Router` — stateless Streamable HTTP `/mcp`, Bearer validation, quotas, фиксированный allowlist инструментов с фильтрацией по правам токена
- `oauth::router(app: AppState): Router` — protected-resource/authorization-server metadata и ChatGPT CIMD authorization code + PKCE endpoints
- `mcp_access::{access,tokens,create_token,revoke_token,grants,revoke_grant}` — browser management PAT и OAuth grants
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
- `CoreClient::search_page(&self, request: &SearchRequest): Result<SearchResponse, CoreQueryError>` — историческая страница с фильтрами, текстом и курсором
- `CoreClient::log_sources(&self, request: &LogSourcesRequest): Result<LogSourcesPage, CoreQueryError>` — точные страницы service/server/environment из query-auth Core API
- `CoreClient::trace(&self, trace_id: &str, params: &TraceParams): Result<SearchResponse, CoreQueryError>` — связанные события Trace по времени ASC
- `CoreClient::histogram(&self, request: &HistogramRequest): Result<HistogramResponse, CoreQueryError>` — временные корзины count
- `CoreClient::group_by(&self, request: &GroupRequest): Result<GroupResponse, CoreQueryError>` — ограниченные группировки count
- `CoreClient::summary(&self, request: &SummaryRequest): Result<SummaryResponse, CoreQueryError>` — получает сводку и проверяет лимиты, суммы, duration и общий as_of страницы
- `query::search/trace/histogram/group_by/summary(State, AuthUser, ...): Result<Response, ApiError>` — авторизованные proxy query API
- `CoreClient::patterns(&self, request: &PatternRequest): Result<PatternResponse, CoreQueryError>` — паттерны core с проверкой сумм, лимитов и отсутствия исключённых ID
- `CoreClient::pattern_details(&self, pattern_id: &str, request: &DetailsRequest): Result<DetailsResponse, CoreQueryError>` — детали паттерна с проверкой ID, переменных и примеров
- `patterns::search/details/hidden/hide/show(State, AuthUser, ...): Result<..., ApiError>` — паттерны и персональное скрытие
- `Store::hidden_patterns(&self, username: String): Result<Vec<String>, StoreError>` — скрытые ID пользователя
- `Store::hide_pattern(&self, username: String, pattern_id: String): Result<bool, StoreError>` — идемпотентно скрывает, false при лимите 1000
- `Store::show_pattern(&self, username: String, pattern_id: String): Result<(), StoreError>` — снимает скрытие
- `CoreClient::issues(&self, request: &CoreSearchRequest): Result<IssueResponse, CoreQueryError>` — проблемы статуса с проверкой вкладки, сумм и лимита
- `CoreClient::issue_details(&self, fingerprint: &str, request: &DetailsRequest): Result<IssueDetails, CoreQueryError>` — детали проблемы с проверкой суммы серверов
- `CoreClient::issue_recurrences(&self, request: &RecurrenceRequest): Result<RecurrenceResponse, CoreQueryError>` — случаи решённых проблем после checkpoint
- `issues::search/details/resolve/reopen(State, AuthUser, ...): Result<..., ApiError>` — проблемы и общий статус
- `issues::sync(app: &App): Result<bool, SyncError>` — один шаг наблюдателя, true пока core не догнан
- `issues::watch(app: AppState)` — фоновый наблюдатель возвратов до остановки dock
- `Store::issue_states(&self): Result<HashMap<String, IssueState>, StoreError>` — все строки статусов
- `Store::issue(&self, fingerprint: String): Result<(Option<IssueState>, Vec<IssueAction>), StoreError>` — статус и 50 последних действий
- `Store::resolve_issue(&self, fingerprint: String, username: String): Result<bool, StoreError>` — решение с checkpoint наблюдателя, false при лимите
- `Store::reopen_issue(&self, fingerprint: String, username: String): Result<(), StoreError>` — открывает решённую
- `Store::issue_watch(&self): Result<(Option<u64>, Vec<(String, i64)>), StoreError>` — checkpoint и решённые проблемы
- `Store::record_recurrences(&self, after: u64, watermark: u64, found: Vec<Recurrence>): Result<Vec<String>, StoreError>` — возвраты и новый checkpoint одной транзакцией
- `CoreClient::call_within(&self, method: Method, path: &str, token: &str, body: Option<&Value>, timeout: Duration): Result<CoreReply, CoreError>` - общий HTTP-вызов core с явным таймаутом; `call` подставляет 10 секунд
- `CoreClient::series_query(&self, query: &Value): Result<CoreReply, CoreError>` - `POST /v1/series/query` с таймаутом 35 секунд
- ~~`CoreClient::admin_enabled`~~ (удалён: 2026-10-14)
- `CoreClient::preview(&self, request: &Value): Result<CoreReply, CoreError>` - предпросмотр правила, admin-токен
- `From<CoreError> for ApiError` (`api.rs`) - перевод ответов core в коды ошибок API
- `Live::new(config: &Config, core: Arc<CoreClient>, shutdown: CancellationToken): Arc<Live>` - опрос использует общий клиент
- `live::stream(State, AuthUser): Sse<...>` — SSE `logs`, `core_error`, `core_ok`, `lagged`
- `series::query(State, AuthUser, Json<Value>): Result<(StatusCode, Json<Value>), ApiError>` - `POST /api/series/query`, тело передаётся core как есть
- `series::list(State, AuthUser): Result<(StatusCode, Json<Value>), ApiError>` - `GET /api/series`
- `series::create_definition(State, AuthUser, Json<Value>): Result<(StatusCode, Json<Value>), ApiError>` - `POST /api/series/definitions`, admin-токен

## Зависимости

- Использует: [[Core/LognaraCore]] (`POST /v1/logs/search`, `GET /v1/series`, `POST /v1/series/query` с query-токеном; `/v1/admin/*`, включая `/v1/admin/series/*`, с admin-токеном).
- Библиотеки фронтенда: uPlot (`web/package.json`, график метрик), React и react-router.
- Используется в: браузер через nginx.

## Подключения

Страница `/access` («Подключения») размещает формы создания relay и агента перед списком. После создания relay выбирается в форме агента. Агенты показаны адаптивной сеткой плиток внутри секции своего relay; доступны удаление и перевыпуск токена с подтверждением. React получает список без токенов. После создания или перевыпуска новый токен виден один раз вместе с `.env` строкой `LOGNARA_RELAY_CORE_TOKEN=…` или `LOGNARA_AGENT_TOKEN=…`. Поле `textarea readOnly` позволяет выделять фрагменты; кнопки копируют строку или только секрет, показывают успех или ошибку Clipboard API. До «Я сохранил токен» создание и перевыпуск следующих токенов заблокированы. Plaintext хранится только в состоянии страницы, не в localStorage; ошибка обновления списка не скрывает уже выданный секрет. При выборе relay учитывается его удаление из списка.

API GET/POST `/api/relays`, `/api/agents`, DELETE по `/{id}` и POST `/api/agents/{id}/rotate-token` требует AuthUser, изменения — корректный Origin. Перевыпуск без тела проксируется в одноимённый admin-маршрут core и возвращает 200 `{agent, token}`; сохраняемые данные и отзыв описаны в [[Core/LognaraCore#Реестр подключений]]. Запрос автоматически не повторяется при ошибке. CoreClient отправляет admin Bearer с REQUEST_TIMEOUT=10s. Известные коды core отображаются статическими строками с сохранением 400/404/409; недоступность, неверная авторизация core и неожиданный ответ дают 502 core_unavailable. Ответ создания — 201, перевыпуска — 200, оба с Cache-Control no-store. Query/admin-токены различаются и не выдаются браузеру.

| Метод | Назначение |
|---|---|
| `CoreClient::list_relays(): Result<Value, CoreError>` | Получает профили relay из core. |
| `CoreClient::create_relay(name: &str): Result<Value, CoreError>` | Создаёт relay и проксирует одноразовый токен. |
| `CoreClient::delete_relay(id: i64): Result<(), CoreError>` | Удаляет relay без агентов. |
| `CoreClient::list_agents(): Result<Value, CoreError>` | Получает профили агентов. |
| `CoreClient::create_agent(name: &str, relay_id: i64): Result<Value, CoreError>` | Создаёт агента в выбранном relay. |
| `CoreClient::delete_agent(id: i64): Result<(), CoreError>` | Удаляет агента. |
| `access::list_relays(State<AppState>, AuthUser): Result<Json<Value>, ApiError>` | Авторизованный список relay. |
| `access::list_agents(State<AppState>, AuthUser): Result<Json<Value>, ApiError>` | Авторизованный список агентов. |
| `access::create_relay(State<AppState>, AuthUser, Json<NewRelay>): Result<Response, ApiError>` | Валидирует JSON и проксирует создание relay. |
| `access::create_agent(State<AppState>, AuthUser, Json<NewAgent>): Result<Response, ApiError>` | Валидирует JSON и проксирует создание агента. |
| `access::delete_relay(State<AppState>, AuthUser, Path<i64>): Result<StatusCode, ApiError>` | Проксирует удаление relay. |
| `access::delete_agent(State<AppState>, AuthUser, Path<i64>): Result<StatusCode, ApiError>` | Проксирует удаление агента. |

Обработчики принимают JSON/Path как Result и возвращают JSON invalid_request вместо plain-text rejection. tests/access.rs проверяет admin Bearer/path/body, no-store, сессию, Origin, 409/404 и 502.

### Хост-агенты

Страница «Подключения» показывает хосты отдельным разделом: создание (`POST /api/hosts`), смена токена (`POST /api/hosts/{id}/rotate-token`) и удаление (`DELETE /api/hosts/{id}`) проксируются в hosts API core ([[Core/LognaraCore]]) с admin-токеном. Токен `LOGNARA_HOST_TOKEN` показывается один раз. Хосты не попадают в выбор relay для агента и в список relay; 409 `host_node` от core показывается текстом.

### Алерты на логи

Страница `/log-alerts` (`Lognara-dock/web/src/pages/LogAlerts.tsx`) и API `GET/POST /api/log-alerts`, `PUT/DELETE /api/log-alerts/{id}` (`Lognara-dock/src/log_alerts.rs`). Правило хранит строку запроса страницы «Логи» и разобранные фильтры (`environment`, `server`, `backend`, `service`, `service_instance`, `action`, `request_id`, `level`, `relay_id`, `agent_id`) и текст; поля одного события (`id`, `trace_id` и т. п.) отклоняются. Ссылка из Telegram включает фактические `relay_id` и `agent_id` отдельными параметрами URL; Logs объединяет их с query перед запросом Core. Порог - минимум новых совпадений за одну проверку, пауза - минимальный интервал между сообщениями; совпадения паузы приходят сводкой. Кнопка «Создать алерт» на странице логов переносит текущий запрос, «Проверить за час» считает совпадения через summary. До 100 включённых правил.

`alerts::watch` после алертов метрик одним запросом `POST /v1/logs/alerts/query` проверяет все включённые лог-алерты (`log_alerts::run_log_pass`). Первая проверка, изменение правила, повторное включение алертов или уменьшение watermark (новые данные Core) только запоминают watermark. Результат фиксируется одной транзакцией вместе с событиями outbox; изменение правил во время запроса (ревизия global) отбрасывает его. Статус Core для лог-алертов хранится отдельно (`metric_alert_global.log_core_status`): старый Core без маршрута помечается `incompatible` и не влияет на алерты метрик и событие потери Core.

Получатели, группы, outbox и доставка Telegram общие с алертами метрик. Миграция SQLite добавляет `log_alerts`, `log_alert_groups_used`, `log_alert_states` и `metric_alert_outbox.log_alert_id`. Доставка проверяет, что лог-алерт включён и ревизия события совпадает; очередь одного получателя упорядочена отдельно для каждого правила и для общих событий Core. Группа, используемая лог-алертом, не удаляется (`group_in_use` с полем `log_alerts`); получатели включённых лог-алертов получают и общее событие потери Core. Раскатка: core, затем dock.
