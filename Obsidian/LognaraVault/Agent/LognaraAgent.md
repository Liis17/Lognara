# lognara-agent

Parent: [[Index]]

## Назначение

Rust-процесс, который живёт в контейнере рядом с основным приложением. Принимает логи по localhost (от библиотеки lognara или напрямую от приложения), копит их в оперативной памяти и сжатыми пачками отправляет в lognara-relay на той же машине. Поля `LogEvent` (level, message, trace_id, id и т. д.) агент не разбирает: он только добавляет идентификацию источника и время приёма, а структурирует данные relay. Метрики пока не поддерживаются.

Поток: приложение → `POST /v1/logs` → буфер в памяти → пачка MessagePack + zstd → `POST` в relay → lognara-core.

## Файлы

| Файл | Роль |
|------|------|
| `Lognara-agent/Cargo.toml` | Манифест crate `lognara-agent` (edition 2024). |
| `Lognara-agent/src/main.rs` | Точка входа: логирование, конфиг из env, bind, SIGTERM/SIGINT → остановка. |
| `Lognara-agent/src/lib.rs` | Сборка компонентов и `run()`. |
| `Lognara-agent/src/config.rs` | Параметры `LOGNARA_*`. |
| `Lognara-agent/src/ingest.rs` | HTTP-приём `POST /v1/logs`. |
| `Lognara-agent/src/buffer.rs` | Буфер записей в памяти. |
| `Lognara-agent/src/wire.rs` | Формат пачки, контракт с relay. |
| `Lognara-agent/src/sender.rs` | Отправка пачек в relay, ретраи. |
| `Lognara-agent/tests/e2e.rs` | Сквозные тесты с фейковым relay. |

## Параметры запуска

Переменные окружения в верхнем регистре. Пустое значение считается незаданным. Если значение отсутствует или некорректно, агент завершается с кодом 1 и называет переменную.

| Переменная | Обяз. | По умолчанию | Назначение |
|---|---|---|---|
| `LOGNARA_SERVICE` | да | - | `service` |
| `LOGNARA_SERVER` | да | - | `server` (сервер/нода) |
| `LOGNARA_BACKEND` | да | - | `backend` |
| `LOGNARA_ENVIRONMENT` | нет | нет | `environment` |
| `LOGNARA_SERVICE_INSTANCE` | нет | `$HOSTNAME`, если задан | `service_instance` |
| `LOGNARA_BATCH_SIZE` | нет | `1000` | размер пачки в записях |
| `LOGNARA_FLUSH_INTERVAL_MS` | нет | `5000` | как часто отправлять накопленное |
| `LOGNARA_MAX_BUFFER` | нет | `100000` | лимит записей в памяти, не меньше `BATCH_SIZE` |
| `LOGNARA_LISTEN_ADDR` | нет | `127.0.0.1:7400` | адрес приёма |
| `LOGNARA_RELAY_URL` | нет | `http://lognara-relay:7401/v1/batches` | адрес relay |

## Приём: `POST /v1/logs`

| Content-Type | Записи |
|---|---|
| `text/plain` | всё тело даёт одну запись `Text`, многострочный текст не режется; нужен UTF-8 |
| `application/json` | значение даёт одну запись `Json`, массив даёт по записи на элемент; хранится исходный текст |
| `application/octet-stream` | одна запись `Binary`, байты передаются как есть |

Ответы: `202` запись принята, `400` пустое тело, невалидный UTF-8 или JSON, `413` тело больше 2 MiB, `415` другой или отсутствующий Content-Type.

## Контракт с relay

`POST {LOGNARA_RELAY_URL}`, заголовки `Content-Type: application/msgpack`, `Content-Encoding: zstd`. Тело: `Batch` в MessagePack с именованными полями, сжатый zstd (уровень 3).

```rust
struct Batch { service, server, backend: String, environment, service_instance: Option<String>,
               sent_at: i64, dropped: u64, records: Vec<Record> }
struct Record { received_at: i64, payload: Payload }
enum Payload { Text(String), Json(String), Binary(Vec<u8>) }
```

- `sent_at`, `received_at` — Unix-время в наносекундах.
- `dropped` — сколько записей вытеснено из переполненного буфера с прошлой пачки.
- Ответ relay `2xx` означает, что пачка доставлена. На `4xx`, кроме `408` и `429`, агент отбрасывает пачку с ошибкой в логе. На остальные ответы и сетевые ошибки идёт повтор с backoff от 100 мс до 10 с.

## Отправка

- Когда в буфере набирается `BATCH_SIZE` записей, полные пачки уходят сразу.
- Раз в `FLUSH_INTERVAL_MS` уходит всё накопленное. Поэтому ни одна запись не ждёт дольше интервала, пока relay доступен.
- В полёте всегда одна пачка, порядок записей сохраняется. Пока relay недоступен, новые записи копятся до `MAX_BUFFER`, затем вытесняются самые старые.
- При остановке сначала прекращается приём, затем остаток отправляется в течение 5 с. Недоставленное количество пишется в лог.

## Ключевые методы/функции

| Метод | Описание |
|---|---|
| `run(config: Config, listener: TcpListener, shutdown: CancellationToken): io::Result<()>` | Запускает приём и отправку; после `shutdown` дожидается финальной отправки. |
| `Config::from_env(): Result<Config, ConfigError>` | Читает параметры `LOGNARA_*` из окружения. |
| `Config::from_lookup(lookup: impl Fn(&str) -> Option<String>): Result<Config, ConfigError>` | Собирает конфиг из произвольного источника, нужен для тестов. |
| `ingest::router(buffer: Arc<Buffer>): Router` | Роутер с `POST /v1/logs`. |
| `ingest::parse_body(content_type: Option<&str>, body: &[u8]): Result<Vec<Payload>, IngestError>` | Превращает тело запроса в записи по Content-Type. |
| `Buffer::push(records: impl IntoIterator<Item = Record>)` | Добавляет записи, вытесняет старые при переполнении, сигналит о полной пачке. |
| `Buffer::take(partial: bool): Option<(Vec<Record>, u64)>` | Забирает до `batch_size` старейших записей и счётчик вытесненных. |
| `Buffer::full()` | Ждёт, пока наберётся `batch_size` записей. |
| `Sender::run(self)` | Цикл отправки: полная пачка, интервал, остановка. |
| `Relay::deliver(batch: &mut Batch, shutdown: &CancellationToken): bool` | Доставляет пачку с ретраями; `false`, если прервана остановкой. |
| `wire::encode(batch: &Batch): Vec<u8>` | MessagePack + zstd. |
| `wire::unix_nanos(): i64` | Текущее Unix-время в наносекундах. |

## Зависимости

- Использует: `tokio`, `tokio-util`, `axum`, `reqwest` (без TLS), `serde`, `serde_json` (`raw_value`), `rmp-serde`, `serde_bytes`, `zstd`, `tracing`, `tracing-subscriber`.
- Используется в: [[Architecture]].
