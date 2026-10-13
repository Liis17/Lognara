# Dock: API и данные для интеграции шаблонов

Дата проверки: 2026-10-14. Исследование исходников текущей ветки; код, ветки и коммиты не менялись. Прочитаны Obsidian Index, Core/Dock и их ProjectMap, README, query/analytics/model/wire/access/metrics, клиент и live Dock, ingest/normalize/auth-cache relay, wire/config/sender agent. Шаблоны изучены как примеры желаемого поведения, не как действующие контракты.

## Краткий вывод

Core уже умеет исторический поиск, trace lookup, временную гистограмму и группировки count. Dock предоставляет браузеру только SSE логов и CRUD реестра подключений; его внутренний search-клиент извлекает только `events`, теряя курсор и `as_of`. Первая интеграция логов и базовой аналитики требует расширения Dock, а не нового хранилища. Паттерны, проблемы, alert-engine и телеметрия узлов требуют новых серверных моделей. [Core router][core-router], [Dock router][dock-router], [CoreClient::search][dock-client]

| Область | Уже есть | Нужно расширить | Полностью новое |
|---|---|---|---|
| События (`Main.dc.html`) | Поиск, точные фильтры, текст message, JSON события, cursor | API Dock, парсер строки запроса, `total` для текстового поиска, согласованная сводка, экспорт | Надёжный tail по порядку публикации, если требуется полнота Live |
| Паттерны (`variant-f.dc.html`) | Исходные message/attributes | Поиск/агрегаты по идентификатору паттерна | Версионированный fingerprint, шаблон/переменные, first/last seen, доля, рост, samples, скрытие |
| Ошибки (`errors.dc.html`) | Фильтр level, trace/span IDs, сырые attributes | Извлечение stack trace, события trace в Dock | Проблема как сущность, статус/история/возврат, связь с паттерном/деплоем |
| Аналитика (`analytics.dc.html`) | Histogram/count group-by, топ сервисов по ERROR | Серии по level и общая сводка в одном снимке, p95 числового атрибута | Число/новизна паттернов до появления модели паттернов |
| Инфраструктура (`infra.dc.html`) | Профили relay/agent; core Prometheus counters/gauges | JSON API состояния core/Dock, storage sizes, временные ряды | Достоверная связь профилей и источников; heartbeat, версии, spool и health каждого узла |
| Алерты (`alerts.dc.html`) | Count-гистограмма как примитив вычисления условия | Серверные агрегаты для preview | CRUD правил, evaluator, состояния/история, cooldown/recovery/quiet hours, каналы и доставка |
| Facets / top-k / stats / SQL | Group-by даёт ограниченный top count; histogram даёт count | Facet-контракт/полнота, числовые агрегаты, summary | Произвольный SQL API отсутствует и шаблонам не требуется |

## Поиск, фильтры и пагинация

`POST /v1/logs/search`, query Bearer. Тело: `from`, `to` RFC3339, `filters: {field: string[]}`, optional `text: {query, mode}`, `limit` (default 100), optional `cursor`. Неизвестные поля отклоняются, диапазон строго `from < to`, внутри используется `[from,to)` по времени **события**. `limit` 1..1000. [SearchRequest][query-request], [PreparedFilter][query-filter], [range query][query-range]

Разрешены точные поля `id`, `environment`, `server`, `backend`, `service`, `service_instance`, `action`, `trace_id`, `span_id`, `parent_span_id`, `request_id`, отдельно `level`. Между полями AND, внутри массива значений OR. Нет произвольного OR между полями, NOT, сравнения чисел, regexp/wildcard, фильтра по attributes/ingested_at или сортировки по произвольному полю. Каждому фильтру нужны 1..64 значения, значение ≤4096 байт. UUID нормализуется; trace ID = 32 hex, span ID = 16 hex, регистр hex нормализуется. Имена source/action/request_id сравниваются точно. [Exact fields][index-fields], [validation][query-filter], [BooleanQuery][query-range]

API принимает `trace`, `debug`, `info`, `warn`, `error`, `fatal`, `unknown` в нижнем регистре. `ERROR` в JSON filters даёт 400: query::level сравнивает `as_str()` без нормализации. Relay при ingest принимает mixed-case текст уровня и нормализует. UI может показывать `ERROR`, но отправлять должен `error`. [query::level][query-level], [LogLevel][wire-level], [relay::level][relay-level]

Текст ищется только в `message`: `all` (default) = все токены AND; `phrase` = фраза по позициям. SimpleTokenizer + LowerCaser: это токенный поиск, не поиск подстроки. Query ≤8192 байт и 1..64 токена. Строка `service:billing AND level:(ERROR OR WARN)` из Main не является языком core: нельзя отправить её в `text.query` и ожидать фильтров. Нужен ограниченный, явно описанный парсер Dock или отдельные контролы, переводящие её в `filters` и `text`. [TextQuery][query-request], [token validation][query-search], [tokenizer][index-tokenizer], [query building][query-range], [Main query][template-main-query]

Ответ только `{events, next_cursor, as_of}`. **`total` отсутствует**, длина events - размер страницы. Сортировка search DESC по `(timestamp, sequence)`; trace ASC. Core выбирает limit+1, чтобы определить наличие следующей страницы. Нет offset/page-number и произвольного перехода на страницу. Нужна кнопка «Загрузить ещё» или сохранённый стек cursor для возврата назад. Серверный порядок следует сохранять; JS Date теряет наносекунды, а `sequence` не отдаётся в LogEvent. Текущий фронтенд пересортировывает SSE через Date.parse. [SearchResponse][query-response], [pagination][query-pagination], [TopDocs][query-topdocs], [Logs merge][dock-logs-merge]

Курсор HMAC фиксирует fingerprint фильтров/text/направления, watermark и `as_of`, живёт 10 минут от **первой** страницы, инвалидируется при рестарте и смене retention generation; изменение запроса даёт 400, истечение/несовместимый snapshot - 410. `as_of` - момент публикации снимка core, не конец диапазона и не SLA полноты доставки. Между независимыми search/histogram/group-by нет общего snapshot token: ответы могут иметь разные `as_of`. Для строго согласованной таблицы/total/графика нужен один серверный summary/search-with-summary или новый snapshot-контракт. [cursor checks][query-cursor], [cursor creation][query-pagination], [snapshot publication][storage-publish]

Ошибки query: 400 invalid, 410 cursor gone, 503 capacity/storage, 504 timeout. Четыре concurrent search slots, два analytics slots; по умолчанию timeout 30s, search page memory 128 MiB, общий DataFusion pool 1 GiB. Новый клиент Dock должен сохранять семантику ошибок и envelope; сейчас общий HTTP timeout 10s и возвращаются только events. [errors][core-query-errors], [slots/pool][storage-runtime], [limits][core-config], [Dock client][dock-client]

### `total`, facets, top-k и stats: что можно считать сейчас

- Для **структурных** фильтров total можно получить суммой всех bucket.count histogram или суммой `group-by level` с limit ≥7 (уровней ровно семь). Это полный count в снимке данного запроса, не размер страницы. Для text-фильтра такой путь отсутствует: HistogramRequest/GroupRequest не имеют text. [analytics requests][analytics-requests], [histogram][analytics-histogram], [group-by][analytics-group], [levels][wire-level]
- `group-by service` с `filters.level=["error"]` даёт топ сервисов по ERROR; `group-by action` даёт число лог-событий на action. Это **не число вызовов**, если приложение пишет несколько логов на вызов. Нет distinct request/span, duration, p95 или sum/avg/min/max. [group aggregate][analytics-group]
- Для выпадающего списка сервисов/серверов можно временно применять group-by соответствующего поля. Это максимум 1000 групп, отсортированных count DESC, без `next_cursor`, `has_more`, `other_count`, distinct-total или prefix search. Нельзя обещать полный каталог значений на больших данных. Возврат null есть для optional environment/service_instance/action; отфильтровать null текущими string[] фильтрами нельзя. Нужно определить, учитывает facet все активные фильтры или исключает собственный. [group response][analytics-requests], [group limit/order][analytics-group], [filter literals][analytics-frame]
- Endpoint `/facets`, `/topk`, `/stats` summary отсутствуют; семантически ограниченный top-k count уже покрыт `/v1/stats/group-by`. Не стоит заводить дублирующий endpoint без потребности в новой семантике. [Core routes][core-router]

## Гистограммы и аналитика

`POST /v1/stats/histogram`: `from`, `to`, `filters`, `interval_seconds` (default 60). Ответ `{buckets:[{timestamp,count}], as_of}`. Interval 1..86400 s, ≤10000 buckets; UTC floor от Unix epoch, первая корзина может начаться раньше from, но учитываются только события `[from,to)`. Нули заполнены. Это **одна серия count**, без разбиения по level/service. [request/response][analytics-requests], [histogram implementation][analytics-histogram]

Стек WARN/ERROR/INFO из шаблонов можно получить отдельными histogram-запросами с соответствующим lower-case level; сравнение предыдущего периода - отдельный запрос со сдвинутым диапазоном. Результат не атомарен между запросами и расходует два доступных analytics slots; рекомендуется серверная многосерийная гистограмма/summary для целого экрана. При большом числе параллельных запросов клиент получит 503. Доля ERROR = count(error) / count(all), средняя частота = total / длительность окна; смысл «событий в минуту» (последняя минута или среднее за выбранный период) нужно назвать. [histogram][analytics-histogram], [capacity][core-analytics-run], [analytics KPI][template-analytics]

`POST /v1/stats/group-by`: `group_by` из 1–2 **разных** полей environment/server/backend/service/service_instance/action/level; limit default 100, 1..1000. Count DESC, затем ключи ASC, значения строка/null. Группировка по message/attributes/pattern/trace/request_id не разрешена. Histogram и group-by имеют общие точные filters; text, SQL и attributes-предикаты отклоняются через deny_unknown_fields или PreparedFilter. [requests][analytics-requests], [validation and aggregate][analytics-group]

DataFusion читает выбранные по времени Parquet и активные Arrow batches одного core snapshot, фильтрует timestamp и `sequence <= watermark`. Внешнего SQL нет: код строит типизированные выражения, таблиц SQL для браузера не регистрирует. SQL не является обязательной частью шести шаблонов; добавление редактора SQL расширит отдельную поверхность доступа/ресурсов. [frame][analytics-frame], [Core routes][core-router]

`duration_ms` отсутствует в Event, LogEvent и Arrow schema. При JSON ingest верхний duration_ms становится `attributes.duration_ms`; nested attributes перекрывает одноимённый верхний ключ. Тип и единицы не проверяются. В Arrow attributes хранится UTF-8 JSON-строкой; текущие агрегаты его не разбирают. Для p95/action нужно сначала определить контракт: numeric finite ≥0 ms, какие события являются завершением операции, как считать неизвестные/некорректные значения, точный или приближённый percentile. Минимальная реализация может читать утверждённый атрибут; отдельная типизированная колонка и миграция нужны только после решения о нормализованном поле. [Event][wire-event], [JSON API][core-model], [Arrow schema][columns-schema], [normalize::fill][relay-fill], [analytics p95][template-analytics]

## Trace и ошибки

`GET /v1/traces/{trace_id}?from=...&to=...&limit=...&cursor=...` вызывает обычный search с единственным trace_id фильтром и ASC. Это полные log events с trace/span/parent_span IDs, **не коллекция завершённых spans**. Контракт не содержит start/end/span kind/status/duration; из одного timestamp нельзя построить достоверную waterfall 820/750/520 ms из errors. Первая версия может показывать timeline событий и ссылки; waterfall требует отдельного span-контракта либо утверждённых атрибутов start/end/duration и правил неполных spans. [trace handler][core-trace], [LogEvent][core-model], [waterfall example][template-errors]

Stack trace/exception/deploy version не имеют канонических полей. Их можно показать из message/attributes при известном формате приложения; текущий relay сохраняет незнакомые JSON keys. Простое оформление Stack trace возможно, автоматическое выделение app/framework frames и fingerprint требует правил нормализации. [normalization][relay-fill], [stack trace template][template-errors]

`variant-f` требует паттерн с переменными ip/num/word/uuid, count/share/growth/sparkline, first seen/new, samples, доли значений переменных, скрытие, связь с проблемой и алертом. В core нет pattern_id/fingerprint на событии и агрегатов по ним. `fingerprint` в query.rs относится к **параметрам курсора**, а не сообщениям. Нельзя группировать один browser page и показывать count/share как статистику всего окна. Нужна серверная версия алгоритма, стабильный ID, ограничения cardinality и хранение соответствия event→pattern, а также миграционная стратегия для старых событий. [Event/schema][wire-event], [index fields][index-fields], [allowed group-by][analytics-group], [cursor fingerprint][query-search], [pattern template][template-patterns]

`errors` дополнительно требует persisted issue с состояниями new/open/resolved/reopened, first/last occurrence, count за окно, affected servers, transitions пользователя и связь с deployment. Ни API, ни таблиц проблем нет. Предлагается хранить пользовательские состояния/историю отдельно от сырых логов, с устойчивой связью на pattern/fingerprint: retention логов не должен молча стирать решение проблемы. Выбор хранилища/владельца (core или Dock) - отдельное архитектурное решение. [Core routes][core-router], [Dock migrations][dock-store], [errors template][template-errors]

«Контекст ±5 мин» можно реализовать search вокруг timestamp с нужными source filters; связь по trace/request_id уже возможна. Нужно определить scope контекста (инстанс/сервис/trace), иначе попадут чужие события. «Копировать ссылку» - frontend URL-state с диапазоном/filters/selected id; id уже разрешён как фильтр. «Все события»/экспорт требуют пагинации core, не выгрузки последних 2000 SSE строк. [Exact fields][index-fields], [Main actions][template-main-actions], [cursor semantics][query-cursor]

## Инфраструктура и идентичность

Реестр core: relay `{id,name,created_at}`, agent `{id,name,relay_id,created_at}`. В нём нет last_seen, source, version, address, heartbeat, spool, last_event, health. Имя relay уникально глобально, имя agent внутри relay; поля name обрезаются trim, 1..64 символа без control. Это **учётные данные**, не обнаруженные работающие узлы. [registry models/schema][core-access], [registry names][core-access-name]

Source события - `{environment?, server, backend, service, service_instance?}`. Агент берёт их из env (`service_instance` fallback HOSTNAME), relay переносит из batch. В Event/Source не входят `agent_id`/`relay_id`; core получает relay_id только в request extensions для authorization, а ingest обработчик не сохраняет его в событии. Relay получает от core только список хешей токенов агентов; его authorize возвращает boolean membership, не agent_id. Поэтому невозможно достоверно связать `agent.name == server` или `relay.name == backend`, вычислить last_event зарегистрированного узла либо безопасно ограничить Source указанным профилем. Один агент может заявить любой Source. [Source][wire-source], [agent config][agent-config], [relay source mapping][relay-ingest], [core authorization][core-relay-auth], [core ingest signature][core-ingest], [agent hashes/cache][relay-agents], [relay auth][relay-auth]

До телеметрии нужно утвердить node identity: сохранять аутентифицированный relay_id/agent_id на сервере, связать их с Source и heartbeat; определить поведение при пересоздании профиля/смене service_instance и совместимость старого wire. Не стоит строить инфраструктурный монитор на совпадении строковых имён. Для нескольких экземпляров одного профиля отдельно решить стабильный instance ID и boot ID.

Heartbeat endpoint и wire отсутствуют. Пустая пачка agent→relay отвергается, sender agent без pending выходит, поэтому отсутствие логов не означает отсутствие подключения. Статусы «Нет новых логов» и «Отключён» из infra принципиально не выводятся из current last log; нужен независимый heartbeat, received_at на принимающей стороне, TTL/stale и правила degraded. [empty batch][relay-ingest], [sender empty][agent-sender], [infra status semantics][template-infra]

Core `/metrics` защищён query Bearer, отдаёт Prometheus: accepted/duplicate/rejected batches, accepted events, relay/agent dropped, WAL bytes, visible_sequence, visibility lag, sealed/deleted segments, queries/errors, total query/ack duration. Counters не размечены по узлам; cumulative duration не позволяет p95. Нет per-node CPU/RAM, свободного диска, Parquet/index bytes, accepted/transport bytes total, process version или endpoint истории. Метрики создаются заново при Journal::open (wal_bytes инициализируется), rate требует delta/time и обработку restart/reset. `delta(accepted_events_total)/delta(time)` даёт скорость приёма новым core процессом, а histogram по timestamp - частоту событий приложения; эти показатели имеют разные часы и не взаимозаменяемы. WAL bytes - текущий объём очереди, не объём всего хранилища и не скорость трафика. [metrics][core-metrics], [metrics endpoint][core-metrics-endpoint], [metrics initialization][core-journal]

`visibility_lag_milliseconds` = возраст старейшей ещё не материализованной WAL-пачки от её receipt.received_at до now. Это не p95 «от события до Core». `core_received_at` хранится внутренне в Arrow, но отсутствует в JSON; `timestamp` относится приложению (fallback приёма агентом), `ingested_at` - приёму агентом. Для p95 доставки нужна измеряемая latency distribution, явные точки измерения и оговорка clock skew. [lag calculation][storage-publish], [StoredEvent/LogEvent][core-model], [normalization timestamps][relay-event], [columns][columns-schema]

Core retention default **7 суток**, шаблон показывает 30. Retention удаляет целые закрытые сегменты по max core_received_at, не по timestamp события; receipt history живёт минимум 7 дней. В `infra` нужно отображать фактическую config из серверного API, а не число из макета. Storage breakdown, oldest segment и прогноз disk fill требуют новых измерений/истории и обработки невозможности прогноза. [config][core-config], [retention][storage-retain], [infra storage][template-infra]

Dock уже знает размер broadcast subscriptions внутри Live и хранит sessions SQLite, но количества SSE-клиентов/активных сессий и версии не публикует. Dock `/health/ready` проверяет корректность htpasswd, **не доступность core**; нельзя подписывать его как здоровье всей цепочки. [Live][dock-live], [sessions schema][dock-store], [Dock ready][dock-router]

## Live: пределы существующего механизма

SSE `/api/logs/stream` с auth session выдаёт `logs`, `core_error`, `core_ok`, `lagged`. Один общий poller ищет `[now-lookback,now+60s)` без пользовательских filters/text/cursor, limit 1000; новый подписчик получает до 200 событий, frontend хранит 2000. Следовательно, фильтрация только уже пришедших SSE данных не даёт полного filtered live view: нужный сервис может вытесняться из общей top1000. [Live poll][dock-live-poll], [SSE][dock-sse], [CoreClient][dock-client], [browser rows][dock-logs-merge]

При >1000 новых событий за опрос или доставке позднее lookback события теряются для ленты; это не надёжный tail. `lagged` - пропуски broadcast **пачек**, не точное число потерянных logs. Текущий браузер не подписан на lagged. Core cursor поискового окна не решает вечное tail-слежение за поздними публикациями. Для заявленной полноты нужны tail API по sequence/publication, resume/backfill и контракт gaps; для первой версии интерфейс должен явно обозначать ограниченную live-ленту. [live limitation][dock-live], [poll][dock-live-poll], [lagged event][dock-sse], [browser handlers][dock-logs-merge]

Live дедуплицирует по пользовательскому event.id. Relay принимает валидный supplied UUID, core дедуплицирует пачки по байтам, не отдельные IDs. Повторяющиеся IDs приложений схлопнутся в текущем Live/браузере. Это существующее ограничение, не причина для незапрошенного рефакторинга, но будущий tail стоит адресовать внутренним sequence. [relay supplied id][relay-fill], [Live dedup][dock-live-poll], [browser merge][dock-logs-merge], [journal receipt dedup][core-journal]

## Алерты

Core router и Dock router не содержат alerts API; migrations Dock хранят только sessions/users/passkeys. Нет evaluator, persisted rules/firings, channel configuration, delivery queue/retry, cooldown/recovery/quiet-hours. Условие count можно считать существующим histogram, но автоматическое правило должно работать серверно даже при закрытом браузере. [routes][core-router], [Dock routes][dock-router], [Dock persistence][dock-store], [alert template][template-alerts]

Шаблон запрашивает несколько разных типов правил: count threshold, новый pattern за 30d, pattern growth, отсутствие событий, сравнение с медианой 7d. Их нельзя честно реализовать одним checkbox/UI. Первый серверный этап может включить count threshold и silence; pattern rules зависят от модели паттернов, baseline median - от временных рядов и определения сравнительного окна. Для «silence» нужно отличать транспортный сбой/heartbeat stale от отсутствия бизнес-событий, если это требуется продуктом. [alert rule examples][template-alerts]

Preview «сработало бы 3 раза» требует точной семантики окон (sliding/tumbling), transition detection, cooldown, late arrivals, часового пояса и quiet hours; сумма bucket превышений не обязательно равна числу notifications. Recovery и quiet-hours morning digest требуют persisted state. Доставку каналов следует вводить выбранными пользователем приоритетами; секреты хранить на сервере, не в браузерном состоянии/URL. Исходники текущего проекта не подтверждают ни одну интеграцию Telegram/Slack/Email/Discord/Webhook. [alert behavior][template-alerts]

## Доступ и безопасность интеграции

Все новые proxy endpoints Dock должны требовать `AuthUser`; POST/DELETE/PATCH проходят тот же `require_origin`. Он проверяет Origin у любого non-GET/HEAD/OPTIONS, включая POST-поиск. Cookie HttpOnly/Secure/SameSite Strict; core query/admin токены остаются в backend. Браузеру следует обращаться только к `/api/*`. [AuthUser/cookie/origin][dock-session], [client tokens][dock-client], [router middleware][dock-router]

Сейчас AuthUser содержит username/token_hash, role/permissions отсутствуют. Любой аутентифицированный пользователь может управлять профилями relay/agent через общий admin credential Dock. Надпись `admin` шаблона - пример username, не доказательство RBAC. Если требуется ограничить rules/channels/node admin или дать viewer-only доступ, это отдельный контракт ролей. [AuthUser][dock-session], [access handlers][dock-access], [App user name][dock-app]

Персональные relay/agent secrets видны один раз при создании; профили содержат только metadata, токен хранится core хешем, create response Dock `Cache-Control:no-store`. Перенос раздела «Подключения» в Infra должен сохранить этот flow и запрет удаления relay с агентами. Новые telemetry responses не должны включать token hashes или payload секретов. [registry][core-access], [one-time proxy][dock-access], [delete restriction][core-access-delete]

Строки message/attributes/stacktrace/variable values - недоверенные данные логов: рендерить как текст/React text nodes; подсветку JSON и trace строить без вставки raw HTML. Ссылка из шаблона `href="#"` не является API. Alert webhooks вводят outbound requests и нуждаются в определённой политике URL/redirect/network destinations; это требование новой функциональности, не ограничение текущего query API.

## Рекомендуемые решения до реализации

1. Договориться о первой функциональной поставке: historical logs + базовая count-аналитика + существующие подключения доступны раньше pattern/issue/alert/heartbeat.
2. Утвердить общий query model: диапазон, exact filters, message text; lower-case levels; границы ограниченного DSL; URL-state. Метрики для text-запроса должны использовать тот же предикат или показываться отдельно с явным scope.
3. Для таблицы и графиков выбрать строго общий snapshot или независимые ответы с показом `as_of`; не выдавать top-k за полный facet и page length за total.
4. Решить identity relay/agent/source **до** мониторинга и heartbeat; не использовать совпадение имён как join key.
5. Утвердить источник duration/span/exception/deployment metadata и модель паттернов с версией fingerprint. Числовые p95 и waterfall нельзя получить из текущего JSON без новых соглашений.
6. Для alert этапа выбрать первый тип правила и канал, semantics окна/late logs/cooldown и retention истории. 30d new-pattern baseline несовместим с default 7d raw retention без отдельного состояния/агрегатов.

Проверки для реализации: contract/integration tests proxy filters/cursor/400/410/503/504 и Origin/session; точность total/histogram/series при text-фильтре, null facet и >1000 групп; сохранение server order; export completeness и TTL; heartbeat без логов/после рестарта/при spool backlog; issue reopen и alert transitions на истории. В этой read-only проверке тесты не запускались: выводы подтверждены кодом.

[core-router]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/api.rs:28
[dock-router]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/api.rs:110
[dock-client]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/core_client.rs:14
[query-request]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:24
[query-filter]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:85
[query-range]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:287
[index-fields]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/index.rs:18
[query-level]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:145
[wire-level]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/wire.rs:151
[relay-level]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-relay/src/normalize.rs:174
[query-search]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:181
[index-tokenizer]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/index.rs:61
[query-response]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:57
[query-pagination]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:239
[query-topdocs]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:446
[query-cursor]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/query.rs:204
[storage-publish]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/storage.rs:370
[core-query-errors]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/api.rs:205
[storage-runtime]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/storage.rs:63
[core-config]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/config.rs:48
[analytics-requests]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/analytics.rs:24
[analytics-histogram]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/analytics.rs:71
[analytics-group]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/analytics.rs:133
[analytics-frame]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/analytics.rs:227
[core-analytics-run]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/api.rs:74
[wire-event]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/wire.rs:40
[core-model]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/model.rs:8
[columns-schema]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/columns.rs:32
[relay-fill]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-relay/src/normalize.rs:121
[core-trace]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/api.rs:138
[core-access]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/access.rs:37
[core-access-name]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/access.rs:233
[core-access-delete]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/access.rs:191
[wire-source]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/wire.rs:30
[agent-config]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-agent/src/config.rs:135
[relay-ingest]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-relay/src/ingest.rs:108
[core-relay-auth]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/api.rs:360
[core-ingest]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/api.rs:232
[relay-agents]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-relay/src/agents.rs:17
[relay-auth]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-relay/src/ingest.rs:31
[agent-sender]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-agent/src/sender.rs:64
[core-metrics]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/metrics.rs:4
[core-metrics-endpoint]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/api.rs:333
[core-journal]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/journal.rs:64
[relay-event]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-relay/src/normalize.rs:64
[storage-retain]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-core/src/storage.rs:466
[dock-live]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/live.rs:1
[dock-live-poll]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/live.rs:129
[dock-sse]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/live.rs:178
[dock-logs-merge]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/web/src/pages/Logs.tsx:14
[dock-store]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/store.rs:12
[dock-session]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/session.rs:132
[dock-access]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/src/access.rs:51
[dock-app]: /Users/liis/.t3/worktrees/Lognara/t3-08da1ce8/Lognara-dock/web/src/App.tsx:40
[template-main-query]: </Users/liis/Projects/Lognara/docs/страницы дизайна/Main.dc.html:95>
[template-main-actions]: </Users/liis/Projects/Lognara/docs/страницы дизайна/Main.dc.html:153>
[template-patterns]: </Users/liis/Projects/Lognara/docs/страницы дизайна/variant-f.dc.html:100>
[template-errors]: </Users/liis/Projects/Lognara/docs/страницы дизайна/errors.dc.html:85>
[template-analytics]: </Users/liis/Projects/Lognara/docs/страницы дизайна/analytics.dc.html:85>
[template-infra]: </Users/liis/Projects/Lognara/docs/страницы дизайна/infra.dc.html:85>
[template-alerts]: </Users/liis/Projects/Lognara/docs/страницы дизайна/alerts.dc.html:80>
