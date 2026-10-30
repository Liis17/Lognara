# Серверные паттерны Dock (этап 4)

База: `587bffc`. Пользователь выбрал консервативную нормализацию: сохранять слова, HTTP-статусы и коды ошибок; заменять UUID, IP, длинные hex-ID, явно обозначенные ID и длительности. SQL извне не добавляется. Паттерны не являются проблемами с lifecycle (этап 5) и не создают алерты (этап 7).

## Идентичность v1

Область: точные environment (null отличается от пустой строки), service и level; server/instance/action не входят в идентичность. SHA-256 канонического представления версии, области и структурных частей шаблона, ID `p1_` + 64 lowercase hex. Литералы и переменные различаются в хеше даже при одинаковом отображении. Регистр и переносы строк литералов сохраняются; произвольные слова не заменяются. Многострочное сообщение — один паттерн.

`pattern_identity::identify(environment: Option<&str>, service: &str, level: &str, message: &str): Identity` — общий алгоритм индекса, агрегатов и worker. Identity: id, template, parts, variables, exact. Part: `{kind:"literal",text}` или `{kind:"variable",name,variable_type}`; Variable: `{name,variable_type,value}`. Имена переменных стабильны по позиции (`ip_1`, `uuid_1`, `id_1`, `duration_1`, `hex_1`, `timestamp_1`, `number_1`). Типы: ip/uuid/id/duration/hex/timestamp/number.

Для nginx access-log стандартного формата время в квадратных скобках становится `timestamp`, размер ответа — `number`; метод, URL, версия HTTP и статус остаются литералами. Для полного сообщения `ServiceMetrics { ServiceName: "...", Metrics: [("name": value), ...], Timestamp: MM/dd/yyyy HH:mm:ss }` числовые значения метрик становятся `number`, время — `timestamp`. Названия сервиса и метрик сохраняются. Числа в обычном тексте не заменяются. Правила не зависят от имени сервиса в источнике. Изменения объединяют ID затронутых сообщений; остальные ID и формат `p1_` сохраняются.

Пределы: сообщение свыше 16 KiB или более 64 переменных получает точный hash полного текста без нормализации (`exact:true`), preview ограничен 4096 UTF-8 bytes, никакие разные длинные сообщения не объединяются из-за усечения. В обычном случае template/parts также являются preview, identity считается до усечения. Литеральный `<ip_1>` не равен переменной.

## Данные и публикации

Отдельный производный SQLite-каталог core хранит version, checkpoint sequence, processed as_of, полноту истории и lifetime first_seen/last_seen паттерна. Worker читает новые видимые sequence из удерживаемого Snapshot Arrow/Parquet, фиксирует метаданные и checkpoint одной транзакцией. Перезапуск не учитывает публикацию дважды. Новые публикации получаются через внутренний серверный cursor по sequence/watermark, а не через SSE браузера. Граница доступных исходных данных сохраняется; пропуски из-за retention явно помечают историю неполной. Прочтение уже сохранённых сегментов создаёт backfill. Пока у паттерна есть хранимые события, lifetime first_seen сохраняется и после raw retention. Запись, last_seen которой раньше самого старого хранимого события, удаляется при старте и после каждого удаления данных retention, в том числе без нового приёма: у неё не осталось строк, а лимиты освобождаются. Если такой паттерн появится снова, он считается новым. Rebuild из оставшихся raw не обещает восстановить удалённую историю.

Counts, shares, previous window и buckets вычисляются по сырым событиям выбранного диапазона в core через общий DataFusion pool, не по сохранённым примерам. Один запрос занимает analytics slot, имеет deadline и ограничение памяти. Каталог также имеет явные лимиты кардинальности/размера; превышение даёт явную ошибку индекса паттернов, не потерю событий и не частичные числа. API использует обработанный watermark, не новейший ещё не обработанный, и сообщает lag. Идентичность доступна в точном filters.pattern_id (один/несколько ID по обычным правилам filters) в search/summary; индексы старой схемы восстанавливаются штатно из Parquet. Raw Arrow/Parquet схема не меняется.

## Core API

`POST /v1/patterns/search` (query token):

```ts
interface PatternRequest {
  from: string; to: string;
  filters?: Record<string,string[]>; text?: TextQuery;
  interval_seconds?: number; // default 300, 1..86400, <=10000 buckets
  limit?: number; // default100, 1..100
  sort?: "count"|"growth"|"last_seen"; // defaultcount
  exclude_patterns?: string[]; // <=1000 valid IDs, list visibility only
}
interface PatternResponse {
  algorithm_version: 1;
  as_of: string;
  watermark: number;
  core_watermark: number;
  history_from: string|null;
  history_complete: boolean;
  total_events: number; // all matches, including hidden patterns
  total_patterns: number; // all matches
  visible_patterns: number; // after exclude_patterns
  new_patterns: number|null; // visible, null when history incomplete
  patterns: Pattern[];
}
interface Pattern {
  pattern_id: string;
  environment: string|null; service: string; level: LogLevel;
  template: string; parts: Part[]; exact: boolean;
  count: number; share: number; // count/total_events 0..1
  previous_count: number;
  growth_percent: number|null; // null when previous_count=0
  is_new: boolean|null; // lifetime first_seen in [from,to), null if not provable
  first_seen: string; // lifetime within known history
  last_seen: string; // selected range
  buckets: {timestamp:string,count:number}[];
}
```

Previous window — соседнее окно той же длины и с теми же filters/text. Сортировка count DESC / growth DESC (нулевая база первой) / last_seen DESC, затем pattern_id ASC. Отсутствие событий в прошлом окне называется «нет в прошлом окне», это не доказательство новизны. `is_new:false` допустим при first_seen раньше from даже при неполной истории; иначе при неполной истории null. Гистограмма и текущие/предыдущие counts используют один обработанный Snapshot. Top limit явно показан и не меняет denominator/total_patterns. История scopes не зависит от текущих фильтров или размера страницы.

`POST /v1/patterns/{pattern_id}/details`: from/to/filters/text/interval_seconds (без limit/sort/exclude). Ответ `{as_of,pattern,variables,examples}`. variables: `[{name,variable_type,total,values:[{value,count}],other_count}]`, top5 значений по count DESC/value ASC, не выборка; остальные суммируются в other_count. Не более64 переменных, заданный общий лимит distinct/bytes, превышение — явный503. Примеры — до3 полных событий с теми же условиями, не источник counts. Идентификатор не найден в диапазоне =>404. `service_instance` можно показывать отдельным измерением с теми же честными counts, если реализовано.

Ошибки:400 invalid,404 notfound,503 capacity/storage/index unavailable,504 timeout. Никаких частичных успешных ответов с недосчитанными данными. Summary/search продолжают работать без готовности фонового каталога (pattern_id вычисляется детерминированно), а patterns API сообщает ещё не готовую историю явно.

## Dock и персональное скрытие

Те же POST-маршруты под `/api/patterns` с AuthUser, Origin, no-store, query Bearer и прежним безопасным mapping ошибок. Search принимает query `show_hidden` (bool, defaultfalse); Dock добавляет персонально скрытые ID в exclude_patterns, при show_hidden=true не добавляет. Тело — core PatternRequest. Details не исключает скрытый ID.

SQLite dock `hidden_patterns(username,pattern_id,created_at)`; максимум1000 на пользователя. `GET /api/patterns/hidden` =>`{pattern_ids:string[]}`; `POST /api/patterns/{id}/hide`, `DELETE /api/patterns/{id}/hide` =>204, идемпотентно. Список и скрытие относятся только к текущему пользователю, изменения требуют Origin; неизвестный/невалидный ID нельзя использовать как SQL. Ограничение1000=>409 pattern_limit. Secrets и core admin token браузеру не передаются.

## UI

Режим `/logs?view=patterns` в существующей панели с q/range/from/to, сортировкой и show_hidden в URL. Отдельный компонент Patterns и общий query parser; существующие events/Trace/Live/экспорт сохраняются. Паттерны используют history, Live для них недоступен с объяснением. Отображать количество событий/паттернов/новых, верхние100/visible_patterns, доли, рост прошлого окна, sparkline, first_seen, loading/empty/error/processing lag/history coverage. Доля считается от всех совпавших событий, включая скрытые; это объяснено.

Раскрытие — серверные details: top переменных/other, реальные примеры, точный переход «Все события» с pattern_id и сохранением q/time. Литеральный текст и значения рендерятся как текст. Скрыть/показать с восстановлением после перезагрузки и обработкой ошибок. «Алерт» и «Проблема» не обещают работающего backend до соответствующих этапов; скрыть эти действия или явно подписать недоступность. KPI аналитики по паттернам допустим только на тех же серверных данных, не блокирует этот этап.

Проверки: консервативные правила/границы/Unicode/коды/многострочность, hash stability и защита усечения; restart/checkpoint/late events/retention/rebuild, одинаковые counts при разных limits, previous/new/coverage, filters.pattern_id search+summary/cursor, permissions/Origin/скрытие/изоляция; UI list/details/links/hide/reload/error/mobile и реальный core→Dock→browser.

## Уточнения реализации

- Worker обрабатывает по одному закрытому сегменту после checkpoint (затем открытый), не занимает analytics slot пользовательских запросов и при остановке или deadline не фиксирует checkpoint.
- Неготовый каталог: core 503 `{"error":"pattern index is not ready"}`, Dock 503 `patterns_not_ready`. Отказ каталога или превышение бюджета запроса: 503 `query_unavailable`.
- Примеры details: до 3 новейших событий по (timestamp, sequence) DESC. Значения переменных и примеры берутся только из текущего окна.
- limit × число корзин × 64 байта не должно превышать `LOGNARA_PATTERN_QUERY_BYTES`, иначе 400.
- Live недоступен в режиме паттернов и для запроса с `pattern_id`: браузер не вычисляет идентичность.
- Каталог версии 3 хранит также проблемы ([контракт](DockIssuesContract.md)); версии 1/2 атомарно сбрасывают только производные таблицы и пересобираются из сохранённых raw-событий. Tantivy commit payload содержит ревизию нормализации; старые индексы перестраиваются из Parquet при первом поиске. Raw, WAL и схема событий не меняются. После обновления backfill и первый поиск по старым сегментам могут занять больше времени. Ранее скрытые ID и статусы проблем на основе изменившихся ID автоматически не переносятся; lifetime-история за пределами сохранённых raw не восстанавливается. Обновляйте core и Dock вместе из-за новых типов переменных. Старый core не открывает каталог версии 3. Текст 404 core — `not found in range`.
- Ошибка переполнения каталога в журнале core содержит число паттернов, проблем, учтённые байты и оба настроенных лимита.
