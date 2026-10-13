# lognara-relay

Parent: [[Index]]

## Назначение

Rust-процесс, один на машину. Принимает пачки от [[Agent/LognaraAgent]] всех микросервисов, разбирает сырые записи в события, группирует их по источнику и сжатыми пачками отправляет в lognara-core раз в интервал. Пока core недоступен, пачки ждут на диске (spool) и потом уходят от старых к новым. Метрики пока не поддерживаются.

Поток: агент → `POST /v1/batches` → проверка персонального токена агента по списку core до чтения тела → разбор в `Event` → пачки MessagePack + zstd → атомарный durable spool → `202` → `POST` в core.

Файлы, модули и связи между ними описаны в [[Relay/LognaraRelay-ProjectMap]].

## Параметры запуска

Правила как у агента: переменные в верхнем регистре, пустое значение считается незаданным, при ошибке relay завершается с кодом 1 и называет переменную.

`LOGNARA_CORE_TOKEN` содержит персональный токен relay из dock, скрытый в Debug. Общий `LOGNARA_RELAY_TOKEN` из конфигурации relay удалён.

| Переменная | Обяз. | По умолчанию | Назначение |
|---|---|---|---|
| `LOGNARA_CORE_URL` | да | - | адрес core, схема `http` или `https` |
| `LOGNARA_CORE_TOKEN` | да | - | персональный токен relay для batches и списка агентов |
| `LOGNARA_FLUSH_INTERVAL_MS` | нет | `60000` | интервал отправки в core |
| `LOGNARA_BATCH_SIZE` | нет | `10000` | при таком числе событий пачка уходит раньше интервала |
| `LOGNARA_MAX_BUFFER` | нет | `100000` | лимит подтверждённых событий в очереди, не меньше `BATCH_SIZE`; сверх него агенты получают `503` |
| `LOGNARA_LISTEN_ADDR` | нет | `127.0.0.1:7401` | адрес приёма от агентов; для Docker явно `0.0.0.0:7401` |
| `LOGNARA_SPOOL_DIR` | нет | `/var/lib/lognara-relay/spool` | каталог spool на volume |
| `LOGNARA_SPOOL_MAX_MB` | нет | `1024` | лимит spool на диске |
| `LOGNARA_CORE_MAX_BODY_BYTES` | нет | `67108864` | максимум сжатой исходящей пачки, не больше лимита core |
| `LOGNARA_CORE_MAX_DECODED_BYTES` | нет | `268435456` | максимум MessagePack до сжатия, не больше лимита core |
| `LOGNARA_CORE_MAX_MODEL_BYTES` | нет | `268435456` | бюджет модели: 128 байт на узел MessagePack плюс string/bin |
| `LOGNARA_RELAY_MEMORY_MB` | нет | `2048` | общий RAM в MiB, минимум 2048; 256 MiB запас runtime |
| `LOGNARA_RELAY_MAX_MODEL_BYTES` | нет | `268435456` | резерв входной модели и нормализации |
| `LOGNARA_RELAY_MAX_BUFFER_BYTES` | нет | `268435456` | резерв нормализованных моделей до durable commit |
| `LOGNARA_RELAY_MAX_INGEST_CONCURRENCY` | нет | `1` | число одновременных чтений/обработок |

Ресурсная конфигурация, admission до чтения тела и ожидание blocking-работников при остановке описаны в [[Relay/Memory]].

`Lognara-relay/Dockerfile` собирается из корня репозитория и включает общий spool. Образ GHCR публикуется после успешного CI в `master`, работает от `1000:1000`; схема поставки и тегов в [[Repository/Images]].

Пример docker-compose:

```yaml
services:
  lognara-relay:
    image: lognara-relay
    environment:
      LOGNARA_CORE_URL: https://core.example.com/v1/batches
      LOGNARA_CORE_TOKEN: ${LOGNARA_RELAY_CORE_TOKEN:?Задайте токен relay из dock}
      LOGNARA_RELAY_MEMORY_MB: ${LOGNARA_RELAY_MEMORY_MB:-2048}
      LOGNARA_LISTEN_ADDR: "0.0.0.0:7401"
      LOGNARA_FLUSH_INTERVAL_MS: "60000"
    mem_limit: ${LOGNARA_RELAY_MEMORY_MB:-2048}m
    volumes:
      - ${LOGNARA_RELAY_DATA:-lognara-spool}:/var/lib/lognara-relay/spool
    networks: [lognara-ingest]
networks:
  lognara-ingest:
    driver: bridge
volumes:
  lognara-spool:
```

Пустой `LOGNARA_RELAY_DATA` оставляет volume `lognara-spool`, абсолютный путь монтирует папку на хосте. Агенты по умолчанию шлют на `http://lognara-relay:7401/v1/batches`, то есть на сервис с этим именем в той же сети. Каждый агент получает свой токен из dock как `LOGNARA_RELAY_TOKEN` через environment. Порт `7401` на хост не публикуется, `network_mode: host` не используется. `.env.example` находится в корне репозитория, а фрагмент для контейнера приложения с агентом показан в корневом README. После пересоздания профиля обновляется токен только соответствующего процесса с сохранением spool.

## Приём: `POST /v1/batches`

Нужен единственный `Authorization: Bearer {персональный токен агента}`. Middleware проверяет SHA-256 по HashSet зарегистрированных агентов данного relay до чтения тела и распаковки; схема Bearer без учёта регистра. Затем проверяются `Content-Type: application/msgpack` и `Content-Encoding: zstd`; тело — `Batch` агента (см. «Контракт с relay» в [[Agent/LognaraAgent]]). Relay держит копию этих типов, совместимость проверяет фикстура `tests/fixtures/agent-batch.bin`, закодированная кодом агента.

Ответы: `401` отказ авторизации до тела; `202` все части запроса сохранены и синхронизированы на диске; `415` неподдерживаемые заголовки; `413` превышение 2 MiB одной записи, 64/256 MiB тела/распаковки, бюджета модели либо лимитов кодирования отдельного события; `400` невалидное тело или пустые records; `503` admission, память, квота spool или ошибки диска; `408` тайм-аут чтения. При отказе не подтверждается ни одна часть запроса; ранее подтверждённые данные не вытесняются.

После декодирования relay требует хотя бы одну запись: `records=[]` отклоняется с `400` при любом `dropped`, до создания группы и резервирования памяти буфера. Пустая пачка не меняет счётчики и не попадает в core или spool; сведения о потерях агент передаёт вместе с записями.

Агент сначала регистрируется в core через dock с привязкой к relay. Поля `service/server/backend` принимаются без проверки принадлежности.

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

Origin агента необязателен и совместим со старыми записями. До создания атрибутов нормализация резервирует их модельную память, проверяет ID/путь/поток и диапазон байтов, затем добавляет `lognara.source`, `log.file.path` или `log.stream`, `log.origin.offset/end`, `log.fragment.index/continued`. Служебные атрибуты имеют приоритет над payload. Детали происхождения в [[Agent/LogSources]]; core получает обычные attributes.

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
- `CoreBatch.dropped` — счётчик совместимости старых пачек; новые пачки содержат 0; `Group.dropped` — сумма `dropped` от агентов источника.
- `2xx` — доставлено; только после ACK удаляется файл. Любой отказ или сетевая ошибка сохраняет пачку для повторов, включая `400`, `401` и `413`.
- Доставка «хотя бы один раз»: после сбоя посреди отправки пачка может прийти повторно. [[Core/LognaraCore]] дедуплицирует SHA-256 исходных сжатых байтов пачки; перегруппирование тех же событий не устраняет повторы.

## Накопление и отправка

- Все запросы сначала кодируются и сохраняются атомарно через [[Delivery/Spool]]. Одиночное непомещающееся событие отменяет весь запрос до `202`.
- Части идут FIFO без перегруппирования уже сохранённых тел; source и события каждой входящей пачки сохраняются. Разные запросы могут давать отдельные HTTP-пачки core.
- Отправка запускается на тике либо при `BATCH_SIZE` ожидающих событий. Восстановленная очередь отправляется на первом тике.
- Квоты байтов, событий и индекса возвращают `503`, ничего не удаляя. Чтение ограничено резервом sender; ошибки и oversized файлы сохраняются, приём приостанавливается. После исправления повреждённого/изменённого файла требуется перезапуск для обновления metadata.
- После остановки приёма ждут blocking-работников, затем отправляют до 5 секунд; все остатки уже на диске. Чтение/ACK sender также завершаются до освобождения каталога.

## Ключевые методы/функции

| Метод | Описание |
|---|---|
| `run(config: Config, listener: TcpListener, spool: Spool, shutdown: CancellationToken): io::Result<()>` | Запускает приём и отправку; после `shutdown` доставляет или сохраняет в spool накопленное. |
| `Config::from_env(): Result<Config, ConfigError>` | Читает параметры `LOGNARA_*` из окружения. |
| `Config::from_lookup(lookup: impl Fn(&str) -> Option<String>): Result<Config, ConfigError>` | Собирает конфиг и проверяет параметры core и ресурсы. |
| `Config::fmt(f: &mut fmt::Formatter<'_>): fmt::Result` | Форматирует `Debug` конфигурации со скрытым токеном core. |
| `agent_wire::decode(body: &[u8], limit: u64): Result<Batch, DecodeError>` | Распаковывает zstd не больше `limit` байт и разбирает пачку агента. |
| `normalize::event(record: Record): Event` | Превращает запись агента в событие по правилам разбора. |
| `agent_wire::decode_with_budget(body: &[u8], limit: usize, budget: &mut ModelBudget): Result<Batch, DecodeError>` | Проверяет распаковку и модель до serde. |
| `normalize::event_with_budget(record: Record, budget: &mut ModelBudget): Result<Event, TooLarge>` | Проверяет JSON/base64 и возвращает ресурсную ошибку. |
| `core_wire::encode(batch: &CoreBatch): Vec<u8>` | MessagePack + zstd. |
| `core_wire::encode_split(batch: CoreBatch, body_limit: usize, decoded_limit: usize, model_limit: usize): (Vec<EncodedBatch>, u64)` | Делит по байтовым лимитам и бюджету модели, возвращает готовые тела и число неотправляемых событий. |
| ~~`ingest::router(buffer: Arc<Buffer>): Router`~~ (удалён: 2026-10-07) | Заменён вариантом с обязательным ключом. |
| ~~`ingest::router(buffer: Arc<Buffer>, token: &str): Router`~~ (удалён: 2026-10-07) | Заменён вариантом с ресурсами admission. |
| ~~`ingest::router(buffer: Arc<Buffer>, token: &str, resources: Arc<Resources>): Router`~~ (удалён: 2026-10-08) | Проверяет авторизацию, заголовки и admission перед чтением тела. |
| ~~`ingest::token_hash(token: &str): [u8; 32]`~~ (удалён: 2026-10-13) | Хеширование перенесено в Agents. |
| ~~`ingest::authorize(State(expected): State<[u8; 32]>, request: Request, next: Next): Response`~~ (удалён: 2026-10-13) | Общий ключ заменён списком агентов. |
| `ingest::authorize(State(agents): State<Arc<Agents>>, request: Request, next: Next): Response` | Проверяет персональный токен до тела. |
| `ingest::read_body(body: Body): Result<Vec<u8>, IngestError>` | Копирует фрагменты последовательно в capacity 64 MiB, освобождая каждый до следующего. |
| ~~`ingest::accept(buffer: &Buffer, body: &[u8], model_limit: usize): Result<StatusCode, IngestError>`~~ (удалён: 2026-10-08) | Декодирует пачку, отклоняет пустые records и атомарно добавляет нормализованные события. |
| ~~`Buffer::push(source: Source, dropped: u64, events: Vec<Event>): Result<(), Full>`~~ (удалён: 2026-10-08) | Резервирует capacity до изменения группы; различает TooLarge и Busy. |
| ~~`Buffer::take(partial: bool): Option<(Vec<Group>, usize)>`~~ (удалён: 2026-10-07) | Заменён передачей моделей и резервов. |
| ~~`Buffer::take(partial: bool): Option<Taken>`~~ (удалён: 2026-10-08) | Забирает группы и резервы; также реагирует на байтовое давление и dropped-only группы. |
| ~~`Buffer::full()`~~ (удалён: 2026-10-08) | Ждёт `batch_size` событий либо байтового давления/отказа. |
| `Spool::open(dir: &Path, max_bytes: u64): io::Result<Spool>` | Создаёт каталог, удаляет `*.tmp`, восстанавливает очередь. |
| ~~`Spool::push(body: &[u8], events: u64)`~~ (удалён: 2026-10-08) | Дописывает пачку; вытесняет старые, сохраняя oversized файлы даже сверх дисковой квоты. |
| `Spool::set_replay_limit(limit: usize)` | Задаёт допустимый размер чтения сохранённого тела. |
| `Spool::fits_replay_limit(): bool` | Проверяет размеры всех известных файлов для готовности приёма. |
| ~~`Spool::oldest(): Option<Vec<u8>>`~~ (удалён: 2026-10-08) | Тело самой старой пачки; нечитаемые пропускает и считает потерянными. |
| ~~`Spool::remove_oldest()`~~ (удалён: 2026-10-08) | Удаляет самую старую пачку после доставки. |
| ~~`Spool::take_dropped(): u64`~~ (удалён: 2026-10-08) | Забирает счётчик потерянных событий для `CoreBatch.dropped`. |
| ~~`Spool::record_dropped(events: u64)`~~ (удалён: 2026-10-08) | Учитывает одиночные события, превышающие лимиты core. |
| `Sender::run(self)` | Цикл отправки: тик, полная пачка, остановка. |
| ~~`Sender::flush(partial: bool)`~~ (удалён: 2026-10-08) | Сначала завершает сохранённое кодирование, затем replay без одновременных больших workspace. |
| ~~`Sender::seal(partial: bool)`~~ (удалён: 2026-10-08) | Передаёт группы с резервами ленивому BatchEncoder. |
| ~~`Sender::save_pending()`~~ (удалён: 2026-10-08) | Последовательно кодирует и сохраняет остатки, включая job отменённого flush. |
| `Core::deliver(body: Bytes): bool` | Одна попытка отправки; `false`, если core недоступен. |

## Зависимости

- Использует: `tokio`, `tokio-util`, `axum`, `reqwest` (rustls), `serde`, `serde_json`, `rmp-serde`, `serde_bytes`, `zstd`, `uuid` (v7), `time`, `base64`, `bytes`, `http-body`, `sha2`, `subtle`, `tracing`, `tracing-subscriber`; в тестах `tempfile`, `tower` (`util`).
- Принимает пачки от: [[Agent/LognaraAgent]].
- Используется в: [[Architecture]].

| Метод | Назначение |
|---|---|
| `ingest::router(spool: Spool, token: &str, resources: Arc<Resources>): Router` | Принимает только после авторизации и admission. |
| `ingest::accept(spool: &Spool, body: &[u8], resources: &Resources): Result<StatusCode, IngestError>` | Нормализует, кодирует и атомарно сохраняет все части до 202. |
| `Spool::append_group(parts: IntoIterator): Result<(), Error>` | В blocking-работнике публикует группу целиком. |
| `Spool::push(body: &[u8], events: u64): Result<(), Error>` | Сохраняет одну часть без вытеснения. |
| `Spool::oldest(): Result<Option<Vec<u8>>, Error>` | Возвращает ограниченное тело или ошибку без пропуска. |
| `Spool::remove_oldest(): Result<(), Error>` | Удаляет только после подтверждения core. |
| `Spool::drain()` | Ждёт завершения отменённого чтения/ACK sender. |
| `Sender::flush()` | Отправляет сохранённые байты до первого отказа. |

## Синхронизация агентов

`Agents` хранит RwLock<HashSet<[u8;32]>>. Перед axum serve: загрузка `.agents` из `Spool::dir()`, затем один GET по `core_url.join("agents")` с relay Bearer и timeout 5 секунд. Далее обновление раз в 10 секунд; shutdown отменяет фоновую задачу. Кеш — JSON ответа core, atomic_file/fsync/rename; spool игнорирует этот side-файл и очищает `.agents.tmp`. Файл перезаписывается только при изменении множества хешей; ошибка записи warn и повтор на следующем тике. Память обновляется при каждом успешном ответе независимо от сохранённого кеша.

Не-200, сеть, невалидный JSON/хеш оставляют старый список. Размер кеша/ответа ограничен 8 MiB. Без валидного кеша и доступного core никто не принимается. При недоступном core кеш позволяет работу после рестарта; отзыв применяется при успешном обновлении списка.

| Метод | Назначение |
|---|---|
| ~~`ingest::router(spool: Spool, token: &str, resources: Arc<Resources>): Router`~~ (удалён: 2026-10-13) | Один общий ключ заменён Agents. |
| `ingest::router(spool: Spool, agents: Arc<Agents>, resources: Arc<Resources>): Router` | Собирает авторизованный приём. |
| `Agents::contains(token: &str): bool` | Проверяет SHA-256 токена по множеству хешей. |
| `AgentSync::new(core_url: &Url, core_token: &str, dir: &Path, agents: Arc<Agents>): AgentSync` | Создаёт клиент и путь кеша. |
| `AgentSync::load_cache(): ()` | Восстанавливает валидный кеш без plaintext токенов. |
| `AgentSync::sync(): ()` | Обновляет память и при необходимости дисковый кеш. |
| `AgentSync::run(interval: Duration, stop: CancellationToken): ()` | Обновляет список до отмены. |
| `Spool::dir(): &Path` | Возвращает фактический канонический каталог spool. |

Unit/e2e: неизвестный токен отклоняется до тела, добавление и отзыв в фоне, кеш при рестарте и недоступном core, отсутствие кеша закрывает приём, сбой записи кеша не сохраняет отозванные права в памяти.

`LOGNARA_RELAY_MEMORY_MB` заменяет прежний лимит резервов в байтах. `Config::memory_mb` — RAM всего процесса/контейнера; `Config::reserve_bytes(): Option<usize>` вычисляет `MB * MiB - RUNTIME_RESERVE` с checked arithmetic. По умолчанию резерв равен прежним 1792 MiB.
