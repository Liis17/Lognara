# lognara-relay — карта проекта

Parent: [[Relay/LognaraRelay]]

## Дерево

```
Lognara-relay/
├── Cargo.toml              crate lognara-relay, edition 2024, MIT
├── Cargo.lock
├── src/
│   ├── main.rs             точка входа: логирование, конфиг, spool, bind, SIGTERM/SIGINT
│   ├── lib.rs              сборка компонентов и run()
│   ├── config.rs           параметры LOGNARA_*
│   ├── agent_wire.rs       контракт агента: Batch, Record, Payload, decode()
│   ├── core_wire.rs        контракт с core: CoreBatch, Group, Source, Event, encode()
│   ├── wire_budget.rs      no-alloc оценка памяти MessagePack, идентичная core
│   ├── normalize.rs        разбор Record в Event
│   ├── ingest.rs           Bearer-аутентификация до чтения тела, POST /v1/batches
│   ├── memory.rs           резервы, admission и ожидание blocking-работников
│   ├── model_budget.rs     общий бюджет Batch и нормализации JSON/base64
│   ├── buffer.rs           группы событий в памяти
│   ├── spool.rs            дисковая очередь пачек
│   └── sender.rs           отправка в core, работа со spool, остановка
└── tests/
    ├── e2e.rs              сквозные тесты с фейковым core
    └── fixtures/
        └── agent-batch.bin пачка, закодированная wire::encode агента
```

## Стадии конвейера

| Стадия | Модуль | Вход | Выход |
|---|---|---|---|
| Приём | `ingest` | HTTP-запрос агента с общим Bearer-токеном | `401` без чтения тела или `Batch` через `agent_wire::decode` |
| Разбор | `normalize` | `Record` | `Event` |
| Накопление | `buffer` | `Source` + `Vec<Event>` | `Vec<Group>` по `take()` |
| Кодирование | `core_wire` | `CoreBatch` | ограниченные по байтам тела через `encode_split()` |
| Отправка | `sender` | тело запроса | `POST` в core или `Spool::push` |
| Хранение | `spool` | тело запроса | файлы `*.batch` на volume |

## Связи модулей

- `main` → `config`, `spool`, `lib::run`.
- `lib::run` создаёт `Buffer`, запускает `Sender` в отдельной задаче и `ingest::router` через `axum::serve`. Отправка останавливается отдельным токеном только после приёма.
- `ingest` → `agent_wire`, `normalize`, `buffer`.
- `ingest` → `memory`: slot до чтения тела; blocking-задача владеет permit. Подробности в [[Relay/Memory]].
- `sender` → `buffer`, `spool`, `core_wire`. `Sender` владеет `Spool` единолично, поэтому очередь без блокировок.
- Публичные модули (`agent_wire`, `config`, `core_wire`, `normalize`, `spool`) используют сквозные тесты; `buffer`, `ingest`, `sender` скрыты.

## Тесты

| Где | Что проверяет |
|---|---|
| `config.rs` | loopback по умолчанию, обязательные переменные и ключ, валидация ключа и скрытие секретов, проверки чисел, URL и адреса |
| `ingest.rs` | отказ до чтения непрочитываемого тела, неверная/дублированная авторизация, приоритет `401` над лимитом тела, сохранение проверок типа и тела при верном ключе |
| `agent_wire.rs` | разбор фикстуры агента, лимит распаковки, невалидное тело |
| `core_wire.rs` | кодирование `CoreBatch`, оба лимита, обратный разбор, порядок/ID/dropped при разделении |
| `normalize.rs` | пример лога из задачи, неверные значения в `attributes`, text, binary, не-объекты JSON |
| `buffer.rs` | группировка по источнику, `take`, лимит, сигнал полной пачки |
| `spool.rs` | порядок, восстановление после `open`, вытеснение с `dropped`, нечитаемый файл |
| `sender.rs` | какие ответы core окончательны |
| `tests/e2e.rs` | группы на тике, ранняя отправка, spool при `503` и порядок после восстановления, spool с прошлого запуска, сохранение при остановке, отказ core `400`, ответы `415`/`400`/`503`, отказ без ключа не меняет dropped и не попадает в core/spool |

Разделённые пачки дополнительно проходят рестарт relay со spool и проверку идентичности байтов повторной отправки. Настоящий core проверяется в `Lognara-core/tests/e2e.rs`.
