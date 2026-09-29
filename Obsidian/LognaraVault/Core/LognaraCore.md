# lognara-core

Parent: [[Index]]

## Назначение и состояние

Центральный Rust-crate `Lognara-core/`, edition 2024, MSRV 1.94. Принимает контракт [[Relay/LognaraRelay]], хранит события и предоставляет API поиска и аналитики.
Реализованы конфигурация, бинарная модель relay и JSON-представление события. Конвейер хранения и HTTP добавляются следующими этапами.

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
