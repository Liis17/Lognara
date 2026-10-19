# lognara-core

Центральное хранилище Lognara: HTTP-пачки relay → WAL → Arrow / Parquet + ZSTD → Tantivy / DataFusion.

Самостоятельный crate, Rust 2024, минимальный Rust 1.94. Сборка: `cargo build --release --manifest-path Lognara-core/Cargo.toml`.

## Контракт relay

`POST /v1/batches`, `Content-Type: application/msgpack`, `Content-Encoding: zstd`, `Authorization: Bearer <персональный токен relay из dock>`.
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

## Профили relay и агентов

Core запускается с обязательными разными `LOGNARA_ADMIN_TOKEN` и `LOGNARA_QUERY_TOKEN`. Dock использует admin-токен для страницы «Подключения» и определений метрик. Реестр хранится в отдельном `access.sqlite3` внутри каталога данных, WAL/FULL, под общей блокировкой `core.lock`. Plaintext возвращается только в ответе создания или перевыпуска токена; в БД хранится SHA-256.

| Метод и путь | Токен | Тело / ответ |
|---|---|---|
| `GET /v1/admin/relays` | admin | `{"relays":[{"id":1,"name":"relay","created_at":"RFC3339"}]}` |
| `POST /v1/admin/relays` | admin | `{"name":"relay"}` → `201 {"relay":{…},"token":"lnr_…"}` |
| `DELETE /v1/admin/relays/{id}` | admin | `204`, `404 not_found`, `409 relay_has_agents` |
| `GET /v1/admin/agents` | admin | `{"agents":[{"id":1,"name":"app","relay_id":1,"created_at":"RFC3339"}]}` |
| `POST /v1/admin/agents` | admin | `{"name":"app","relay_id":1}` → `201 {"agent":{…},"token":"lna_…"}` |
| `POST /v1/admin/agents/{id}/rotate-token` | admin | Без тела → `200 {"agent":{…},"token":"lna_…"}`, `Cache-Control: no-store`; `404 not_found` |
| `DELETE /v1/admin/agents/{id}` | admin | `204`, `404 not_found` |
| `POST /v1/heartbeat` | relay | JSON отчёт relay и его агентов → 204; hash агента сопоставляется только внутри relay |
| `GET /v1/status` | query | ready/version/lag/WAL и текущий filesystem storage |
| `GET /v1/agents` | relay | `{"agents":["sha256 hex",…]}` только для этого relay |

Имя после trim: 1..64 символа, без управляющих. Имя relay уникально глобально, агента — внутри relay. Ошибки JSON `{"error":"code"}`: `400 invalid_name/invalid_request`, `404 relay_not_found/not_found`, `409 name_taken/relay_has_agents`. Неизвестный или удалённый токен relay даёт `401`; admin/query-токены не дают права отправлять пачки. Поля `service/server/backend` не сверяются. Регистрируйте каждый процесс отдельно; потерянный токен требует пересоздания профиля.

Прежний `LOGNARA_INGEST_TOKEN` удалён. Relay получает свой токен через `LOGNARA_CORE_TOKEN`; agent получает свой через `LOGNARA_RELAY_TOKEN`. Примеры регистрации — в [корневом README](../README.md#быстрый-старт).

Admin-списки добавляют `as_of` по часам core и последний `heartbeat` при наличии. Отчёты сохраняются в `access.sqlite3`, старые не заменяют новые. `GET /v1/status` считает размеры Parquet/индексов (включая активный), WAL и место filesystem `data_dir`; lag публикации не является p95 доставки. [Контракт инфраструктуры](../docs/DockInfrastructureContract.md).

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

`POST /v1/stats/summary` принимает те же `from/to/filters/text`, `interval_seconds` и необязательный `search_limit` 1..1000. Возвращает `total`, stacked `buckets` по семи уровням, `levels`, до 5 `error_services`, `duration`, до 20 `actions`, `page` и общий `as_of`. Все части используют один Snapshot; первая страница и её cursor сохраняют тот же watermark. Догрузка страниц не обновляет сводку.

`duration` содержит приблизительный `p95_ms` (t-digest), `measured`, `coverage` (0..1) и `method: "tdigest"`. Из существующих `attributes.duration_ms` принимаются только конечные неотрицательные JSON-числа; нет измерений — `p95_ms: null`. `actions[].count` означает события, не вызовы. Raw-сегменты не мигрируются. Summary занимает один analytics slot и использует общий memory pool; данные не собираются целиком в память. Полный wire-контракт: [DockSummaryContract](../docs/DockSummaryContract.md).
## Определения метрик

Маршруты доступны только с `LOGNARA_ADMIN_TOKEN` (Bearer).

- `GET /v1/admin/series/definitions` возвращает `{"definitions": [...]}` в порядке service, name.
- `POST /v1/admin/series/definitions` отвечает `201`, `PUT /v1/admin/series/definitions/{id}` — `200`, `DELETE` — `204`; неизвестный id — `404`.
- `POST /v1/admin/series/preview` с `service`, `environment`, `rule` и `minutes` (1..60, по умолчанию 60) возвращает `scanned`, `matched` и до 50 последних `samples`.

Определение без `rule` описывает native-метрику с тем же service и name. С `rule` core извлекает точку из `message`: `contains` — подстрока, `pattern` — регулярное выражение, `value_group` — именованная группа значения (без неё значение 1), `label_groups` — именованные группы меток. Правило действует на строки, материализованные после изменения; историю не пересчитывает. Точки хранятся сырыми, `kind` и `aggregation` применяются при запросе.

## Графики метрик

Маршруты доступны с токеном запросов (Bearer), как поиск и статистика.

- `GET /v1/series` возвращает `{"metrics": [...]}`: по записи на (service, name, origin) с `first_seen`, `last_seen` в RFC3339, `definition` и `environments` (отсортированные окружения точек по всем origin, `[]` если окружений нет). Определения без точек тоже в списке, даты в них `null`.
- `POST /v1/series/query` с `service`, `name`, `from`, `to`, `step_seconds` (1..604800), `split` (`none`, `instance` или `labels`, по умолчанию `none`) и необязательными `origin` и `environment` (без `environment` - сумма по всем окружениям) возвращает `kind`, `aggregation`, `unit`, `buckets`, до 20 серий `series` с `values` (`null` без данных) и `as_of`. Корзин не больше 10000. У `cumulative` значения - приращения; первое приращение считается от предыдущей корзины. Сутки окна читаются суточными агрегатами вместо точек, если шаг кратен их корзине и агрегат актуален (см. [эксплуатация](docs/operations.md)).

## Запуск и проверка

Настройка, HTTPS-прокси, graceful shutdown, восстановление и метрики: [эксплуатация](docs/operations.md).

```sh
cargo test --manifest-path Lognara-core/Cargo.toml --all-features
cargo fmt --manifest-path Lognara-core/Cargo.toml --check
cargo clippy --manifest-path Lognara-core/Cargo.toml --all-targets --all-features -- -D warnings
```

Feature `crash-tests` включает только аварийные точки для тестовых subprocess. Для эксплуатации собирайте без него. Тесты запускают настоящий relay, проверяют повтор после рестарта, SIGKILL после ACK, сбои публикации, точность данных, индексы, курсоры и retention.

Нагрузочный стенд и критерии: [benchmark](docs/benchmark.md). Нагрузка 10000 событий/с на 4 CPU / 8 GiB — проверяемая цель, а не гарантия для произвольных логов и запросов.

[Контрольный прогон](docs/benchmark-results.md): 18 001 000 событий за 30 минут, без ошибок, p99 видимости 1,276 с; фактический хост macOS 10 CPU / 16 GiB. Целевой Linux-стенд требует отдельной приёмки.
