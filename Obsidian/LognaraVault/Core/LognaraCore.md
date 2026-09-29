# lognara-core

Parent: [[Index]]

## Назначение и состояние

Центральный Rust-crate `Lognara-core/`, edition 2024, MSRV 1.94. Принимает контракт [[Relay/LognaraRelay]], хранит события и предоставляет API поиска и аналитики.
Реализованы конфигурация, HTTP-приём, бинарная модель, WAL, дедупликация пачек, восстановление и JSON-представление. Реализованы материализация в Arrow/Parquet, каталог SQLite, согласованные снимки и индексы; доступны HTTP-поиск и trace lookup.

## Контракт

MessagePack с именованными полями + ZSTD; `CoreBatch = dropped + groups`, группа = источник + dropped + события.
Время — Unix наносекунды; `ingested_at` принадлежит агенту. Идентификаторы сохраняются без изменения.
Внешний JSON — RFC3339, UUID/hex строки. Дедупликация планируется по байтам пачки, а не отдельным LogId.

## Методы

| Метод | Назначение |
|---|---|
| `Config::from_env(): Result<Config>` | Проверяет переменные `LOGNARA_*`; секреты не печатает. |
| `Config::from_lookup(lookup): Result<Config>` | Собирает конфиг из источника значений. |
| `wire::decode(body: &[u8], limit: usize): Result<CoreBatch>` | Ограничивает распаковку и глубину MessagePack, отклоняет хвост после пачки. |
| `CoreBatch::into_events(first_sequence: u64, received_at: i64): Iterator<StoredEvent>` | Объединяет источник и события, назначает внутренние номера. |
| `LogEvent::from(row: StoredEvent): LogEvent` | Преобразует бинарные поля в JSON API. |

## Зависимости

Tokio, Axum, serde/MessagePack/ZSTD; Parquet 59.2 совместим с Arrow из DataFusion 55.1; Tantivy 0.26, SQLite через rusqlite. Зависимость на relay существует только в тестах и примерах.

## Надёжный приём

WAL хранит одну версионированную запись на файл: заголовок, исходные сжатые байты, CRC32 и SHA-256. Запись *.tmp, fsync, rename в *.wal, fsync каталога, затем 204. Незавершённые *.tmp удаляются, повреждённые опубликованные *.wal останавливают запуск. Один writer и блокировка каталога исключают гонки. HTTP резервирует память до чтения тела.

| Метод | Назначение |
|---|---|
| `Wal::open(dir: &Path, next_batch: u64, next_sequence: u64): Result<Wal>` | Проверяет WAL и восстанавливает номера. |
| `Wal::append(body: &[u8], events: u64, received_at: i64): Result<Receipt>` | Долговечно публикует запись. |
| `Wal::prune(checkpoint: Position): Result<()>` | Удаляет только полностью материализованные пачки. |
| `Journal::open(config: Config): Result<Arc<Journal>>` | Захватывает блокировку и восстанавливает журнал. |
| `Journal::accept(body: &[u8]): Result<Receipt>` | Дедуплицирует, проверяет лимиты, подтверждает после fsync. |
| `~~api::router(journal: Arc<Journal>)~~ (удалён: 2026-09-29)` | Приём, health, авторизованные метрики. |

## Сегменты

Открытый сегмент хранит Arrow-батчи и Tantivy без STORED-копий. Пороги: 250000 строк / 256 MiB Arrow / 300 секунд. Снимок публикуется после commit + reload. При seal сначала долговечно записываются Parquet, индекс и meta.json, затем SQLite FULL-транзакция регистрирует сегмент, receipts и позицию внутри пачки; после неё WAL можно удалить. Незарегистрированные каталоги при старте удаляются, данные восстанавливаются из WAL. Индекс восстанавливается из Parquet по запросу, повреждённый сегмент не скрывается.

| Метод | Назначение |
|---|---|
| `Core::open(config: Config): Result<Arc<Core>>` | Восстанавливает хранилище, запускает материализацию. |
| `Core::snapshot(): Arc<Snapshot>` | Даёт согласованное представление Arrow и закрытых сегментов. |
| `Core::shutdown(): Result<()>` | Дорабатывает WAL, закрывает открытый сегмент. |
| `columns::encode(rows: &[StoredEvent]): Result<RecordBatch>` | Строит Arrow по фиксированной схеме. |
| `columns::decode(batch: &RecordBatch): Result<Vec<StoredEvent>>` | Восстанавливает события, включая attributes. |
| `columns::read_rows(path: &Path, ids: &[usize]): Result<Vec<StoredEvent>>` | Читает выбранные строки Parquet через RowSelection. |
| `Catalog::publish(segment, checkpoint, receipts, next_ids): Result<()>` | Атомарно фиксирует материализацию и дедупликацию. |
| `IndexCache::searcher(segment: &Path, rows: usize): Result<Searcher>` | Открывает или восстанавливает индекс; LRU ограничен. |

Структура файлов: [[Core/LognaraCore-ProjectMap]].

## Поиск

`POST /v1/logs/search`: from/to RFC3339, filters с массивами значений, text (all/phrase), limit, cursor. `GET /v1/traces/{trace_id}`: те же границы, ASC. Сортировка timestamp + sequence. Tantivy выдаёт ограниченный top-k каждого сегмента, глобальный буфер тоже ограничен limit+1. Полные события читаются из Arrow или выбранных строк Parquet. HMAC-курсор фиксирует watermark, параметры и поколение retention; TTL 10 минут, после рестарта 410.

| Метод | Назначение |
|---|---|
| `api::router(core: Arc<Core>): Router` | Собирает HTTP приёма и чтения с отдельными токенами. |
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
