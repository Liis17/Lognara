# Архитектура

Parent: [[Index]]

## Технологический стек

| Область | Текущее состояние |
|---------|-------------------|
| Язык и runtime | Rust (edition 2024) и tokio подтверждены для `Lognara-agent/` и `Lognara-relay/`. Core также использует Rust 2024 и tokio; MSRV 1.94. |
| Зависимости | `Lognara-agent/Cargo.toml`: axum, reqwest, tokio, tokio-util, serde, serde_json, rmp-serde, serde_bytes, zstd, tracing. Подробности в [[Agent/LognaraAgent]]. `Lognara-relay/Cargo.toml`: те же, reqwest с rustls, а также uuid, time, base64, bytes. Подробности в [[Relay/LognaraRelay]]. |
| Лицензия | MIT, см. `LICENSE`. |

## Компоненты и сервисы

| Компонент | Каталог | Состояние |
|-----------|---------|-----------|
| [[Agent/LognaraAgent]] | `Lognara-agent/` | Реализован приём логов, буфер в памяти и отправка в relay. |
| [[Relay/LognaraRelay]] | `Lognara-relay/` | Реализованы приём пачек агентов, разбор в события, группировка по источнику, отправка в core и spool на диске. Структура в [[Relay/LognaraRelay-ProjectMap]]. |
| [[Core/LognaraCore]] | `Lognara-core/` | Реализованы HTTP-приём, WAL, Arrow/Parquet, каталог SQLite, поиск Tantivy, аналитика DataFusion и retention. |

Корневые файлы описаны в [[Repository/RootFiles]].

## Структура и точки входа

| Путь | Роль |
|------|------|
| `Lognara-agent/src/main.rs` | Точка входа агента. Параметры задаются переменными `LOGNARA_*`. |
| `Lognara-relay/src/main.rs` | Точка входа relay. Параметры задаются переменными `LOGNARA_*`, spool хранится в `LOGNARA_SPOOL_DIR`. |
| `Lognara-core/src/main.rs` | Точка входа core; отдельный crate, конфигурация `LOGNARA_*`. |
| `README.md` | Содержит только заголовок `Lognara`. |
| `.gitignore` | Исключает сборки Cargo (`target`) и артефакты агента в корневом `/build/`, резервные файлы rustfmt, PDB, Cargo Mutants, `default.profraw`, `.DS_Store`. |
| `LICENSE` | Текст лицензии MIT. |

Каждый сервис собирается отдельным crate, общего Cargo workspace нет.

## Сквозной поток логов

1. Приложение (или библиотека lognara) отправляет лог на `POST http://127.0.0.1:7400/v1/logs` агента в своём контейнере: text, JSON или бинарные данные.
2. Агент добавляет время приёма, держит записи в памяти и отправляет пачку, как только набрано `LOGNARA_BATCH_SIZE` записей или прошёл `LOGNARA_FLUSH_INTERVAL_MS`.
3. Пачка (MessagePack + zstd, идентификация источника: service, server, backend, environment, service_instance) уходит в lognara-relay на той же машине.
4. Relay разбирает записи в события, группирует их по источнику и раз в `LOGNARA_FLUSH_INTERVAL_MS` (или сразу по `LOGNARA_BATCH_SIZE` событий) отправляет пачку MessagePack + zstd в lognara-core с Bearer-токеном.
5. Пока core недоступен, пачки relay ждут в spool на volume и затем уходят от старых к новым. Core подтверждает пачку после fsync WAL, затем публикует поиск и аналитику по открытым и закрытым сегментам.

Формат пачки агента описан в разделе «Контракт с relay» заметки [[Agent/LognaraAgent]], формат пачки для core — в разделе «Контракт с core» заметки [[Relay/LognaraRelay]].

## Паттерны

- Модули агента и relay разделены по стадиям конвейера: config, ingest, buffer, wire, sender; у relay добавлены normalize и spool.
- Сервисы не делят код: relay держит копию контракта агента, совместимость проверяет фикстура, закодированная агентом.
- Конфигурация читается только из переменных окружения с префиксом `LOGNARA_`, с проверкой при старте.
- Остановка идёт через `CancellationToken`: сначала прекращается приём, затем выполняется финальная отправка.

## Как добавлять компоненты

1. Определи домен по фактической структуре исходников.
2. Добавь заметку компонента в соответствующую папку vault и ссылку на неё в [[Index]].
3. Для крупного компонента (более 10 файлов или сложная структура типов) создай `-ProjectMap.md`.
4. Обнови эту заметку при изменении стека, точек входа или сквозных потоков.
5. Changelog в vault не веди — историю изменений хранит git.

## Хранилище core

[[Core/LognaraCore]] использует WAL, Arrow/Parquet + ZSTD, Tantivy, DataFusion и SQLite-каталог. Приём совместим с relay, время в наносекундах.
