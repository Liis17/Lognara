# lognara-core

Parent: [[Index]]

## Назначение и состояние

Центральный Rust-crate `Lognara-core/`, edition 2024, MSRV 1.94. Принимает контракт [[Relay/LognaraRelay]], хранит события и предоставляет API поиска и аналитики.
Реализованы конфигурация, HTTP-приём, бинарная модель, WAL, дедупликация пачек, восстановление и JSON-представление. Перенос в сегменты и запросы добавляются следующими этапами.

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
| `api::router(journal: Arc<Journal>): Router` | Приём, health, авторизованные метрики. |
