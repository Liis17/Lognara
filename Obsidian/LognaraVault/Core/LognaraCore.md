# lognara-core

Parent: [[Index]]

## Назначение и состояние

Центральный Rust-crate `Lognara-core/`, edition 2024, MSRV 1.94. Принимает контракт [[Relay/LognaraRelay]], хранит события и предоставляет API поиска и аналитики.
Реализованы конфигурация, HTTP-приём, бинарная модель, WAL, дедупликация пачек, восстановление и JSON-представление. Реализованы материализация в Arrow/Parquet, каталог SQLite, согласованные снимки и индексы; доступны HTTP-поиск, trace lookup, статистика и графики метрик (раздел «Метрики»).

## Контракт

MessagePack с именованными полями + ZSTD; `CoreBatch = dropped + groups`, группа = источник + dropped + события.
Время — Unix наносекунды; `ingested_at` принадлежит агенту. Идентификаторы сохраняются без изменения.
Внешний JSON — RFC3339, UUID/hex строки. Дедупликация реализована по байтам пачки, а не отдельным LogId. Relay делит новые исходящие пачки по согласованным лимитам 64/256 MiB и сохраняет байты при повторе.

## Методы

| Метод | Назначение |
|---|---|
| `Config::from_env(): Result<Config>` | Проверяет переменные `LOGNARA_*`; секреты не печатает. |
| `Config::from_lookup(lookup): Result<Config>` | Собирает конфиг из источника значений. |
| `wire::decode(body: &[u8], limit: usize): Result<CoreBatch>` | Ограничивает распаковку и глубину MessagePack, отклоняет хвост после пачки. |
| `wire::decode_with_budget(body: &[u8], limit: usize, model_limit: usize): Result<CoreBatch>` | До serde проверяет размер модели, исключая увеличение памяти на вложенных attributes. |
| `wire_budget::validate(bytes: &[u8], budget: usize): Result<()>` | Без аллокаций проверяет MessagePack и резервирует 128 байт/узел плюс длины string/bin. |
| `Config::ingest_request_bytes(): usize` | Резерв body + две decoded capacity + модель для admission. |
| `CoreBatch::into_events(first_sequence: u64, received_at: i64): Iterator<StoredEvent>` | Объединяет источник и события, назначает внутренние номера. |
| `LogEvent::from(row: StoredEvent): LogEvent` | Преобразует бинарные поля в JSON API. |

## Зависимости

Tokio, Axum, serde/MessagePack/ZSTD; rand 0.9 для системной генерации токенов; Parquet 59.2 совместим с Arrow из DataFusion 55.1; Tantivy 0.26, SQLite через rusqlite. Зависимость на relay существует только в тестах и примерах, на agent только в сквозном тесте.

## Надёжный приём

WAL хранит одну версионированную запись на файл: заголовок, исходные сжатые байты, CRC32 и SHA-256. Запись *.tmp, fsync, rename в *.wal, fsync каталога, затем 204. Незавершённые *.tmp удаляются, повреждённые опубликованные *.wal останавливают запуск. Один writer и блокировка каталога исключают гонки. HTTP резервирует память до чтения тела.

По умолчанию ingest budget 1 GiB, model budget 256 MiB; паритет проверки с relay закреплён тестом. До 64 запросов ждут слот максимум секунду, затем 503. Транспортная ошибка чтения тела даёт 503, именно превышение размера — 413. Метаданные очереди дополнительно ограничены 65536 WAL-пачками. Worker опрашивает WAL каждые 20 мс.

| Метод | Назначение |
|---|---|
| `Wal::open(dir: &Path, next_batch: u64, next_sequence: u64): Result<Wal>` | Проверяет WAL и восстанавливает номера. |
| `Wal::append(body: &[u8], events: u64, received_at: i64): Result<Receipt>` | Долговечно публикует запись. |
| `Wal::prune(checkpoint: Position): Result<()>` | Удаляет только полностью материализованные пачки. |
| `Wal::pending_batches(): usize` | Число пачек для ограничения памяти метаданных очереди. |
| `Journal::open(config: Config): Result<Arc<Journal>>` | Захватывает core.lock, открывает Access и восстанавливает журнал. |
| `Journal::accept(body: &[u8]): Result<Receipt>` | Дедуплицирует, проверяет лимиты, подтверждает после fsync. |
| `~~api::router(journal: Arc<Journal>)~~ (удалён: 2026-09-29)` | Приём, health, авторизованные метрики. |

## Сегменты

Открытый сегмент хранит Arrow-батчи и Tantivy без STORED-копий. Пороги: 250000 строк / 256 MiB Arrow / 300 секунд. Снимок публикуется после commit + reload. При seal сначала долговечно записываются Parquet, индекс и meta.json, затем SQLite FULL-транзакция регистрирует сегмент, файл точек метрик, имена серий, receipts и позицию внутри пачки; после неё WAL можно удалить. Незарегистрированные каталоги при старте удаляются, данные восстанавливаются из WAL. Индекс восстанавливается из Parquet по запросу, повреждённый сегмент не скрывается.

### Слияние сегментов

При ~10 событиях/с сегмент закрывается по возрасту (~3000 строк, 288 в сутки), поэтому поток `lognara-compaction` (`Lognara-core/src/compaction.rs`) сливает закрытые сегменты одного окна `LOGNARA_SEGMENT_COMPACT_SECONDS` (по умолчанию сутки UTC) в один. Окно сегмента задаёт `max_received_at`: он не убывает по sequence, и группа всегда идёт подряд. Окно готово через `2 × LOGNARA_SEGMENT_SECONDS` после конца, без активного сегмента в нём и только после обработки его строк каталогом паттернов: иначе `next_chunk` получил бы весь слитый сегмент одним проходом. Граница группы — 20 млн строк; одиночный сегмент не переписывается, поздний сегмент слитого окна сливается повторно.

Поток пишет новый каталог (Parquet ZSTD 9 потоково из источников в порядке sequence, индекс `index::build`, `meta.json`) и передаёт его материализатору по каналу, поэтому материализация и видимость логов не ждут слияния. Материализатор проверяет, что источники ещё не удалены retention (иначе снимает новый каталог), и `Catalog::replace_segments` одной транзакцией делает новый сегмент `ready`, источники `deleting`; удаление файлов ждёт освобождения снимков. Поколение курсоров не меняется: строки и их (timestamp, sequence) те же. Восстановление при старте покрывает оба сбоя существующими правилами (незарегистрированный каталог, `deleting`). Тесты: `tests/compaction.rs`, точки `segment_compact_after_parquet`/`segment_compact_after_mark` в `tests/crashes.rs`.

| Метод | Назначение |
|---|---|
| `compaction::select(closed, active, window, quiet, now, processed, max_rows): Option<Vec<Arc<DiskSegment>>>` | Подряд идущие сегменты самого старого готового окна. |
| `Catalog::replace_segments(new: &SegmentMeta, old_ids: &[String]): Result<()>` | Регистрирует слитый сегмент и переводит источники в `deleting`. |
| `PatternCatalog::compaction_limit(): Option<u64>` | Обработанный sequence; без ограничения при отказе каталога, None до первичного прохода. |

| Метод | Назначение |
|---|---|
| `Core::open(config: Config): Result<Arc<Core>>` | Восстанавливает хранилище, запускает материализацию, потоки агрегатов `lognara-rollups` и слияния `lognara-compaction`. |
| `Core::snapshot(): Arc<Snapshot>` | Даёт согласованное представление Arrow и закрытых сегментов. |
| `Core::shutdown(): Result<()>` | Останавливает потоки агрегатов и слияния, дорабатывает WAL, закрывает открытый сегмент. |
| `Core::wake_rollups(&self): ()` | Будит поток агрегатов после запечатывания сегмента и слияния файлов точек. |
| `columns::encode(rows: &[StoredEvent]): Result<RecordBatch>` | Строит Arrow по фиксированной схеме. |
| `columns::decode(batch: &RecordBatch): Result<Vec<StoredEvent>>` | Восстанавливает события, включая attributes. |
| `columns::read_rows(path: &Path, ids: &[usize]): Result<Vec<StoredEvent>>` | Читает выбранные строки Parquet через RowSelection. |
| `columns::read_rows_with_budget(path, ids, remaining): Result<Vec<StoredEvent>>` | Читает выбранные строки по одной, проверяя бюджет до разбора attributes. |
| `columns::charge_row(batch, row, remaining): Result<()>` | Резервирует память модели и JSON-ответа для строки Arrow/Parquet. |
| `Catalog::publish(segment, series, names, environments, checkpoint, receipts, next_ids): Result<()>` | Атомарно фиксирует сегмент, файл точек, имена серий, окружения серий и дедупликацию. |
| `IndexCache::searcher(segment: &Path, rows: usize): Result<Searcher>` | Открывает или восстанавливает индекс; LRU (по умолчанию 256), полная проверка checksum один раз за процесс и повторно после `invalidate`. |

Структура файлов: [[Core/LognaraCore-ProjectMap]].

## Поиск

`POST /v1/logs/search`: from/to RFC3339, filters с массивами значений, text (all/phrase), limit, cursor. `GET /v1/traces/{trace_id}`: те же границы, ASC. Сортировка timestamp + sequence. Tantivy выдаёт ограниченный top-k каждого сегмента, глобальный буфер тоже ограничен limit+1. Полные события читаются из Arrow или выбранных строк Parquet. HMAC-курсор фиксирует watermark, параметры и поколение retention; TTL 10 минут, после рестарта 410.

Модель и JSON-ответ страницы ограничены `LOGNARA_SEARCH_MEMORY_BYTES` (128 MiB на поиск): проверка перед десериализацией каждой строки, общий бюджет всех сегментов, 503 при превышении без частичного ответа. Search permit удерживается до завершения сериализации ответа, а при HTTP-таймауте остаётся у выполняющегося blocking task.

| Метод | Назначение |
|---|---|
| `api::router(core: Arc<Core>): Router` | Собирает приём relay, query API (логи и серии) и admin API (relay, агенты, `/v1/admin/series/*`). |
| `api::ingest_router(journal: Arc<Journal>): Router` | Приём relay, список его агентов, health и query-метрики. |
| `PreparedFilter::new(from, to, filters): Result<PreparedFilter>` | Проверяет диапазон и допустимые фильтры. |
| `query::search(core, request, ascending, deadline): Result<SearchResponse>` | Ищет в согласованном снимке, возвращает события и курсор. |

## Аналитика и retention

DataFusion объединяет только выбранные по времени Parquet-файлы и Arrow-батчи одного снимка. Общий GreedyMemoryPool — 1 GiB, два запроса одновременно, timeout 30 секунд. SQL извне не принимается. Histogram — 1..86400 секунд, не больше 10000 корзин, UTC floor, нули заполнены; диапазон [from,to). Group-by — одно/два разных измерения инфраструктуры, action или level, count DESC и стабильные ключи, limit 1..1000. У старых histogram/group-by фильтры общие с поиском, текст и attributes не допускаются. Новый summary принимает text и читает числовой duration_ms из JSON без миграции сегментов.

Retention смотрит max core_received_at, исключает expired сегменты из новых снимков, повышает поколение курсоров, затем ждёт Arc-ссылки текущих читателей и удаляет файлы через состояние deleting. При старте незавершённое удаление продолжается. Receipts живут минимум 7 суток и пока живут строки.

| Метод | Назначение |
|---|---|
| `analytics::histogram(core, request): Result<HistogramResponse>` | Считает временные корзины через типизированные выражения DataFusion. |
| `analytics::group_by(core, request): Result<GroupResponse>` | Считает группы по разрешённым измерениям. |
| `PreparedText::new(text: &TextQuery): Result<PreparedText>` | Проверяет и токенизирует общее текстовое условие поиска/аналитики. |
| `PreparedText::matches(message: &str): bool` | Сопоставляет all/phrase с тем же tokenizer, что у Tantivy. |
| `query::search_snapshot(core, snapshot, request, ascending, deadline): Result<SearchResponse>` | Получает страницу из заданного снимка; обычный search использует тот же метод. |
| `summary::summary(core: Arc<Core>, request: SummaryRequest): Result<SummaryResponse>` | Считает согласованную сводку и необязательную первую страницу. |

`POST /v1/stats/summary`: общий Snapshot для total, stacked buckets (семь уровней), levels, top-5 ERROR services, p95 duration_ms/coverage, top-20 action groups и первой страницы DESC. Cursor страницы фиксирует тот же watermark; новый запрос создаёт новый снимок. `duration_ms` читается из старых attributes как конечное неотрицательное JSON-число; строки и отсутствие не измерены, null p95 при measured=0. Метод приблизительный t-digest с линейным масштабированием экстремальных чисел против переполнения центроидов; count action означает события. Данные сканируются потоково, агрегаты выполняются последовательно в общем DataFusion memory pool. Summary сам удерживает analytics permit; при отмене HTTP blocking-поиск удерживает свою Arc-копию permit до завершения. Контракт: [DockSummaryContract](../../../docs/DockSummaryContract.md).

## Паттерны сообщений

`pattern_identity` нормализует сообщение консервативно (v1): заменяются только UUID, IPv4/IPv6, hex-ID от 16 символов с цифрой, значения явно обозначенных ID (`id`, `*_id`, `userId`, `pid`...) и длительности с единицей или ключом `duration/elapsed/latency/took/*_ms`. Слова, числа, HTTP-статусы и коды ошибок остаются литералами; регистр, переносы строк и многострочность сохраняются. Идентичность: `p1_` + SHA-256 версии, environment (null отличается от пустой строки), service, level и частей шаблона. Сообщение свыше 16 KiB или с более чем 64 переменными хешируется целиком (`exact`); preview ограничен 4096 байтами, ID считается до усечения.

`pattern_id` добавлен в Tantivy-индекс как точное поле, поэтому `filters.pattern_id` работает в search/cursor; индексы старой схемы восстанавливаются из Parquet при первом чтении. В summary/histogram/group-by фильтр вычисляется UDF `log_pattern_id`. Raw Arrow/Parquet схема не менялась.

Фоновый поток `lognara-patterns` ведёт производный каталог `patterns.sqlite`: checkpoint sequence, as_of, history_from, признак полноты и lifetime first/last каждого паттерна. Он обрабатывает по одному сегменту после checkpoint и фиксирует метаданные и checkpoint одной транзакцией; повторный запуск не учитывает строки дважды. Пропуск sequence (retention до обработки) помечает историю неполной; удаление каталога вызывает rebuild по оставшимся данным без восстановления удалённой истории. Поток не занимает analytics slot пользователей, ошибки ёмкости делают только API паттернов недоступным.

`POST /v1/patterns/search` и `POST /v1/patterns/{pattern_id}/details` считают counts, прошлое окно, корзины и значения переменных по сырым событиям диапазона до обработанного checkpoint; каталог даёт шаблон, first_seen и новизну. Не готов каталог → 503 `pattern index is not ready`, паттерн не найден в диапазоне → 404, превышение бюджета → 503. Контракт: [DockPatternsContract](../../../docs/DockPatternsContract.md).

| Метод | Назначение |
|---|---|
| `pattern_identity::identify(environment: Option<&str>, service: &str, level: &str, message: &str): Identity` | Вычисляет pattern_id, шаблон, части и значения переменных. |
| `pattern_identity::valid_id(id: &str): bool` | Проверяет формат `p1_` + 64 lowercase hex. |
| `patterns::id_expr(): Expr` | DataFusion-выражение pattern_id для точного фильтра агрегатов. |
| `patterns::search(core: &Core, request: PatternRequest): Result<PatternResponse, QueryError>` | Список паттернов с долями, ростом, новизной и корзинами. |
| `patterns::details(core: &Core, pattern_id: &str, request: DetailsRequest): Result<DetailsResponse, QueryError>` | Top-5 значений переменных, до 3 новейших примеров и метрики паттерна. |
| `PatternCatalog::checkpoint(): Option<u64>` | Обработанный sequence каталога или None до первичного прохода. |

## Проблемы ошибок

`issue_identity` группирует только error и fatal (v1). Fingerprint `f1_` + SHA-256: environment/service/action и `exception.type` с первыми 5 фреймами `exception.stacktrace` без номеров строк, колонок и адресов (`at …`, Python `File "…"` в обратном порядке). Без `exception.type` используется pattern_id сообщения и action. Поле `fingerprint` добавлено в Tantivy как точное, в агрегатах фильтр вычисляет UDF `log_issue_fingerprint`.

Каталог `patterns.sqlite` версии 3 в том же проходе и с тем же checkpoint ведёт таблицу `issues`: scope, тип, фреймы, шаблон первого события и lifetime first/last. Производные таблицы версий 1/2 атомарно сбрасываются и пересобираются из оставшихся raw событий. Лимиты `LOGNARA_PATTERN_MAX_ENTRIES`/`_CATALOG_BYTES` общие для паттернов и проблем; ошибка переполнения сообщает оба лимита и фактические значения. Access-log нормализует время и размер ответа, а ServiceMetrics — время и числовые значения, сохраняя статусы, URL, названия метрик и сервиса. Иначе каждое измерение времени создавало отдельный паттерн. Новые JSON-типы переменных `timestamp`/`number` поддерживаются core и Dock совместно; алгоритм и ID незатронутых сообщений остаются v1. `index::ActiveIndex` фиксирует ревизию нормализации в Tantivy commit payload; `IndexCache` перестраивает старый индекс из Parquet при первом запросе. Ограничения обновления, lifetime-истории и сохранённых ID описаны в [контракте паттернов](../../../docs/DockPatternsContract.md).

`POST /v1/issues/search` получает от Dock решённые и вернувшиеся fingerprint и отдаёт счётчики вкладок, сумму событий статуса и проблемы с count/distinct servers/корзинами. `POST /v1/issues/{fingerprint}/details` отдаёт top-5 серверов и до 3 примеров. `POST /v1/issues/recurrences` по одному сегменту после `after` ищет случаи с timestamp не раньше `since`, независимо от готовности каталога. Контракт: [DockIssuesContract](../../../docs/DockIssuesContract.md).

| Метод | Назначение |
|---|---|
| `issue_identity::fingerprint(environment: Option<&str>, service: &str, action: Option<&str>, level: LogLevel, pattern: &Identity, exception: &Exception): Option<Fingerprint>` | Fingerprint проблемы для error/fatal. |
| `issue_identity::valid_id(id: &str): bool` | Проверяет формат `f1_` + 64 lowercase hex. |
| `Exception::parse(attributes: &str): Exception` | Читает exception.type/stacktrace из JSON атрибутов. |
| `issues::id_expr(): Expr` | DataFusion-выражение fingerprint для точного фильтра агрегатов. |
| `issues::search(core: &Core, request: IssueRequest): Result<IssueResponse, QueryError>` | Проблемы статуса и счётчики вкладок по сырым событиям. |
| `issues::details(core: &Core, fingerprint: &str, request: IssueDetailsRequest): Result<IssueDetails, QueryError>` | Серверы, примеры и метрики проблемы. |
| `issues::recurrences(core: &Core, request: RecurrenceRequest): Result<RecurrenceResponse, QueryError>` | Случаи наблюдаемых проблем после checkpoint Dock. |

## Мониторинг инфраструктуры

Relay с персональным токеном отправляет `POST /v1/heartbeat`; `Access::record_heartbeat(relay_id: i64, heartbeat: RelayHeartbeat): Result<usize>` одной транзакцией сохраняет последний отчёт relay и отчёты агентов с hash, зарегистрированным за ним. `heartbeats` в access.sqlite3 переживает рестарт; более старые отчёты не заменяют новые. Списки admin API добавляют heartbeat и as_of core; до первого отчёта поле может отсутствовать. Относительные возраста ограничены 100*365 днями, перевод в наносекунды выполняется через i128.

`status::status(core: &Core): io::Result<Status>` обслуживает query-защищённый `GET /v1/status`: версия, ready, visible sequence/lag, WAL и лимит, число закрытых сегментов, Parquet, индексы включая активный сегмент, oldest event и настроенный retention, total/available filesystem data_dir. Lag публикации не является p95 доставки; размеры не означают квоту процесса. Размеры закрытого сегмента считаются один раз (`DiskSegment::sizes`), повторно обходится только индекс активного сегмента. Snapshot удерживает сегменты от retention во время подсчёта. Полный [контракт инфраструктуры](../../../docs/DockInfrastructureContract.md).

## Метрики

Точки метрик хранятся в `metrics/YYYY/MM/DD/<id>.parquet` без срока хранения. Материализатор извлекает их при добавлении строк в открытый сегмент из `attributes.Metrics` версии 2 (встроенный формат) и из правил. Правила лежат в `series_definitions`, `RuleSet` пересобирается после каждой записи определения, а один вызов записи строк использует один снимок набора. Поэтому правило действует на новые строки; историю не пересчитывает ([[Design/Metrics]], раздел «Отличия реализации»).

Запрос видит те же данные, что и поиск: Parquet-файлы из `Snapshot.series_files` и открытые точки сегмента. Без определения ряд считается показателем (среднее), если все его точки gauge, иначе приращениями (сумма). Определение задаёт `kind` (`delta`, `cumulative`, `gauge`) и `aggregation`; у `cumulative` всегда сумма приращений.

Необязательные `server` и `environment` сужают запрос сервером и окружением; `GET /v1/series` отдаёт окружения (`environments`) из `series_environments` и открытого сегмента. Для полных суток окна запрос берёт суточный агрегат вместо точек, если он актуален (раздел «Суточные агрегаты»).

Определения уникальны по `(server, service, name)`. `scoped_metrics.rs` хранит индекс источников и серий по серверу; `GET /v1/series?group_by=server` и `GET /v1/series/sources` читают его вместе с открытым снимком. При отсутствии сервера несовместимые эффективные настройки дают `server_required`. Серверный каталог поддерживает фильтры `server`, `service`; `POST /v1/series/lookup` возвращает метаданные выбранных составных ключей. Список источников включает определения без точек; контракт выборки и избранного Dock описан в [MetricsBundle](../../../docs/MetricsBundle.md#выборочные-метаданные-и-избранное).

`metric_import.rs` проверяет переносимые наборы и применяет create/skip/rename/replace атомарно, с проверкой SHA-256 revision каталога. Каталог версии 3 переносит общие определения на известные серверы, удаляет определения без источника и сохраняет историческую идентичность правил. Подробности API, миграции и JSON: [контракт метрик](../../../docs/MetricsBundle.md).

Ошибки разбора и отброшенные точки считаются в `lognara_core_series_*`; построение агрегатов: `lognara_core_series_rollups_built_total` и `lognara_core_series_rollup_errors_total`. Маршруты определений и предпросмотра требуют `LOGNARA_ADMIN_TOKEN`; query-токен даёт только чтение серий. Операционные детали (каталог, слияние, счётчики): `Lognara-core/docs/operations.md`. Структура модулей: [[Core/LognaraCore-ProjectMap]].

| Метод | Назначение |
|---|---|
| `series::schema(): SchemaRef` | Схема Parquet точек: время UTC, источник, `name`, `origin`, `rule_id`, `labels`, `value`, `event_id`. |
| `series::encode(points: &[Point]): Result<RecordBatch>` | Строит RecordBatch точек по схеме серий. |
| `SeriesFileMeta::overlaps(&self, from: i64, to: i64): bool` | Есть ли у файла точки во времени `[from, to)`. |
| `extract::native(row: &StoredEvent): (Vec<Point>, bool)` | Точки из `attributes.Metrics` (SchemaVersion 2); флаг - была ошибка разбора. |
| `extract::rules(row: &StoredEvent, rules: &RuleSet): (Vec<Point>, bool)` | Точки всех правил пары сервер/сервис строки. |
| `extract::rule_point(rule: &CompiledRule, row: &StoredEvent): (Option<Point>, bool)` | Точка одного правила: окружение, подстрока, regex, значение, метки. |
| `definitions::validate(input: &DefinitionInput, others: &[Definition], native: bool): Result<(), String>` | Проверяет имя, длины, уникальность, лимит правил и коллизию с native-метрикой. |
| `definitions::validate_rule(rule: &Rule): Result<Regex, String>` | Лимиты правила, компиляция regex, проверка именованных групп. |
| `RuleSet::load(definitions: &[Definition]): Result<RuleSet>` | Проверяет квоту правил каждого источника ([[Design/Metrics]]) и компилирует включённые правила по сервисам и серверам. |
| `RuleSet::for_source(&self, server: &str, service: &str): &[CompiledRule]` | Правила источника без перебора других серверов; пустой срез, если их нет. |
| `LabelGuard::admit(&mut self, rule_id: &str, labels: &str): bool` | Пропускает до 1000 различных наборов меток на правило за время жизни процесса. |
| `definitions::list(core: &Core): Result<Vec<Definition>, QueryError>` | Все определения из каталога. |
| `definitions::create(core: &Core, input: DefinitionInput): Result<Definition, QueryError>` | Проверка, запись и замена набора правил под одной блокировкой каталога. |
| `definitions::update(core: &Core, id: &str, input: DefinitionInput): Result<Option<Definition>, QueryError>` | Замена определения; `None`, если id нет. |
| `definitions::delete(core: &Core, id: &str): Result<bool, QueryError>` | Удаляет определение и пересобирает правила. |
| `series_query::list(core: Arc<Core>): Result<ListResponse, QueryError>` | `GET /v1/series`: серии каталога и открытого сегмента, окружения (`environments`), определения без точек. |
| `series_query::query(core: Arc<Core>, request: QueryRequest): Result<QueryResponse, QueryError>` | `POST /v1/series/query`: корзины, split, `environment`, приращения для cumulative, суточные агрегаты для полных суток, до 20 серий. |
| `analytics::preview(core: &Core, request: PreviewRequest): Result<PreviewResponse, QueryError>` | Применяет правило к последним строкам сервиса: до 5000 строк, до 50 образцов. |
| `columns::write_parquet_with(path: &Path, schema: SchemaRef, batches: &[RecordBatch], failpoint_name: &str): Result<()>` | Общая запись Parquet для логов и серий. |
| `columns::stream_batches(path: &Path): Result<impl Iterator<Item = Result<RecordBatch>>>` | Потоковое чтение Parquet для слияния дня. |
| `ParquetWriter::create(path: &Path, schema: SchemaRef): Result<ParquetWriter>` | Открывает запись через `.tmp`, ZSTD уровень 3. |
| `ParquetWriter::write(&mut self, batch: &RecordBatch): Result<()>` | Добавляет пачку в файл. |
| `ParquetWriter::finish(self): Result<()>` | Синхронизирует `.tmp` и переименовывает его в итоговый файл. |
| `Catalog::series_files(&self, state: &str): Result<Vec<SeriesFileMeta>>` | Файлы точек по состоянию `ready` или `deleting`. |
| `Catalog::series_names(&self): Result<Vec<SeriesName>>` | Имена серий с первой и последней точкой. |
| `Catalog::series_environments(&self): Result<Vec<SeriesEnvironment>>` | Окружения серий `(service, name, environment)` из `series_environments`. |
| `Catalog::has_native_name(&self, service: &str, name: &str): Result<bool>` | Есть ли у (service, name) native counter или gauge. |
| `Catalog::definitions(&self): Result<Vec<Definition>>` | Определения из `series_definitions`. |
| `Catalog::insert_definition(&self, definition: &Definition): Result<()>` | Добавляет определение. |
| `Catalog::update_definition(&self, definition: &Definition): Result<bool>` | Заменяет определение по id; `false`, если его нет. |
| `Catalog::delete_definition(&self, id: &str): Result<bool>` | Удаляет определение по id. |
| `Catalog::replace_series(&mut self, new: &SeriesFileMeta, old_ids: &[String]): Result<()>` | Одной транзакцией регистрирует слитый файл и переводит исходные в `deleting`. |
| `Catalog::finish_deleting_series(&self, id: &str): Result<()>` | Удаляет строку файла в состоянии `deleting`; вызывается после удаления самого файла. |
| `ActiveSegment::add_points(&mut self, rows: &[StoredEvent], rules: &RuleSet, guard: &mut LabelGuard, metrics: &Metrics): Result<()>` | Извлекает точки пачки, применяет LabelGuard, запоминает окружения серий и обновляет счётчики. |
| `Materializer::seal_series(&self, segment: &ActiveSegment): Result<Option<SeriesFile>>` | Пишет Parquet точек сегмента; без точек файла нет. |
| `Materializer::compact(&mut self): Result<()>` | Сливает один день, в котором все файлы старше порога тишины и их не меньше двух. |

## Суточные агрегаты

Фоновый поток `lognara-rollups` (`Lognara-core/src/rollup.rs`) строит по одним суткам UTC агрегаты из файлов `metrics/` в `rollups/1h/` и `rollups/12h/`; регистрация в таблице `series_rollups` каталога. `Core::open` запускает поток, `Core::shutdown` останавливает его раньше материализатора. Ошибки построения не меняют готовность core. Уровни по возрасту, отпечаток, ночная перепроверка, план чтения и числа: [[Design/Metrics]], раздел «Суточные агрегаты». Тесты: `tests/rollups.rs`, `tests/series_query.rs`, `tests/crashes.rs` (`crashes_in_rollup_builds_leave_one_registered_rollup_after_restart`).

| Метод | Назначение |
|---|---|
| `rollup::schema(): SchemaRef` | Схема агрегата: `bucket`, ключи точки, `sum`, `count`, `min`, `max`, `last`, `last_ts`. |
| `rollup::spawn(core: Arc<Core>): std::io::Result<JoinHandle<()>>` | Запускает поток `lognara-rollups`; ошибка рантайма только логируется. |
| `rollup::usable_rollup(rollups: &[Arc<RollupFile>], snapshot: &Snapshot, day_start: i128, step_seconds: u64): Option<Arc<RollupFile>>` | Агрегат суток, если шаг кратен корзине, в открытом сегменте нет точек суток и отпечаток совпадает. |
| `rollup::resolution(day_end: i128, now: i128, config: &Config): Option<u32>` | Корзина по концу суток: `None` для суток моложе `LOGNARA_SERIES_RAW_DAYS`, 3600 до `LOGNARA_SERIES_HOURLY_DAYS`, иначе 43200. |
| `rollup::day_start(day: Date): i128` | Начало суток UTC в наносекундах без зажатия к i64. |
| `rollup::window(day: Date): (i64, i64)` | Окно суток для чтения файлов серий, границы зажаты в диапазон i64. |
| `rollup::key(day: Date): String` | Ключ суток `YYYY-MM-DD`. |
| `rollup::directory(resolution: u32): &'static str` | Каталог разрешения: `1h` или `12h`. |
| `rollup::candidate_days(files: &[Arc<SeriesFile>]): Result<BTreeSet<Date>>` | Сутки UTC, которые пересекает хотя бы один файл точек. |
| `rollup::fingerprint(files: &[Arc<SeriesFile>], window: (i64, i64)): String` | SHA-256 отсортированного списка `id:rows` файлов, пересекающих окно. |
| `rollup::open_points(snapshot: &Snapshot, day_start: i128): bool` | Есть ли в открытом сегменте точки суток. |
| `rollup::build(core: &Core, snapshot: &Snapshot, day: Date, resolution: u32): Result<Vec<RecordBatch>>` | Агрегирует файлы точек суток с дедупликацией; открытый сегмент не входит. |
| `rollup::remove_if_exists(path: &Path): Result<()>` | Удаляет файл, если он есть, и синхронизирует каталог. |
| `Worker::run(self)` | Цикл: шаг, затем пауза: после построенных суток `LOGNARA_SERIES_ROLLUP_PAUSE_MS`, без работы 60 секунд, 60 секунд после ошибки. |
| `Worker::step(&mut self): Result<Step>` | Восстановление при первом шаге, очистка заменённых файлов, ночная проверка, затем не больше одних суток. |
| `Worker::apply(&mut self, snapshot: &Snapshot, day: Date, target: Option<u32>, digest: String): Result<()>` | Строит агрегат суток (при `None` только снимает старый) и публикует замену. |
| `Worker::write(&self, day: Date, resolution: u32, digest: String, batches: &[RecordBatch]): Result<RollupFile>` | Пишет Parquet через `.tmp` в файл с UUID v7. |
| `Worker::publish(&mut self): ()` | Публикует `Core::rollups` для читателей, порядок по дню и разрешению. |
| `Worker::schedule_nightly(&mut self): ()` | Раз в сутки UTC в час `LOGNARA_SERIES_ROLLUP_HOUR_UTC` ставит все сутки с агрегатом на принудительную пересборку. |
| `Worker::recover(&mut self): Result<()>` | Удаляет файлы `deleting`, снимает строки без файла, удаляет незарегистрированные `.parquet` и `.tmp`. |
| `Worker::finish_deletions(&mut self): Result<()>` | Удаляет заменённые агрегаты, которых не держит ни один читатель. |
| `Catalog::rollups(&self, state: &str): Result<Vec<RollupMeta>>` | Агрегаты в состоянии `ready` или `deleting`. |
| `Catalog::replace_rollups(&mut self, new: Option<&RollupMeta>, retired: &[String]): Result<()>` | Одной транзакцией переводит старые строки в `deleting` и регистрирует новую как `ready`. |
| `Catalog::finish_deleting_rollup(&self, id: &str): Result<()>` | Удаляет строку агрегата в состоянии `deleting` после удаления файла. |
| `series_query::read_buckets(core: &Core, snapshot: &Snapshot, request: &QueryRequest, step: i64, from: i64, to: i64): Result<Vec<RecordBatch>>` | Объединяет строки точек и агрегатов и сворачивает их по корзине, split и серии. |
| `series_query::plan(core: &Core, snapshot: &Snapshot, step_seconds: u64, from: i64, to: i64): (Vec<Arc<RollupFile>>, Vec<(i64, i64)>)` | Делит окно по суткам: агрегаты для полных суток, остальное диапазонами точек. |
| `series_query::raw_rows(core: &Core, snapshot: &Snapshot, request: &QueryRequest, step: i64, ranges: &[(i64, i64)]): Result<Option<DataFrame>>` | Строки точек в диапазонах; `None`, если диапазонов нет. |
| `series_query::rollup_rows(core: &Core, files: &[Arc<RollupFile>], request: &QueryRequest, step: i64): Result<Option<DataFrame>>` | Строки агрегатов той же формы, что точки; `None`, если агрегатов нет. |
| `series_query::merge(rows: DataFrame): Result<DataFrame>` | На корзину, split и серию: суммы складываются, минимум и максимум по значению, последнее значение по времени. |

## Эксплуатация и проверки

`Lognara-core/docs/operations.md` описывает все переменные, запуск за TLS-прокси, остановку, резервную копию всего каталога и восстановление. SIGTERM/SIGINT прекращает HTTP, дорабатывает WAL и закрывает открытый сегмент. Метрики Prometheus защищены query-токеном; health доступен без токена. В примере Compose корневого README каталог `LOGNARA_DATA_DIR` монтируется из `LOGNARA_CORE_DATA` (папка на хосте) или volume `lognara-core-data`.

`tests/crashes.rs` запускает настоящий бинарник с feature `crash-tests`: завершение до rename WAL, после fsync, записи Parquet, до/после SQLite-транзакции, после очистки WAL; отдельно SIGKILL после 204. В production feature выключен. `tests/e2e.rs` запускает настоящие agent, relay и core, проверяет API чтения и доступ; временно останавливает HTTP core, принимает логи через agent с `202`, после восстановления проверяет наличие всех сообщений.

Аварийные проверки также включают остановку до fsync, во время записи Parquet и после пометки retention `deleting`. Двухосевое ревью и исправления описаны в `docs/review.md`: бюджет модели/поиска, классификация транспортных ошибок и ожидание конкурентных повторов.

Стенд `examples/load.rs` генерирует поток и измеряет HTTP ACK, поиск, гистограмму и выборочную видимость от ACK. `scripts/benchmark.py` запускает release-процессы, снимает RSS/CPU/диск/WAL, сохраняет JSON и логи, выполняет graceful shutdown. Настройки и ограничения измерений — `docs/benchmark.md`. Цель 10000/с на 4 CPU / 8 GiB требует проверки именно на этом оборудовании.

Контрольная проверка: 18 001 000 событий за 1800 секунд без ошибок, p99 ACK 21,02 мс, p99 видимости 1,276 с, пик измеренного RSS 426,5 MiB. После рестарта все события доступны аналитике и поиску; 73 сегмента, 8,40 GiB, WAL пуст. Фактический стенд macOS 10 CPU / 16 GiB; целевая приёмка на 4 CPU / 8 GiB не выполнена. Подробности и числовой JSON: `docs/benchmark-results.md`, `docs/benchmark-results.json`. Тесты: 25 core и 40 relay, fmt/clippy прошли.

| Метод | Назначение |
|---|---|
| `Metrics::render(): String` | Сериализует низкокардинальные счётчики/gauges Prometheus, в том числе `lognara_core_series_*`. |
| `main::shutdown()` | Ждёт SIGINT или SIGTERM. |

## Реестр подключений

`access.sqlite3` в `data_dir` хранит relay и агентов отдельно от `catalog.sqlite`. SQLite использует WAL, FULL, foreign keys и отдельный `user_version=1`. Реестр открывается после захвата `core.lock`. Идентификаторы i64 не переиспользуются, `created_at` — RFC3339 UTC.

Токен: 32 системных случайных байта в hex с префиксом `lnr_` для relay или `lna_` для агента. Plaintext выдаётся только при создании или перевыпуске; БД хранит SHA-256. Имя после trim: 1..64 символа, без управляющих. Relay-имена уникальны глобально, имена агентов внутри relay. Relay с агентами удалить нельзя.

POST `/v1/admin/agents/{id}/rotate-token` с admin Bearer, без тела, возвращает 200 `{agent, token}` с `Cache-Control: no-store`, неизвестный ID — 404 `not_found`. `Access::rotate_agent_token` одной транзакцией заменяет только token_hash; ID, имя, relay, created_at и heartbeat сохраняются. После commit новый хеш виден в `/v1/agents`, старый отсутствует; relay применяет замену при следующей успешной синхронизации (обычный интервал 10 секунд, при недоступном core работает по кешу). Повтор запроса выпускает ещё один новый токен; автоматические повторы клиента не используются.

`LOGNARA_ADMIN_TOKEN` обязателен и отличается от `LOGNARA_QUERY_TOKEN`; `LOGNARA_INGEST_TOKEN` удалён. Admin API: GET/POST `/v1/admin/relays`, `/v1/admin/agents`, DELETE по `/{id}`. SQLite вызывается через `spawn_blocking`. Ошибки JSON `{"error":"code"}`: invalid_name/invalid_request (400), relay_not_found/not_found (404), name_taken/relay_has_agents (409).

POST `/v1/batches` и GET `/v1/agents` проверяют SHA-256 персонального relay-токена по RwLock-карте до чтения тела. Один Bearer-заголовок, схема без учёта регистра. Relay получает только хеши своих агентов в `{"agents":["sha256 hex"]}`. После удаления relay карта обновляется и токен даёт 401. Поля источника не сверяются.

| Метод | Назначение |
|---|---|
| `Access::open(path: &Path): Result<Access>` | Открывает реестр и карту relay-токенов. |
| `Access::create_relay(name: &str): Result<CreatedRelay>` | Создаёт relay и один раз выдаёт токен. |
| `Access::create_agent(name: &str, relay_id: i64): Result<CreatedAgent>` | Создаёт агента для существующего relay. |
| `Access::list_relays(): Result<Vec<Relay>>` | Возвращает профили без токенов. |
| `Access::list_agents(): Result<Vec<Agent>>` | Возвращает агентов без токенов. |
| `Access::delete_relay(id: i64): Result<()>` | Удаляет relay без агентов и отзывает доступ. |
| `Access::delete_agent(id: i64): Result<()>` | Удаляет агента из списка relay. |
| `Access::relay_for_token(hash: &[u8;32]): Option<i64>` | Проверяет хеш без SQLite. |
| `Access::agent_hashes(relay_id: i64): Result<Vec<String>>` | Возвращает хеши агентов relay. |
| `api::authorize_relay(access, request, next): Response` | Проверяет токен и добавляет id relay в extensions. |

`tests/access.rs`: CRUD, права, изоляция, отзыв и восстановление реестра. Нагрузочный скрипт задаёт admin-токен, регистрирует relay и передаёт его токен load через `LOGNARA_CORE_TOKEN`.

Каталог паттернов и агрегаты метрик работают отдельными фоновыми потоками. `pattern_catalog::next_chunk` сохраняет ограниченный снимок raw-логов без `series_files`: серии метрик не участвуют в идентичности паттернов и проблем.
