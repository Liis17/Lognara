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

Лимиты по умолчанию: 64 MiB сжатого тела, 256 MiB распакованного. Relay делит исходящие пачки по этим лимитам перед первой отправкой. Его `LOGNARA_CORE_MAX_BODY_BYTES` и `LOGNARA_CORE_MAX_DECODED_BYTES` должны быть не больше лимитов core. Уже записанный spool не перекодируется: изменение лимитов не меняет ключи дедупликации старых пачек.

Дополнительно до десериализации проверяется бюджет модели: 256 MiB, `LOGNARA_MAX_MODEL_BYTES` в core и `LOGNARA_CORE_MAX_MODEL_BYTES` в relay. Оценка — 128 байт на MessagePack-узел плюс длины string/bin; превышение даёт 413. Это ограничивает увеличение памяти на больших массивах/объектах `attributes`. Relay учитывает тот же бюджет при разделении.

Проверка совместимости использует настоящий encoder relay через dev-dependency. Fixture воспроизводится командой:

```sh
cd Lognara-core
cargo run --example relay_fixture -- tests/fixtures/relay-batch.bin
```

## Поиск

`POST /v1/logs/search`, JSON, query-токен:

```json
{"from":"2026-09-29T10:00:00Z","to":"2026-09-29T11:00:00Z","filters":{"service":["api"],"level":["error"]},"text":{"query":"connection refused","mode":"phrase"},"limit":100}
```

Ответ: `events`, `next_cursor`, `as_of`. `from <= timestamp < to`, обязательные RFC3339 границы. Фильтры: id, environment, server, backend, service, service_instance, action, level, trace_id, span_id, parent_span_id, request_id. Значения — массивы (OR внутри, AND между полями).
Текст: `all` (по умолчанию) или `phrase`, Unicode, lowercase, без стемминга. Limit 1..1000, по умолчанию 100. Для следующей страницы повторить запрос с `cursor`; limit можно менять. Новые записи в этот обход не включаются. Курсор подписан, живёт 10 минут и становится недействителен после рестарта или retention (`410`).

`GET /v1/traces/{32 hex}?from=...&to=...&limit=100&cursor=...` возвращает такие же события в порядке возрастания времени. Это события trace, не реконструкция длительностей span. Поиск сортирует по убыванию времени; ничьи разрешает внутренний номер события.

Ошибки JSON/фильтров — `400`, истёкший курсор — `410`, занятая ёмкость или ошибка хранилища — `503`, таймаут — `504`. Неподдерживаемые поля не игнорируются.

## Аналитика

`POST /v1/stats/histogram`:

```json
{"from":"2026-09-29T10:00:00Z","to":"2026-09-29T11:00:00Z","filters":{"service":["api"]},"interval_seconds":60}
```

Ответ: `buckets: [{timestamp, count}]`, `as_of`. Корзины выровнены по UTC от Unix epoch; первая может начинаться до `from`, но учитываются только события внутри `[from,to)`. Пустые корзины заполнены нулями. Интервал 1..86400 секунд, по умолчанию 60; не больше 10000 корзин.

`POST /v1/stats/group-by`:

```json
{"from":"2026-09-29T10:00:00Z","to":"2026-09-29T11:00:00Z","group_by":["service","level"],"limit":100}
```

Ответ: `groups: [{values: {service: "api", level: "error"}, count}]`, `as_of`. Разрешены 1–2 разных поля: environment, server, backend, service, service_instance, level, action. Отсутствующее значение — JSON null. Сортировка count DESC, затем ключи группы. Limit 1..1000, по умолчанию 100. Фильтры совпадают с поиском; `text`, `attributes` и SQL отклоняются.

## Запуск и проверка

Настройка, HTTPS-прокси, graceful shutdown, восстановление и метрики: [эксплуатация](docs/operations.md).

```sh
cargo test --manifest-path Lognara-core/Cargo.toml --all-features
cargo fmt --manifest-path Lognara-core/Cargo.toml --check
cargo clippy --manifest-path Lognara-core/Cargo.toml --all-targets --all-features -- -D warnings
```

Feature `crash-tests` включает только аварийные точки для тестовых subprocess. Для эксплуатации собирайте без него. Тесты запускают настоящий relay, проверяют повтор после рестарта, SIGKILL после ACK, сбои публикации, точность данных, индексы, курсоры и retention.

Нагрузочный стенд и критерии: [benchmark](docs/benchmark.md). Нагрузка 10000 событий/с на 4 CPU / 8 GiB — проверяемая цель, а не гарантия для произвольных логов и запросов.
