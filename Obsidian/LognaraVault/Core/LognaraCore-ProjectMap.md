# lognara-core — Project Map

Parent: [[Core/LognaraCore]]

| Модуль | Ответственность |
|---|---|
| main, lib | Запуск и сборка сервиса |
| config | Проверяемые переменные окружения |
| wire, wire_budget, model | Бинарный контракт, бюджет десериализации без аллокаций и JSON-представление |
| api | Авторизация и HTTP |
| journal, wal | Долговечный приём и дедупликация |
| catalog | SQLite: сегменты, receipts, checkpoint |
| columns | Arrow/Parquet и физические строки |
| storage | Материализация, снимки, seal, retention |
| index | Индексация и ограниченный кеш читателей |
| query | Проверка фильтров, top-k, чтение строк, HMAC-курсоры |
| analytics | Типизированные агрегаты DataFusion, общий memory pool |
| metrics | Счётчики и gauges Prometheus |
| tests | Контракт, HTTP, WAL, Parquet, поиск, аналитика, retention, аварийные subprocess и настоящие agent/relay и восстановление всей цепочки после недоступности core |
| examples/relay_fixture | Fixture через настоящий encoder relay |
| examples/load | Нагрузочный HTTP-клиент и выборочная проверка свежести от ACK |
| scripts/benchmark.py | Запуск release-стенда, сбор RSS/CPU/диска/метрик |
| docs/operations.md | Конфигурация, TLS-прокси, восстановление и мониторинг |
| docs/benchmark.md | Воспроизведение нагрузочного сценария и критерии |
| docs/benchmark-results.md, .json | Фактические результаты 30 минут и проверки после рестарта |
| docs/review.md | Две оси ревью, исправления и границы проверки |

Поток: API → Journal → WAL → материализатор → Arrow + Tantivy → Parquet + каталог. Запросы используют согласованный Snapshot.
