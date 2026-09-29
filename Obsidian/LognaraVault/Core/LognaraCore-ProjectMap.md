# lognara-core — Project Map

Parent: [[Core/LognaraCore]]

| Модуль | Ответственность |
|---|---|
| main, lib | Запуск и сборка сервиса |
| config | Проверяемые переменные окружения |
| wire, model | Бинарный контракт и JSON-представление |
| api | Авторизация и HTTP |
| journal, wal | Долговечный приём и дедупликация |
| catalog | SQLite: сегменты, receipts, checkpoint |
| columns | Arrow/Parquet и физические строки |
| storage | Материализация, снимки, seal, retention |
| index | Индексация и ограниченный кеш читателей |
| query | Проверка фильтров, top-k, чтение строк, HMAC-курсоры |
| metrics | Счётчики и gauges Prometheus |
| tests | Контракт, HTTP, WAL, Parquet, восстановление |
| examples/relay_fixture | Fixture через настоящий encoder relay |

Поток: API → Journal → WAL → материализатор → Arrow + Tantivy → Parquet + каталог. Запросы используют согласованный Snapshot.
