# lognara-core — Project Map

Parent: [[Core/LognaraCore]]

| Модуль | Ответственность |
|---|---|
| main, lib | Запуск и сборка сервиса |
| Dockerfile | Release-сборка из корневого контекста, runtime Debian slim от 1000:1000; публикация в [[Repository/Images]] |
| config | Проверяемые переменные окружения, в том числе `LOGNARA_SEGMENT_COMPACT_SECONDS`, `LOGNARA_SERIES_COMPACT_QUIET_SECONDS` и параметры агрегатов `LOGNARA_SERIES_RAW_DAYS`, `LOGNARA_SERIES_HOURLY_DAYS`, `LOGNARA_SERIES_ROLLUP_HOUR_UTC`, `LOGNARA_SERIES_ROLLUP_PAUSE_MS` |
| wire, wire_budget, model | Бинарный контракт, бюджет десериализации без аллокаций и JSON-представление |
| api | Авторизация relay/query/admin и HTTP, включая `/v1/admin/series/*` |
| access | SQLite-реестр relay/агентов, SHA-256 токенов и генерация секретов |
| journal, wal | Долговечный приём и дедупликация |
| catalog | SQLite (user_version 2): сегменты, файлы точек, имена и окружения серий, агрегаты суток (`series_rollups`), определения метрик, receipts, checkpoint |
| columns | Arrow/Parquet и физические строки; запись Parquet через `.tmp` для логов и серий |
| storage | Материализация, точки метрик, снимки, seal, retention, слияние файлов метрик, атомарная замена слитых сегментов логов; запуск и остановка потоков `lognara-rollups`, `lognara-compaction` и каталога паттернов |
| tests/compaction.rs | Слияние окна: поиск, текст, курсор до и после слияния, histogram, старый снимок держит источники, рестарт и дедупликация; окно не сливается до конца |
| index | Индексация, построение индекса из Parquet (`index::build`) и ограниченный кеш читателей с однократной проверкой checksum |
| compaction | Слияние закрытых сегментов логов одного окна (`compaction::select`) и прошедших месяцев файлов точек; поток `lognara-compaction`, запрос замены материализатору (`Replacement`), удаление непринятого результата |
| query | Проверка фильтров, top-k, чтение строк, HMAC-курсоры |
| analytics | Типизированные агрегаты DataFusion, общий memory pool, общий фильтр Arrow/Parquet и предпросмотр правил |
| summary | Единый Snapshot, text-aware count/stacked histogram, p95 duration_ms/coverage, action groups и первая страница |
| pattern_identity | Консервативная нормализация v1 и SHA-256 pattern_id |
| pattern_catalog | Фоновый каталог `patterns.sqlite` v2: checkpoint, полнота истории, lifetime first/last паттернов и проблем |
| patterns | API паттернов и деталей по сырым событиям, UDF `log_pattern_id`, общие окно и сканирование |
| issue_identity | Fingerprint проблемы v1: exception.type и фреймы без позиций, fallback к pattern_id |
| issues | API проблем, деталей и возвратов, UDF `log_issue_fingerprint` |
| telemetry | Heartbeat DTO, проверка возрастов и перевод в часы core |
| status | Версия/ready/WAL, размеры Parquet/индексов и filesystem data_dir |
| tests | retention.rs: ожидание читателей, дедупликация, квота размера удаляет старейшие сегменты; issues.rs: группировка, статусы вкладок, fingerprint в search/summary, рестарт, возвраты после checkpoint; patterns.rs: counts, прошлое окно, скрытие, pattern_id в search/summary, рестарт, поздние события, неполная история и очистка каталога после retention; access.rs: CRUD, изоляция и отзыв токенов; контракт, HTTP, WAL, Parquet, поиск, аналитика, retention, аварийные subprocess и настоящие agent/relay и восстановление всей цепочки после недоступности core |
| series | Схема и запись Parquet точек метрик (`timestamp`, источник, `name`, `origin`, `rule_id`, `labels`, `value`, `event_id`) |
| extract | Точки из `attributes.Metrics` версии 2 и из правил regex |
| scoped_metrics | Индекс источников/серий по серверам и возобновляемая миграция каталога |
| metric_import | Проверка JSON-наборов, конфликты и атомарное применение |
| definitions | Определения метрик, валидация, `RuleSet`, `LabelGuard` (лимит меток) |
| series_query | `GET /v1/series` и `POST /v1/series/query`: корзины, split, `server`, `environment`, приращения, план чтения по суткам (агрегат или точки), до 20 серий |
| rollup | Суточные агрегаты точек: уровни по возрасту, отпечаток, фоновый поток `lognara-rollups`, ночная перепроверка, восстановление при старте |
| metrics | Счётчики и gauges Prometheus, включая `lognara_core_series_*` |
| tests/series.rs | Точки из отчётов, сохранность файлов после рестарта, retention логов не трогает метрики, слияние дня, состояния незавершённого слияния (файлы и каталог), срок хранения метрик и снятие агрегатов |
| tests/crashes.rs | Subprocess с feature `crash-tests`: точки `series_after_parquet`, `series_compact_*` и `segment_compact_*` в списке аварийных точек; тест агрегатов с `rollup_after_parquet` и `rollup_after_mark` |
| tests/series_query.rs | Повтор одного отчёта, суммы по экземплярам, cumulative, gauge, открытый сегмент, список, окружение (фильтр и список), ошибки запросов, токен; план чтения по суткам: агрегаты, смешанные окна, устаревший агрегат, открытые точки суток |
| tests/rollups.rs | Почасовые и 12-часовые корзины, повтор доставки, поздняя точка, смена окна при рестарте, ночная перепроверка, очистка незарегистрированных файлов и строк при старте, точки последних 7 суток без агрегата |
| tests/definitions.rs | Админ-маршруты только с admin-токеном, валидация, правила по сервису, применение к новым событиям, предпросмотр |
| examples/relay_fixture | Fixture через настоящий encoder relay; запись без происхождения (`origin: None`) сохраняет прежний контракт |
| examples/series_bench | Стенд графиков: год файлов серий, построение агрегатов суток и время `series_query::query` по четырём сценариям |
| examples/load | Нагрузочный HTTP-клиент и выборочная проверка свежести от ACK |
| scripts/benchmark.py | Запуск release-стенда, сбор RSS/CPU/диска/метрик |
| docs/operations.md | Конфигурация, TLS-прокси, восстановление и мониторинг |
| docs/benchmark.md | Воспроизведение нагрузочного сценария и критерии |
| docs/benchmark-results.md, .json | Фактические результаты 30 минут и проверки после рестарта |
| docs/review.md | Две оси ревью, исправления и границы проверки |

Поток: API → Journal → WAL → материализатор → Arrow + Tantivy → Parquet + каталог. Запросы используют согласованный Snapshot.

Поток метрик: материализатор → `extract` (native и `RuleSet`) → `LabelGuard` → точки открытого сегмента → Parquet `metrics/` при seal → `series_query` (`/v1/series`, `/v1/series/query`).

Поток агрегатов: `metrics/` → `rollup` (поток `lognara-rollups`, одни сутки за шаг) → `rollups/1h|12h/` и таблица `series_rollups` → `series_query::plan` → `rollup_rows` (агрегаты) и `raw_rows` (точки вне агрегатов и открытый сегмент) → `merge` → ответ `/v1/series/query`.
