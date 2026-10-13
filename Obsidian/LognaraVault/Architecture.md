# Архитектура

Parent: [[Index]]

## Технологический стек

| Область | Текущее состояние |
|---------|-------------------|
| Язык и runtime | Rust (edition 2024) и tokio подтверждены для `Lognara-agent/` и `Lognara-relay/`. Core также использует Rust 2024 и tokio; MSRV 1.94. |
| Зависимости | `Lognara-agent/Cargo.toml`: axum, reqwest, tokio, tokio-util, serde, serde_json, rmp-serde, serde_bytes, zstd, tracing, http-body, glob, regex, sha2, libc, общий lognara-spool. Подробности в [[Agent/LognaraAgent]]. `Lognara-relay/Cargo.toml`: общие HTTP/runtime/serde/zstd зависимости, reqwest с rustls, а также uuid, time, base64, bytes, http-body для ограниченного чтения, sha2 для проверки хешей агентов из core. Подробности в [[Relay/LognaraRelay]]. |
| Веб-интерфейс | `Lognara-dock/Cargo.toml`: axum, tower-http (ServeDir), reqwest, rusqlite (bundled), bcrypt, webauthn-rs 0.5 (`conditional-ui`, требует OpenSSL), fs2, time. Фронтенд `Lognara-dock/web/`: React, react-router, TypeScript, Vite; локальные Commissioner и IBM Plex Mono с лицензиями OFL. Подробности в [[Dock/LognaraDock]]. |
| Лицензия | MIT, см. `LICENSE`. |
| Локальный стенд | Docker Compose и Python 3.13 (стандартная библиотека без сторонних Python-пакетов) в `Example/`; сборка Rust-сервисов в Docker на Rust 1.94. Подробнее в [[Repository/LocalExample]]. |

## Компоненты и сервисы

| Компонент | Каталог | Состояние |
|-----------|---------|-----------|
| [[Agent/LognaraAgent]] | `Lognara-agent/` | HTTP, файлы и stdout/stderr через `run --`, атомарная дисковая очередь и отправка в relay; [[Agent/LogSources]]. |
| [[Relay/LognaraRelay]] | `Lognara-relay/` | Реализованы приём пачек агентов, разбор в события, группировка по источнику, отправка в core и spool на диске. Структура в [[Relay/LognaraRelay-ProjectMap]]. |
| [[Core/LognaraCore]] | `Lognara-core/` | Реализованы реестр подключений access.sqlite3 и admin API, авторизация relay, HTTP-приём, WAL, Arrow/Parquet, каталог SQLite, поиск Tantivy, аналитика DataFusion и retention. |
| [[Dock/LognaraDock]] | `Lognara-dock/` | Реализованы вход по htpasswd и passkey, сессии в SQLite, смена пароля, регистрация relay/агентов через admin API core, лента логов через SSE и React-интерфейс. Структура в [[Dock/LognaraDock-ProjectMap]]. |
| [[Delivery/Spool]] | `Lognara-spool/` | Общая атомарная файловая очередь; оба процесса подтверждают приём только после durable commit. |
| [[Repository/LocalExample]] | `Example/` | Локальный Compose: Python-сервер на 8881 и agent в одном контейнере, relay, core и dock отдельно; веб-логи на localhost:7403. |

Корневые файлы описаны в [[Repository/RootFiles]].

## Структура и точки входа

| Путь | Роль |
|------|------|
| `Lognara-agent/src/main.rs` | Точка входа агента. Параметры `LOGNARA_*`, JSON `LOGNARA_SOURCES_FILE`; обычный режим или `run -- программа аргументы`. |
| `Lognara-relay/src/main.rs` | Точка входа relay. Параметры задаются переменными `LOGNARA_*`, spool хранится в `LOGNARA_SPOOL_DIR`. |
| `Lognara-core/src/main.rs` | Точка входа core; отдельный crate, конфигурация `LOGNARA_*`. |
| `Example/compose.yaml` | Запуск app, relay, core и dock на одной машине; dock-init создаёт тестовый вход, на хост публикуются localhost:8881, localhost:7402 и localhost:7403. |
| `Example/entrypoint.py` | Запускает agent и Python-сервер внутри `app`, передаёт остановку обоим процессам. |
| `Example/server.py` | Любой корректный HTTP-метод и путь; данные клиента в JSON/stdout и на локальный agent. |
| `Lognara-dock/src/main.rs` | Точка входа веб-интерфейса; конфигурация `LOGNARA_DOCK_*`, порт `7403`, фронтенд из `LOGNARA_DOCK_WEB_DIR`. |
| `README.md` | Обзор проекта: схема потока, быстрый старт, порты и параметры, API core, статус. |
| `.gitignore` | Исключает сборки Cargo (`target`) и артефакты агента в корневом `/build/`, резервные файлы rustfmt, PDB, Cargo Mutants, `default.profraw`, `.DS_Store`, локальный `.env` с ключом. |
| `.env.example` | Центральные admin/query-токены и данные core; персональные LOGNARA_RELAY_CORE_TOKEN/LOGNARA_AGENT_TOKEN из dock, LOGNARA_RELAY_MEMORY_MB=2048 и данные agent/relay на машине приложений. |
| `LICENSE` | Текст лицензии MIT. |

Каждый сервис собирается отдельным crate, общего Cargo workspace нет.

Автоматические проверки описаны в [[Repository/CI]]: `.github/workflows/ci.yml` на GitHub-hosted Ubuntu проверяет все пять Rust-crate и веб-интерфейс после push в любую ветку и при событиях PR. Проверки включают сборку, тесты с `crash-tests` у core, форматирование, Clippy, профили памяти agent/relay и TypeScript/Vite на Node.js 24. Итоговый status check `CI passed` объединяет результаты; для блокировки merge его нужно включить в правилах защищённой ветки.

Поставка описана в [[Repository/Images]]: после успешного CI push в `master` публикует в GHCR отдельные образы core, relay и dock для `linux/amd64`, теги `master` и `sha-<полный SHA коммита>`. Agent собирается в контейнере приложения. Все три Dockerfile собирают release-бинарник и используют Debian slim с процессом `1000:1000`; dock также включает веб-сборку. Примеры Compose используют общий `LOGNARA_IMAGE_TAG` из `.env`.

## Сквозной поток логов

1. Приложение (или библиотека lognara) отправляет лог на `POST http://127.0.0.1:7400/v1/logs` агента в своём контейнере: text, JSON или бинарные данные.
2. Агент добавляет время приёма, атомарно сохраняет запрос до `202` и отправляет пачку, как только набрано `LOGNARA_BATCH_SIZE` записей или прошёл `LOGNARA_FLUSH_INTERVAL_MS`; дополнительно ограничивает модель и байты пачки.
3. Непустая пачка (MessagePack + zstd, идентификация источника: service, server, backend, environment, service_instance) уходит в lognara-relay на той же машине с `Authorization: Bearer {LOGNARA_RELAY_TOKEN}` при каждой попытке. При пустом буфере агент не отправляет запрос; relay отклоняет `records=[]` с `400` независимо от `dropped`.
4. Relay проверяет персональный токен по списку своих агентов из core до чтения тела и распаковки, затем разбирает записи в события, атомарно сохраняет все части до `202` и раз в `LOGNARA_FLUSH_INTERVAL_MS` (или сразу по `LOGNARA_BATCH_SIZE` событий) отправляет пачки MessagePack + zstd в lognara-core с персональным relay-токеном. Перед первой отправкой делит по согласованным байтовым лимитам core; повторяет исходные байты.
5. Пока core недоступен, пачки relay ждут в spool на volume и затем уходят от старых к новым. Core подтверждает пачку после fsync WAL, затем публикует поиск и аналитику по открытым и закрытым сегментам.

Формат пачки агента описан в разделе «Контракт с relay» заметки [[Agent/LognaraAgent]], формат пачки для core — в разделе «Контракт с core» заметки [[Relay/LognaraRelay]].

## Просмотр логов

Браузер → nginx (TLS) → lognara-dock `:7403` → `POST /v1/logs/search` core. Dock проверяет вход по htpasswd с хоста или passkey, хранит сессии и passkey в SQLite и раздаёт новые события через SSE. Core для этого не меняется; ограничения ленты описаны в [[Dock/LognaraDock]].

## Паттерны

- Модули разделены по стадиям конвейера; agent использует buffer как дисковую очередь и подготовку ограниченных пачек, relay использует normalize и spool.
- Agent и relay используют общий crate [[Delivery/Spool]]; relay держит копию контракта агента, совместимость проверяет фикстура, закодированная агентом.
- Основная конфигурация читается из `LOGNARA_*`; агент дополнительно загружает JSON источников из `LOGNARA_SOURCES_FILE`. Всё проверяется при старте до запуска дочерней программы.
- HTTP, файлы и потоки идут напрямую в общий Buffer. Для файлов сначала fsync spool, затем атомарный checkpoint; после crash возможны повторы. Потоки при перегрузке продолжают чтение с учётом потерь. Origin переносится relay в attributes без изменения core. Подробности в [[Agent/LogSources]].
- Сначала запускают core и dock. Dock с admin-токеном создаёт relay и агентов по имени и привязке к relay. Core генерирует 32 случайных байта через rand 0.9 (системный OsRng), выдаёт токен один раз, хранит только SHA-256 в `access.sqlite3`. Имена и RFC3339 created_at — метаданные профиля; source-поля не проверяются.
- Core требует разные `LOGNARA_ADMIN_TOKEN` и `LOGNARA_QUERY_TOKEN`. Dock хранит их как `LOGNARA_DOCK_CORE_ADMIN_TOKEN` и `LOGNARA_DOCK_CORE_TOKEN`; эти секреты не попадают в браузер. Зарегистрированный relay передаёт свой `LOGNARA_CORE_TOKEN` для batches и GET `/v1/agents`.
- Agent получает свой персональный токен через прежний `LOGNARA_RELAY_TOKEN`. Relay перед HTTP-стартом читает кеш `.agents` из реально открытого spool и обновляет его из core с таймаутом 5 секунд, затем каждые 10 секунд. Ошибка сохраняет прежний список; после успешного обновления удалённый агент получает 401. Список переживает рестарт при недоступном core. Core сразу отзывает удалённый relay.
- Relay слушает `127.0.0.1:7401`, в Docker явно `0.0.0.0:7401` в выделенной сети без публикации на хост. `.env` содержит `LOGNARA_RELAY_CORE_TOKEN`, `LOGNARA_AGENT_TOKEN`, Compose передаёт их процессам. Каждый процесс использует отдельный токен и spool.
- В обычном режиме остановка идёт через `CancellationToken`: прекращается приём, затем финальная отправка. В `run` сначала завершается программа с продолжающимся сбором, затем ограниченное дочитывание pipe, остановка сборщиков и HTTP, финальная отправка; сигналы и сроки в [[Agent/LogSources]].
- Relay проверяет сумму резервов памяти при запуске и ограничивает конкурентный приём до чтения тела. Входная модель и JSON нормализации проверяются до объектов; байтовые резервы удерживаются нормализованными событиями до атомарного сохранения; sender читает готовые тела с отдельным резервом. Перед финальной отправкой ждёт также blocking-работников отменённых запросов. Подтверждённая очередь хранится на диске; ошибки и переполнение не удаляют данные. Ресурсный контракт описан в [[Relay/Memory]].

## Как добавлять компоненты

1. Определи домен по фактической структуре исходников.
2. Добавь заметку компонента в соответствующую папку vault и ссылку на неё в [[Index]].
3. Для крупного компонента (более 10 файлов или сложная структура типов) создай `-ProjectMap.md`.
4. Обнови эту заметку при изменении стека, точек входа или сквозных потоков.
5. Changelog в vault не веди — историю изменений хранит git.

## Хранилище core

[[Core/LognaraCore]] использует WAL, Arrow/Parquet + ZSTD, Tantivy, DataFusion и SQLite-каталог. Приём совместим с relay, время в наносекундах.

Core запускается отдельно за TLS-прокси, приём использует персональные relay-токены; admin и query разделены. Query-токен использует [[Dock/LognaraDock]]: браузер получает логи только через dock и его сессии. SIGTERM/SIGINT дорабатывает WAL. HTTP API, эксплуатация и нагрузочный стенд документированы в `Lognara-core/README.md` и `Lognara-core/docs/`.

В тестовом [[Repository/LocalExample]] core запускается без TLS-прокси в изолированной Compose-сети и публикует API только на `127.0.0.1:7402`. App публикует `127.0.0.1:8881`, dock — `127.0.0.1:7403` с origin `http://localhost:7403`; agent остаётся на localhost своего контейнера, relay наружу не публикуется. Admin/query-токены задаются в Compose, токены relay/агентов выдаются после регистрации в dock, отдельные volumes хранят обе очереди, core, htpasswd и сессии/passkey dock. Запрос → Python JSON/stdout → agent → relay → core → SSE dock, включая вход, проверяется `Example/smoke-test.py`.

Agent, relay и core требуют постоянное отдельное хранилище. В Compose источник задают `LOGNARA_AGENT_DATA`, `LOGNARA_RELAY_DATA`, `LOGNARA_CORE_DATA`: пусто означает Docker volume, абсолютный путь означает папку на хосте. Agent резервирует 64 MiB под индекс, источник, приём и подготовку отправки; payload ограничен 2 MiB, пачки 8 MiB. Дисковая квота каждого процесса 1 GiB; у agent отдельно до 8 MiB под pending. Переполнение возвращает `503`, HTTP-отказы удерживают очередь. Подробности и гарантия в [[Delivery/Spool]], [[Agent/LognaraAgent]] и README.

RAM relay задаётся `LOGNARA_RELAY_MEMORY_MB` (MiB, default/min2048); в Compose то же значение определяет `mem_limit`. Внутренний бюджет равен общему RAM минус 256 MiB runtime reserve, отдельные резервы не пересчитываются.
