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

Tokio, Axum, serde/MessagePack/ZSTD; Parquet 59.2 совместим с Arrow из DataFusion 55.1; Tantivy 0.26, SQLite через rusqlite. Зависимость на relay существует только в тестах и примерах, на agent только в сквозном тесте.

## Надёжный приём

WAL хранит одну версионированную запись на файл: заголовок, исходные сжатые байты, CRC32 и SHA-256. Запись *.tmp, fsync, rename в *.wal, fsync каталога, затем 204. Незавершённые *.tmp удаляются, повреждённые опубликованные *.wal останавливают запуск. Один writer и блокировка каталога исключают гонки. HTTP резервирует память до чтения тела.

По умолчанию ingest budget 1 GiB, model budget 256 MiB; паритет проверки с relay закреплён тестом. До 64 запросов ждут слот максимум секунду, затем 503. Транспортная ошибка чтения тела даёт 503, именно превышение размера — 413. Метаданные очереди дополнительно ограничены 65536 WAL-пачками. Worker опрашивает WAL каждые 20 мс.

| Метод | Назначение |
|---|---|
| `Wal::open(dir: &Path, next_batch: u64, next_sequence: u64): Result<Wal>` | Проверяет WAL и восстанавливает номера. |
| `Wal::append(body: &[u8], events: u64, received_at: i64): Result<Receipt>` | Долговечно публикует запись. |
| `Wal::prune(checkpoint: Position): Result<()>` | Удаляет только полностью материализованные пачки. |
| `Wal::pending_batches(): usize` | Число пачек для ограничения памяти метаданных очереди. |
| `Journal::open(config: Config): Result<Arc<Journal>>` | Захватывает блокировку и восстанавливает журнал. |
| `Journal::accept(body: &[u8]): Result<Receipt>` | Дедуплицирует, проверяет лимиты, подтверждает после fsync. |
| `~~api::router(journal: Arc<Journal>)~~ (удалён: 2026-09-29)` | Приём, health, авторизованные метрики. |

## Сегменты

Открытый сегмент хранит Arrow-батчи и Tantivy без STORED-копий. Пороги: 250000 строк / 256 MiB Arrow / 300 секунд. Снимок публикуется после commit + reload. При seal сначала долговечно записываются Parquet, индекс и meta.json, затем SQLite FULL-транзакция регистрирует сегмент, файл точек метрик, имена серий, receipts и позицию внутри пачки; после неё WAL можно удалить. Незарегистрированные каталоги при старте удаляются, данные восстанавливаются из WAL. Индекс восстанавливается из Parquet по запросу, повреждённый сегмент не скрывается.

| Метод | Назначение |
|---|---|
| `Core::open(config: Config): Result<Arc<Core>>` | Восстанавливает хранилище, запускает материализацию и поток агрегатов `lognara-rollups`. |
| `Core::snapshot(): Arc<Snapshot>` | Даёт согласованное представление Arrow и закрытых сегментов. |
| `Core::shutdown(): Result<()>` | Останавливает поток агрегатов, дорабатывает WAL, закрывает открытый сегмент. |
| `Core::wake_rollups(&self): ()` | Будит поток агрегатов после запечатывания сегмента и слияния файлов точек. |
| `columns::encode(rows: &[StoredEvent]): Result<RecordBatch>` | Строит Arrow по фиксированной схеме. |
| `columns::decode(batch: &RecordBatch): Result<Vec<StoredEvent>>` | Восстанавливает события, включая attributes. |
| `columns::read_rows(path: &Path, ids: &[usize]): Result<Vec<StoredEvent>>` | Читает выбранные строки Parquet через RowSelection. |
| `columns::read_rows_with_budget(path, ids, remaining): Result<Vec<StoredEvent>>` | Читает выбранные строки по одной, проверяя бюджет до разбора attributes. |
| `columns::charge_row(batch, row, remaining): Result<()>` | Резервирует память модели и JSON-ответа для строки Arrow/Parquet. |
| `Catalog::publish(segment, series, names, environments, checkpoint, receipts, next_ids): Result<()>` | Атомарно фиксирует сегмент, файл точек, имена серий, окружения серий и дедупликацию. |
| `IndexCache::searcher(segment: &Path, rows: usize): Result<Searcher>` | Открывает или восстанавливает индекс; LRU ограничен. |

Структура файлов: [[Core/LognaraCore-ProjectMap]].

## Поиск

`POST /v1/logs/search`: from/to RFC3339, filters с массивами значений, text (all/phrase), limit, cursor. `GET /v1/traces/{trace_id}`: те же границы, ASC. Сортировка timestamp + sequence. Tantivy выдаёт ограниченный top-k каждого сегмента, глобальный буфер тоже ограничен limit+1. Полные события читаются из Arrow или выбранных строк Parquet. HMAC-курсор фиксирует watermark, параметры и поколение retention; TTL 10 минут, после рестарта 410.

Модель и JSON-ответ страницы ограничены `LOGNARA_SEARCH_MEMORY_BYTES` (128 MiB на поиск): проверка перед десериализацией каждой строки, общий бюджет всех сегментов, 503 при превышении без частичного ответа. Search permit удерживается до завершения сериализации ответа, а при HTTP-таймауте остаётся у выполняющегося blocking task.

| Метод | Назначение |
|---|---|
| `api::router(core: Arc<Core>): Router` | Собирает HTTP приёма и чтения с отдельными токенами; при `LOGNARA_ADMIN_TOKEN` добавляет `/v1/admin/series/*`. |
| `api::ingest_router(journal: Arc<Journal>): Router` | Приём, health и метрики. |
| `PreparedFilter::new(from, to, filters): Result<PreparedFilter>` | Проверяет диапазон и допустимые фильтры. |
| `query::search(core, request, ascending, deadline): Result<SearchResponse>` | Ищет в согласованном снимке, возвращает события и курсор. |

## Аналитика и retention

DataFusion объединяет только выбранные по времени Parquet-файлы и Arrow-батчи одного снимка. Общий GreedyMemoryPool — 1 GiB, два запроса одновременно, timeout 30 секунд. SQL извне не принимается. Histogram — 1..86400 секунд, не больше 10000 корзин, UTC floor, нули заполнены; диапазон [from,to). Group-by — одно/два разных измерения инфраструктуры, action или level, count DESC и стабильные ключи, limit 1..1000. Фильтры общие с поиском, текст и attributes не допускаются.

Retention смотрит max core_received_at, исключает expired сегменты из новых снимков, повышает поколение курсоров, затем ждёт Arc-ссылки текущих читателей и удаляет файлы через состояние deleting. При старте незавершённое удаление продолжается. Receipts живут минимум 7 суток и пока живут строки.

| Метод | Назначение |
|---|---|
| `analytics::histogram(core, request): Result<HistogramResponse>` | Считает временные корзины через типизированные выражения DataFusion. |
| `analytics::group_by(core, request): Result<GroupResponse>` | Считает группы по разрешённым измерениям. |

## Метрики

Точки метрик хранятся в `metrics/YYYY/MM/DD/<id>.parquet` без срока хранения. Материализатор извлекает их при добавлении строк в открытый сегмент из `attributes.Metrics` версии 2 (встроенный формат) и из правил. Правила лежат в `series_definitions`, `RuleSet` пересобирается после каждой записи определения, а один вызов записи строк использует один снимок набора. Поэтому правило действует на новые строки; историю не пересчитывает ([[Design/Metrics]], раздел «Отличия реализации»).

Запрос видит те же данные, что и поиск: Parquet-файлы из `Snapshot.series_files` и открытые точки сегмента. Без определения ряд считается показателем (среднее), если все его точки gauge, иначе приращениями (сумма). Определение задаёт `kind` (`delta`, `cumulative`, `gauge`) и `aggregation`; у `cumulative` всегда сумма приращений.

Необязательный `environment` в запросе сужает серию одним окружением; `GET /v1/series` отдаёт окружения (`environments`) из `series_environments` и открытого сегмента. Для полных суток окна запрос берёт суточный агрегат вместо точек, если он актуален (раздел «Суточные агрегаты»).

Ошибки разбора и отброшенные точки считаются в `lognara_core_series_*`; построение агрегатов: `lognara_core_series_rollups_built_total` и `lognara_core_series_rollup_errors_total`. Маршруты определений и предпросмотра включаются только с `LOGNARA_ADMIN_TOKEN`; query-токен даёт только чтение серий. Операционные детали (каталог, слияние, счётчики): `Lognara-core/docs/operations.md`. Структура модулей: [[Core/LognaraCore-ProjectMap]].

| Метод | Назначение |
|---|---|
| `series::schema(): SchemaRef` | Схема Parquet точек: время UTC, источник, `name`, `origin`, `rule_id`, `labels`, `value`, `event_id`. |
| `series::encode(points: &[Point]): Result<RecordBatch>` | Строит RecordBatch точек по схеме серий. |
| `SeriesFileMeta::overlaps(&self, from: i64, to: i64): bool` | Есть ли у файла точки во времени `[from, to)`. |
| `extract::native(row: &StoredEvent): (Vec<Point>, bool)` | Точки из `attributes.Metrics` (SchemaVersion 2); флаг - была ошибка разбора. |
| `extract::rules(row: &StoredEvent, rules: &RuleSet): (Vec<Point>, bool)` | Точки всех правил сервиса строки. |
| `extract::rule_point(rule: &CompiledRule, row: &StoredEvent): (Option<Point>, bool)` | Точка одного правила: окружение, подстрока, regex, значение, метки. |
| `definitions::validate(input: &DefinitionInput, others: &[Definition], native: bool): Result<(), String>` | Проверяет имя, длины, уникальность, лимит правил и коллизию с native-метрикой. |
| `definitions::validate_rule(rule: &Rule): Result<Regex, String>` | Лимиты правила, компиляция regex, проверка именованных групп. |
| `RuleSet::load(definitions: &[Definition]): Result<RuleSet>` | Компилирует включённые правила по сервисам. |
| `RuleSet::for_service(&self, service: &str): &[CompiledRule]` | Правила сервиса; пустой срез, если их нет. |
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
