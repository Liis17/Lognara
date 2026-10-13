# lognara-core — Project Map

Parent: [[Core/LognaraCore]]

| Модуль | Ответственность |
|---|---|
| main, lib | Запуск и сборка сервиса |
| config | Проверяемые переменные окружения, в том числе `LOGNARA_ADMIN_TOKEN` и `LOGNARA_SERIES_COMPACT_QUIET_SECONDS` |
| wire, wire_budget, model | Бинарный контракт, бюджет десериализации без аллокаций и JSON-представление |
| api | Авторизация и HTTP; `/v1/admin/series/*` только при admin-токене |
| journal, wal | Долговечный приём и дедупликация |
| catalog | SQLite (user_version 2): сегменты, файлы точек и имена серий, определения метрик, receipts, checkpoint |
| columns | Arrow/Parquet и физические строки; запись Parquet через `.tmp` для логов и серий |
| storage | Материализация, точки метрик, снимки, seal, retention, слияние файлов метрик |
| index | Индексация и ограниченный кеш читателей |
| query | Проверка фильтров, top-k, чтение строк, HMAC-курсоры |
| analytics | Типизированные агрегаты DataFusion, общий memory pool, предпросмотр правил |
| series | Схема и запись Parquet точек метрик (`timestamp`, источник, `name`, `origin`, `rule_id`, `labels`, `value`, `event_id`) |
| extract | Точки из `attributes.Metrics` версии 2 и из правил regex |
| definitions | Определения метрик, валидация, `RuleSet`, `LabelGuard` (лимит меток) |
| series_query | `GET /v1/series` и `POST /v1/series/query`: корзины, split, приращения, до 20 серий |
| metrics | Счётчики и gauges Prometheus, включая `lognara_core_series_*` |
| tests | Контракт, HTTP, WAL, Parquet, поиск, аналитика, retention, аварийные subprocess и настоящие agent/relay и восстановление всей цепочки после недоступности core |
| tests/series.rs | Точки из отчётов, сохранность файлов после рестарта, retention не трогает метрики, слияние дня, состояния незавершённого слияния (файлы и каталог) |
| tests/crashes.rs | Subprocess с feature `crash-tests`: точки `series_after_parquet` и `series_compact_*` в списке аварийных точек |
| tests/series_query.rs | Повтор одного отчёта, суммы по экземплярам, cumulative, gauge, открытый сегмент, список, ошибки запросов, токен |
| tests/definitions.rs | Админ-маршруты с токеном и без него, валидация, правила по сервису, применение к новым событиям, предпросмотр |
| examples/relay_fixture | Fixture через настоящий encoder relay |
| examples/series_bench | Стенд графиков: год файлов серий и время `series_query::query` по четырём сценариям |
| examples/load | Нагрузочный HTTP-клиент и выборочная проверка свежести от ACK |
| scripts/benchmark.py | Запуск release-стенда, сбор RSS/CPU/диска/метрик |
| docs/operations.md | Конфигурация, TLS-прокси, восстановление и мониторинг |
| docs/benchmark.md | Воспроизведение нагрузочного сценария и критерии |
| docs/benchmark-results.md, .json | Фактические результаты 30 минут и проверки после рестарта |
| docs/review.md | Две оси ревью, исправления и границы проверки |

Поток: API → Journal → WAL → материализатор → Arrow + Tantivy → Parquet + каталог. Запросы используют согласованный Snapshot.

Поток метрик: материализатор → `extract` (native и `RuleSet`) → `LabelGuard` → точки открытого сегмента → Parquet `metrics/` при seal → `series_query` (`/v1/series`, `/v1/series/query`).
