# lognara-core

Центральное хранилище Lognara: HTTP-пачки relay → WAL → Arrow / Parquet + ZSTD → Tantivy / DataFusion.

Самостоятельный crate, Rust 2024, минимальный Rust 1.94. Сборка: `cargo build --release --manifest-path Lognara-core/Cargo.toml`.

## Контракт relay

`POST /v1/batches`, `Content-Type: application/msgpack`, `Content-Encoding: zstd`, `Authorization: Bearer <LOGNARA_INGEST_TOKEN>`.
Формат повторяет `Lognara-relay/src/core_wire.rs`: `CoreBatch { dropped, groups: [{ source, dropped, events }] }`.
Все времена в бинарном протоколе — i64 Unix **наносекунды**. `ingested_at` — время агента, core его не заменяет.
UUID и trace ID — bin 16 байт; span ID — bin 8 байт. Уровень — lowercase строка, включая `unknown`.
Core принимает UUID независимо от версии. JSON API отдаёт RFC3339 UTC и UUID/hex строки.

Успех — `204`, только после fsync WAL, или для уже сохранённой пачки. Дедупликация — SHA-256 **исходного сжатого тела**, а не LogId. Relay повторяет ровно сохранённые байты.
`400/413/415` — окончательная ошибка пачки; `401/403` — исправить доступ; `503` — временный отказ, повторить позже.
Приём целиком, без частичного ACK. Core не подтверждает буфер в RAM.

Лимиты по умолчанию: 64 MiB сжатого тела, 256 MiB распакованного. Relay обязан разделять исходящие пачки по этим лимитам. Текущий relay ограничивает число событий, поэтому чрезмерно большая исходящая пачка может получить окончательный `413`.

Проверка совместимости использует настоящий encoder relay через dev-dependency. Fixture воспроизводится командой:

```sh
cd Lognara-core
cargo run --example relay_fixture -- tests/fixtures/relay-batch.bin
```
