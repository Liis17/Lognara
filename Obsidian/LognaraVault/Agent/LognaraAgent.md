# lognara-agent

Parent: [[Index]]

## Назначение

Rust-процесс, который живёт в контейнере рядом с основным приложением. Принимает логи по localhost (от библиотеки lognara или напрямую от приложения), сохраняет запросы на постоянный диск до ответа `202` и сжатыми пачками отправляет в lognara-relay на той же машине. Поля `LogEvent` (level, message, trace_id, id и т. д.) агент не разбирает: он только добавляет идентификацию источника и время приёма, а структурирует данные relay. Метрики пока не поддерживаются.

Поток: приложение → `POST /v1/logs` → атомарная очередь [[Delivery/Spool]] → пачка MessagePack + zstd с общим Bearer-токеном → `POST` в relay → lognara-core.

## Файлы

| Файл | Роль |
|------|------|
| `Lognara-agent/Cargo.toml` | Манифест crate `lognara-agent` (edition 2024). |
| `Lognara-agent/src/main.rs` | Точка входа: логирование, конфиг из env, bind, SIGTERM/SIGINT → остановка. |
| `Lognara-agent/src/lib.rs` | Сборка компонентов и `run()`. |
| `Lognara-agent/src/config.rs` | Параметры `LOGNARA_*`. |
| `Lognara-agent/src/ingest.rs` | HTTP-приём `POST /v1/logs`. |
| `Lognara-agent/src/buffer.rs` | Дисковая очередь, резерв моделей и подготовка пачек. |
| `Lognara-agent/src/wire.rs` | Формат пачки, контракт с relay. |
| `Lognara-agent/src/sender.rs` | Отправка пачек в relay, ретраи. |
| `Lognara-agent/tests/e2e.rs` | Сквозные тесты с фейковым relay. |

## Параметры запуска

Переменные окружения в верхнем регистре. Пустое значение считается незаданным. Если значение отсутствует или некорректно, агент завершается с кодом 1 и называет переменную.

`LOGNARA_RELAY_TOKEN` должен содержать только печатные ASCII-символы без пробелов и управляющих символов. Ключ скрыт в `Debug` конфигурации и ошибках валидации.

| Переменная | Обяз. | По умолчанию | Назначение |
|---|---|---|---|
| `LOGNARA_SERVICE` | да | - | `service` |
| `LOGNARA_SERVER` | да | - | `server` (сервер/нода) |
| `LOGNARA_BACKEND` | да | - | `backend` |
| `LOGNARA_ENVIRONMENT` | нет | нет | `environment` |
| `LOGNARA_SERVICE_INSTANCE` | нет | `$HOSTNAME`, если задан | `service_instance` |
| `LOGNARA_BATCH_SIZE` | нет | `1000` | размер пачки в записях |
| `LOGNARA_FLUSH_INTERVAL_MS` | нет | `5000` | как часто отправлять накопленное |
| `LOGNARA_MAX_BUFFER` | нет | `100000` | лимит подтверждённых записей на диске, не меньше `BATCH_SIZE`; сверх него `503` |
| `LOGNARA_MAX_BUFFER_BYTES` | нет | `67108864` | общий бюджет моделей, индекса и сериализации |
| `LOGNARA_MAX_BATCH_BYTES` | нет | `8388608` | максимум MessagePack и сжатой пачки; не более 8 MiB |
| `LOGNARA_MAX_RECORD_BYTES` | нет | `2097152` | payload одной записи; не более 2 MiB |
| `LOGNARA_SPOOL_DIR` | нет | `/var/lib/lognara-agent/spool` | постоянный каталог очереди |
| `LOGNARA_SPOOL_MAX_MB` | нет | `1024` | квота тел очереди; отдельный pending до 8 MiB |
| `LOGNARA_LISTEN_ADDR` | нет | `127.0.0.1:7400` | адрес приёма |
| `LOGNARA_RELAY_URL` | нет | `http://lognara-relay:7401/v1/batches` | адрес relay |
| `LOGNARA_RELAY_TOKEN` | да | - | общий Bearer-токен для отправки в relay |

## Приём: `POST /v1/logs`

| Content-Type | Записи |
|---|---|
| `text/plain` | всё тело даёт одну запись `Text`, многострочный текст не режется; нужен UTF-8 |
| `application/json` | значение даёт одну запись `Json`, массив даёт по записи на элемент; хранится исходный текст |
| `application/octet-stream` | одна запись `Binary`, байты передаются как есть |

Ответы: `202` все части запроса сохранены и синхронизированы на диске; `400` пустое/нечитаемое тело, невалидный UTF-8 или JSON; `413` тело/payload больше 2 MiB или отдельная модель не помещается; `415` другой или отсутствующий Content-Type; `503` занят единственный slot, не хватает текущего ресурса, квоты или недоступен диск; `408` чтение дольше 30 секунд. Весь запрос подтверждается атомарно. Приложение повторяет запросы без успешного ответа, допускаются дубликаты.

## Контракт с relay

`POST {LOGNARA_RELAY_URL}`, заголовки `Content-Type: application/msgpack`, `Content-Encoding: zstd`, `Authorization: Bearer {LOGNARA_RELAY_TOKEN}`. Один и тот же ключ передаётся при каждой попытке, включая повторы. Тело: `Batch` в MessagePack с именованными полями, сжатый zstd (уровень 3).

```rust
struct Batch { service, server, backend: String, environment, service_instance: Option<String>,
               sent_at: i64, dropped: u64, records: Vec<Record> }
struct Record { received_at: i64, payload: Payload }
enum Payload { Text(String), Json(String), Binary(Vec<u8>) }
```

- `sent_at`, `received_at` — Unix-время в наносекундах.
- `dropped` — поле совместимости; новые пачки всегда содержат 0.
- `records` содержит хотя бы одну запись; relay отклоняет `records=[]` с `400` при любом `dropped`. 
- Ответ relay `2xx` разрешает удаление исходных частей. На любые другие HTTP-ответы и сетевые ошибки пачка остаётся и повторяется теми же байтами с backoff 100 мс–10 с. При `4xx` новый приём приостанавливается с диагностикой; успешная доставка возвращает готовность.
- `sent_at` фиксируется при подготовке, тело хранится в `pending/` до ACK. Отдельный дисковый резерв позволяет подготовить отправку даже при полной основной квоте.
- Владельцы общего ключа считаются доверенными; relay не проверяет принадлежность `service/server/backend`. При добавлении контейнера менять конфигурацию relay не нужно. Docker-сеть и передача ключа из `.env` показаны в корневом README; после смены ключа контейнеры relay и агентов пересоздаются.

## Отправка и память

- Запрос сохраняется группой частей MessagePack до `202`; все части публикуются одним rename после fsync. При ошибке последней части промежуточные части не публикуются. После перезапуска очередь и `pending/` восстанавливаются.
- Пачка отправляется по числу записей, таймеру или давлению байтов. Объединяются только последовательные части одного источника; размер и модельный бюджет могут уменьшить пачку. Верхний предел — `BATCH_SIZE`, 8 MiB MessagePack и 8 MiB сжатого тела.
- В памяти одновременно один входящий запрос и одна отправляемая пачка. Бюджет 64 MiB включает 8 MiB индекса (до 10922 файлов, до 768 байт на элемент с временными копиями), источник и два независимых резерва моделей. JSON-массив ограничивается до выделения payload; MessagePack проверяется без аллокаций перед восстановлением моделей.
- Резервы учитывают capacity и временные объекты. Blocking-worker владеет permit при отмене ожидающего запроса; остановка ждёт работников. Workspace zstd, HTTP/runtime/allocator требуют дополнительной памяти сверх бюджета моделей; RSS измеряется отдельно.
- При пустой очереди не отправляется HTTP, JSON `[]` не создаёт файлов. Принятые записи не вытесняются при заполнении.
- При остановке завершается приём, выполняется финальная попытка до 5 секунд, затем ждут дисковых работников. Недоставленные данные уже на постоянном volume.
- Постоянный volume обязателен; каждый процесс блокирует свой каталог. Гарантия не покрывает удаление/повреждение диска. Обновлять после relay и доставки старой RAM-очереди; инструкция в README.

## Ключевые методы/функции

| Метод | Описание |
|---|---|
| `run(config: Config, listener: TcpListener, shutdown: CancellationToken): io::Result<()>` | Запускает приём и отправку; после `shutdown` дожидается финальной отправки. |
| `Config::from_env(): Result<Config, ConfigError>` | Читает параметры `LOGNARA_*` из окружения. |
| `Config::from_lookup(lookup: impl Fn(&str) -> Option<String>): Result<Config, ConfigError>` | Собирает конфиг из произвольного источника и проверяет обязательный ключ relay. |
| `Config::fmt(f: &mut fmt::Formatter<'_>): fmt::Result` | Форматирует `Debug` конфигурации со скрытым ключом relay. |
| `ingest::router(buffer: Arc<Buffer>): Router` | Роутер с `POST /v1/logs`. |
| ~~`ingest::parse_body(content_type: Option<&str>, body: &[u8]): Result<Vec<Payload>, IngestError>`~~ (удалён: 2026-10-08) | Превращает тело запроса в записи по Content-Type. |
| ~~`Buffer::push(records: impl IntoIterator<Item = Record>)`~~ (удалён: 2026-10-08) | Добавляет записи, вытесняет старые при переполнении, сигналит о полной пачке. |
| ~~`Buffer::take(partial: bool): Option<(Vec<Record>, u64)>`~~ (удалён: 2026-10-08) | Забирает до `batch_size` старейших записей и счётчик вытесненных. |
| `Buffer::full()` | Ждёт изменения очереди; отправка проверяет число и байты. |
| `Sender::run(self)` | Цикл отправки: полная пачка, интервал, остановка. |
| ~~`Relay::deliver(batch: &mut Batch, shutdown: &CancellationToken): bool`~~ (удалён: 2026-10-08) | Доставляет пачку с ретраями; `false`, если прервана остановкой. |
| ~~`Relay::post(body: Vec<u8>): reqwest::Result<StatusCode>`~~ (удалён: 2026-10-08) | Отправляет сжатую пачку с общим Bearer-токеном в relay. |
| `wire::encode(batch: &Batch): Vec<u8>` | MessagePack + zstd. |
| `wire::unix_nanos(): i64` | Текущее Unix-время в наносекундах. |

| `Config::validate(): Result<(), ConfigError>` | Проверяет квоты, два конвейера, metadata и размер записи при запуске. |
| `Config::pipeline_bytes(): usize` | Вычисляет резерв одного конвейера после индекса и источника. |
| `ingest::parse_body_limited(content_type: Option<&str>, body: &[u8], records: usize, max_record: usize): Result<Vec<Payload>, IngestError>` | Проверяет размеры до материализации payload. |
| `Buffer::open(config: Config): Result<Arc<Buffer>, Error>` | Восстанавливает очередь и резервирует её индекс. |
| `Buffer::push(records: Vec<Record>): Result<(), Error>` | Атомарно сохраняет все части запроса. |
| `Buffer::prepare_async(partial: bool): Result<Option<Pending>, Error>` | Возвращает сохранённое тело либо готовит и сохраняет новое. |
| `Buffer::ack_async(pending: Pending): Result<(), Error>` | Удаляет подтверждённые relay исходные части и pending. |
| `Buffer::drain()` | Ждёт уничтожения объектов всех blocking-работников. |
| `Sender::flush(partial: bool)` | Отправляет точные байты, удерживая данные при всех отказах. |

## Проверка

`tests/e2e.rs` проверяет таймер, число записей, shutdown, HTTP-отказы, точные повторы и настоящий SIGKILL после `202`. `buffer.rs` проверяет квоту и атомарный отказ, отправку при полной очереди, байтовое давление с несжимаемыми payload и жизнь permit отменённого worker. `tests/check-memory-profile.py` измеряет RSS при 16 клиентах, заполнении диска и восстановлении relay; все подтверждённые записи должны доставиться.

## Зависимости

- Использует: `tokio`, `tokio-util`, `axum`, `reqwest` (без TLS), `serde`, `serde_json` (`raw_value`), `rmp-serde`, `serde_bytes`, `zstd`, `tracing`, `tracing-subscriber`, `http-body`, общий `lognara-spool`.
- Отправляет пачки в: [[Relay/LognaraRelay]].
- Используется в: [[Architecture]].
