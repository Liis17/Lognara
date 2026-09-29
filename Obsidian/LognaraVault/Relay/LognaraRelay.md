# lognara-relay

Parent: [[Index]]

## Назначение

Rust-процесс, один на машину. Принимает пачки от [[Agent/LognaraAgent]] всех микросервисов, разбирает сырые записи в события, группирует их по источнику и сжатыми пачками отправляет в lognara-core раз в интервал. Пока core недоступен, пачки ждут на диске (spool) и потом уходят от старых к новым. Метрики пока не поддерживаются.

Поток: агент → `POST /v1/batches` → разбор в `Event` → группы в памяти → пачка MessagePack + zstd → `POST` в core (при неудаче в spool).

Файлы, модули и связи между ними описаны в [[Relay/LognaraRelay-ProjectMap]].

## Параметры запуска

Правила как у агента: переменные в верхнем регистре, пустое значение считается незаданным, при ошибке relay завершается с кодом 1 и называет переменную.

| Переменная | Обяз. | По умолчанию | Назначение |
|---|---|---|---|
| `LOGNARA_CORE_URL` | да | - | адрес core, схема `http` или `https` |
| `LOGNARA_CORE_TOKEN` | да | - | Bearer-токен для core |
| `LOGNARA_FLUSH_INTERVAL_MS` | нет | `60000` | интервал отправки в core |
| `LOGNARA_BATCH_SIZE` | нет | `10000` | при таком числе событий пачка уходит раньше интервала |
| `LOGNARA_MAX_BUFFER` | нет | `100000` | лимит событий в памяти, не меньше `BATCH_SIZE`; сверх него агенты получают `503` |
| `LOGNARA_LISTEN_ADDR` | нет | `0.0.0.0:7401` | адрес приёма от агентов |
| `LOGNARA_SPOOL_DIR` | нет | `/var/lib/lognara-relay/spool` | каталог spool на volume |
| `LOGNARA_SPOOL_MAX_MB` | нет | `1024` | лимит spool на диске |
| `LOGNARA_CORE_MAX_BODY_BYTES` | нет | `67108864` | максимум сжатой исходящей пачки, не больше лимита core |
| `LOGNARA_CORE_MAX_DECODED_BYTES` | нет | `268435456` | максимум MessagePack до сжатия, не больше лимита core |

Пример docker-compose (Dockerfile пока нет):

```yaml
services:
  lognara-relay:
    image: lognara-relay
    environment:
      LOGNARA_CORE_URL: https://core.example.com/v1/batches
      LOGNARA_CORE_TOKEN: ${LOGNARA_CORE_TOKEN}
      LOGNARA_FLUSH_INTERVAL_MS: "60000"
    volumes:
      - lognara-spool:/var/lib/lognara-relay/spool
volumes:
  lognara-spool:
```

Агенты по умолчанию шлют на `http://lognara-relay:7401/v1/batches`, то есть на сервис с этим именем в той же сети.

## Приём: `POST /v1/batches`

Нужны `Content-Type: application/msgpack` и `Content-Encoding: zstd`; тело — `Batch` агента (см. «Контракт с relay» в [[Agent/LognaraAgent]]). Relay держит копию этих типов, совместимость проверяет фикстура `tests/fixtures/agent-batch.bin`, закодированная кодом агента.

Ответы: `202` пачка принята, `415` другие заголовки, `413` сжатое тело больше 64 MiB или распакованное больше 256 MiB, `400` тело не zstd с MessagePack, `503` буфер заполнен до `MAX_BUFFER` (агент повторит и продержит записи у себя). Пачку больше лимита relay принимает только в пустой буфер.

## Разбор записей в события

Для каждой записи: `id` — новый UUIDv7, `timestamp` и `ingested_at` — `received_at` агента (Unix-время в наносекундах), `level` — `unknown`.

| Payload | Результат |
|---|---|
| `Text` | `message` = текст |
| `Binary` | `message` = `binary payload, N bytes`, байты в base64 в `attributes["payload_base64"]` |
| `Json`, строка | `message` = строка |
| `Json`, число, bool, null, массив или невалидный JSON | `message` = исходный текст |
| `Json`, объект | канонические ключи в поля события, остальные в `attributes` |

Канонические ключи: `id` (UUID), `timestamp` (RFC 3339), `level` (`trace`/`debug`/`info`/`warn`/`error`/`fatal` без учёта регистра), `message`, `action`, `request_id` (строки), `trace_id` (32 hex), `span_id`, `parent_span_id` (16 hex), `attributes` (объект; его ключи перекрывают одноимённые ключи верхнего уровня). Значение неподходящего типа или формата остаётся в `attributes` под тем же ключом, поле получает значение по умолчанию. Ключи источника и `ingested_at` из JSON тоже попадают в `attributes`: источник всегда берётся от агента.

## Контракт с core

`POST {LOGNARA_CORE_URL}`, заголовки `Content-Type: application/msgpack`, `Content-Encoding: zstd`, `Authorization: Bearer {LOGNARA_CORE_TOKEN}`. Тело: `CoreBatch` в MessagePack с именованными полями, сжатый zstd (уровень 3).

```rust
struct CoreBatch { dropped: u64, groups: Vec<Group> }
struct Group { source: Source, dropped: u64, events: Vec<Event> }
struct Source { environment: Option<String>, server, backend, service: String, service_instance: Option<String> }
struct Event { id: LogId, timestamp, ingested_at: i64, action: Option<String>, level: LogLevel, message: String,
               trace_id: Option<TraceId>, span_id, parent_span_id: Option<SpanId>, request_id: Option<String>,
               attributes: HashMap<String, serde_json::Value> }
enum LogLevel { Trace, Debug, Info, Warn, Error, Fatal, Unknown }  // строка в нижнем регистре
struct LogId(Uuid); struct TraceId([u8; 16]); struct SpanId([u8; 8]);  // bin 16, 16 и 8 байт
```

- Полный `LogEvent` в core = `Source` группы + `Event`. Группы упорядочены по `Source`, события внутри группы — в порядке приёма.
- `CoreBatch.dropped` — события, потерянные relay (вытеснение или ошибка spool) с прошлой пачки; `Group.dropped` — сумма `dropped` от агентов источника.
- `2xx` — доставлено. Сетевые ошибки, `5xx`, `408`, `429`, `401`, `403` — core недоступен, пачка ждёт в spool. Остальные `4xx` — пачка отбрасывается с ошибкой в логе.
- Доставка «хотя бы один раз»: после сбоя посреди отправки пачка может прийти повторно. [[Core/LognaraCore]] дедуплицирует SHA-256 исходных сжатых байтов пачки; перегруппирование тех же событий не устраняет повторы.

## Накопление и отправка

- Пачка уходит раз в `FLUSH_INTERVAL_MS` и сразу, когда в памяти набралось `BATCH_SIZE` событий. Пачка забирает все накопленные группы целиком.
- Перед отправкой пачка делится по обоим байтовым лимитам core с сохранением порядка событий, ID и однократным учётом dropped. Каждая часть кодируется один раз, её байты сохраняются в pending/spool для повторов. Одиночное событие, не помещающееся в лимиты, отбрасывается с диагностикой и учитывается как потеря relay. Уже существующий spool не перекодируется при изменении лимитов.
- Сначала отправляются пачки из spool, от старых к новым; на первой неудаче отправка прерывается до следующего тика или полной пачки. Пока разгружается бэклог, полные пачки из памяти встают в конец spool.
- Новая пачка уходит напрямую, только если spool пуст. Если core недоступен или spool не пуст, она пишется в spool, поэтому порядок сохраняется.
- Spool: файл `{seq:020}-{events}.batch` на пачку, тело запроса в core как есть. Запись через `*.tmp` + `fsync` + `rename`. При старте `*.tmp` удаляются, очередь восстанавливается и уходит на первом тике. При превышении `SPOOL_MAX_MB` удаляются самые старые файлы, их события считаются в `dropped`.
- Запрос в core ограничен 30 с. При остановке прекращается приём, затем 5 с идёт отправка; всё недоставленное сохраняется в spool до следующего запуска.

## Ключевые методы/функции

| Метод | Описание |
|---|---|
| `run(config: Config, listener: TcpListener, spool: Spool, shutdown: CancellationToken): io::Result<()>` | Запускает приём и отправку; после `shutdown` доставляет или сохраняет в spool накопленное. |
| `Config::from_env(): Result<Config, ConfigError>` | Читает параметры `LOGNARA_*` из окружения. |
| `Config::from_lookup(lookup: impl Fn(&str) -> Option<String>): Result<Config, ConfigError>` | Собирает конфиг из произвольного источника, нужен для тестов. |
| `agent_wire::decode(body: &[u8], limit: u64): Result<Batch, DecodeError>` | Распаковывает zstd не больше `limit` байт и разбирает пачку агента. |
| `normalize::event(record: Record): Event` | Превращает запись агента в событие по правилам разбора. |
| `core_wire::encode(batch: &CoreBatch): Vec<u8>` | MessagePack + zstd. |
| `core_wire::encode_split(batch: CoreBatch, body_limit: usize, decoded_limit: usize): (Vec<EncodedBatch>, u64)` | Делит по байтовым лимитам, возвращает готовые тела и число неотправляемых событий. |
| `ingest::router(buffer: Arc<Buffer>): Router` | Роутер с `POST /v1/batches`. |
| `Buffer::push(source: Source, dropped: u64, events: Vec<Event>): Result<(), Full>` | Добавляет события в группу источника; `Full`, если не помещаются в лимит. |
| `Buffer::take(partial: bool): Option<(Vec<Group>, usize)>` | Забирает все группы и число событий; без `partial` только набранную пачку. |
| `Buffer::full()` | Ждёт, пока наберётся `batch_size` событий. |
| `Spool::open(dir: &Path, max_bytes: u64): io::Result<Spool>` | Создаёт каталог, удаляет `*.tmp`, восстанавливает очередь. |
| `Spool::push(body: &[u8], events: u64)` | Дописывает пачку в конец, при нехватке места вытесняет старые. |
| `Spool::oldest(): Option<Vec<u8>>` | Тело самой старой пачки; нечитаемые пропускает и считает потерянными. |
| `Spool::remove_oldest()` | Удаляет самую старую пачку после доставки. |
| `Spool::take_dropped(): u64` | Забирает счётчик потерянных событий для `CoreBatch.dropped`. |
| `Spool::record_dropped(events: u64)` | Учитывает одиночные события, превышающие лимиты core. |
| `Sender::run(self)` | Цикл отправки: тик, полная пачка, остановка. |
| `Sender::flush(partial: bool)` | Отправляет spool, затем память; при неудаче пачка уходит в spool. |
| `Sender::seal(partial: bool)` | Берёт группы из буфера и создаёт очередь ограниченных по размеру тел. |
| `Sender::save_pending()` | Сохраняет все неотправленные части без перекодирования. |
| `Core::deliver(body: Bytes): bool` | Одна попытка отправки; `false`, если core недоступен. |

## Зависимости

- Использует: `tokio`, `tokio-util`, `axum`, `reqwest` (rustls), `serde`, `serde_json`, `rmp-serde`, `serde_bytes`, `zstd`, `uuid` (v7), `time`, `base64`, `bytes`, `tracing`, `tracing-subscriber`; в тестах `tempfile`.
- Принимает пачки от: [[Agent/LognaraAgent]].
- Используется в: [[Architecture]].
