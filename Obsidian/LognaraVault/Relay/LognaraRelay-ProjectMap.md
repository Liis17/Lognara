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
│   ├── agent_wire.rs       контракт агента: Batch, Record, Payload, decode_with_budget()
│   ├── core_wire.rs        контракт с core: CoreBatch, Group, Source, Event, encode()
│   ├── wire_budget.rs      общая no-alloc оценка MessagePack из lognara-spool, идентичная core
│   ├── normalize.rs        разбор Record в Event
│   ├── ingest.rs           Bearer-аутентификация до чтения тела, POST /v1/batches
│   ├── memory.rs           резервы, admission и ожидание blocking-работников
│   ├── model_budget.rs     общий бюджет Batch и нормализации JSON/base64
│   ├── spool.rs            дисковая очередь пачек
│   └── sender.rs           отправка в core, работа со spool, остановка
└── tests/
    ├── e2e.rs              сквозные тесты с фейковым core
    ├── memory_profile.rs   отдельная тяжёлая нагрузка стандартного профиля
    ├── check-memory-profile.py сборка и контроль RSS процесса нагрузки
    └── fixtures/
        └── agent-batch.bin пачка, закодированная wire::encode агента
```

## Стадии конвейера

| Стадия | Модуль | Вход | Выход |
|---|---|---|---|
| Приём | `ingest` | HTTP-запрос агента с общим Bearer-токеном | отказ до тела или `Batch` через `agent_wire::decode_with_budget` |
| Разбор | `normalize` | `Record` + `ModelBudget` | `Event` либо ресурсная ошибка |
| Сохранение | `spool` | все закодированные части запроса | fsync и атомарная публикация до `202` |
| Кодирование | `core_wire` | `CoreBatch` | одна часть через `BatchEncoder::next()` |
| Отправка | `sender` | сохранённое тело | ACK core и удаление либо сохранение для повтора |
| Хранение | `spool` | тело запроса | файлы `*.batch` на volume |

## Связи модулей

- `main` → `config`, `spool`, `lib::run`.
- `lib::run` подключает общий `Spool`, запускает `Sender` в отдельной задаче и `ingest::router` через `axum::serve`. Отправка останавливается отдельным токеном только после приёма.
- `ingest` → `agent_wire`, `normalize`, `core_wire`, `spool`.
- `ingest` → `memory`: slot до чтения тела; blocking-задача владеет permit. Подробности в [[Relay/Memory]].
- `sender` → `spool`; очередь делится с ingest, синхронные дисковые операции выполняются в blocking-работниках. Общая реализация в [[Delivery/Spool]].
- Публичные модули (`agent_wire`, `config`, `core_wire`, `model_budget`, `normalize`, `spool`) используют сквозные тесты; `ingest`, `memory`, `sender` скрыты.

## Тесты

| Где | Что проверяет |
|---|---|
| `config.rs` | loopback по умолчанию, обязательные переменные и ключ, валидация ключа и скрытие секретов, проверки чисел, URL и адреса |
| `ingest.rs` | отказ до чтения непрочитываемого тела, неверная/дублированная авторизация, приоритет `401` над лимитом тела, сохранение проверок типа и тела при верном ключе, атомарный отказ группы, квоту и границы одной записи |
| `agent_wire.rs` | разбор фикстуры агента, лимит распаковки, невалидное тело |
| `core_wire.rs` | кодирование `CoreBatch`, оба лимита, обратный разбор, порядок/ID/dropped при разделении |
| `normalize.rs` | пример лога из задачи, неверные значения в `attributes`, text, binary, не-объекты JSON |
| `spool.rs` | порядок, восстановление после `open`, квоты без вытеснения, атомарные группы и удержание нечитаемого файла |
| `tests/e2e.rs` | группы на тике, ранняя отправка, spool при `503` и порядок после восстановления, spool с прошлого запуска, сохранение при остановке, отказ core `400`, ответы `415`/`400`/`503`, отказ без ключа и пустые пачки не попадают в core/spool |
| `tests/memory_profile.rs` | 16 конкурентных клиентов, тела около 60 MiB, распаковка около 256 MiB, JSON с миллионом узлов, base64, байтовый отказ буфера, core `503` и shutdown |
| `tests/check-memory-profile.py` | запускает отдельный процесс теста; останавливает при RSS выше 2 GiB, проверяет kernel high-water mark после завершения |

Разделённые пачки дополнительно проходят рестарт relay со spool и проверку идентичности байтов повторной отправки; SIGKILL после `202` восстанавливает подтверждённые части. Shutdown с зависшей доставкой удерживает все сохранённые части; oversized replay закрывает admission до исправления файла. Настоящий core проверяется в `Lognara-core/tests/e2e.rs`.
