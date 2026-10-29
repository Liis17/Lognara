# Проблемы ошибок Dock (этап 5)

База: `45a40ae`. Пользователь выбрал: fingerprint из типа исключения и фреймов с fallback к паттерну сообщения; возврат только по событию, опубликованному после решения и со временем не раньше решения; вкладки «Новые» и «Возвращаются» входят в «Открытые»; проблемы образуют уровни error и fatal.

## Fingerprint v1

`issue_identity::fingerprint(environment, service, action, level, pattern, exception): Option<Fingerprint>`; для уровней ниже error — None. ID `f1_` + SHA-256 (64 lowercase hex).

- Область: точные environment (null отличается от пустой строки), service, action. Server и instance не входят. Уровень отдельно не входит, поэтому error и fatal одного исключения образуют одну проблему.
- Источник исключения — атрибуты OpenTelemetry: `exception.type` и `exception.stacktrace`. Нестроковые значения и некорректный JSON считаются отсутствием исключения. Тип обрезается по краям и ограничен 256 байтами.
- С исключением хеш берётся от типа и первых 5 фреймов. Фрейм — строка `at …` (.NET, Java, JS, Rust) или `File "…"` (Python, порядок разворачивается: первым идёт внутренний вызов). Номера строк и колонок после `:` и `line `, а также адреса `0x…` удаляются, чтобы деплой не разделял проблему. Стек читается до 64 KiB, фрейм — до 512 байт. Фильтрации фреймов фреймворка нет.
- Без `exception.type` хеш берётся от pattern_id сообщения (в нём уже есть level) и action.

Fingerprint вычисляется детерминированно в индексе, агрегатах и каталоге. Tantivy получил точное поле `fingerprint`, поэтому индексы схемы этапа 4 восстанавливаются из Parquet при первом чтении.

## Core

Каталог `patterns.sqlite` версии 3 хранит в таблице `issues` lifetime-метаданные проблем с тем же checkpoint, что и паттерны: scope, тип исключения, фреймы, pattern_id и шаблон первого обработанного события, first/last. Производные таблицы версий 1/2 пересобираются из оставшихся raw событий с актуальной нормализацией; пропуск из-за retention помечает историю неполной. `LOGNARA_PATTERN_MAX_ENTRIES` и `LOGNARA_PATTERN_CATALOG_BYTES` ограничивают паттерны и проблемы вместе. Совместимость обновления описана в [контракте паттернов](DockPatternsContract.md).

`POST /v1/issues/search` (query token):

```ts
interface IssueRequest {
  from: string; to: string;
  filters?: { environment?: string[]; service?: string[]; action?: string[] }; // другие поля дают 400
  interval_seconds?: number; // default 300
  limit?: number; // 1..100, default 100
  status?: "open" | "new" | "regressed" | "resolved"; // default open
  resolved?: string[]; regressed?: string[]; // статусы Dock, до 10000 каждого
}
interface IssueResponse {
  algorithm_version: 1; as_of: string; watermark: number; core_watermark: number;
  history_from: string | null; history_complete: boolean;
  counts: { open: number; new: number | null; regressed: number; resolved: number };
  status_events: number; // события периода по всем проблемам выбранного статуса
  issues: Issue[];
}
interface Issue {
  fingerprint: string; environment: string | null; service: string; action: string | null;
  exception_type: string | null; frames: string[]; pattern_id: string;
  template: string; parts: Part[]; exact: boolean;
  count: number; servers: number; // события и distinct server в периоде
  is_new: boolean | null; first_seen: string; last_seen: string; // lifetime по каталогу
  buckets: { timestamp: string; count: number }[];
}
```

Открытые — проблемы с событиями в периоде вне `resolved`. Новые — открытые с lifetime first_seen в периоде; при неполной истории `counts.new = null` и список пуст. Возвращаются — открытые из `regressed`. Решённые — проблемы из `resolved`, известные каталогу и подходящие под фильтры, включая проблемы без событий в периоде (count 0). Сортировка: решённые по lifetime last_seen DESC, остальные по count DESC, затем fingerprint ASC.

`POST /v1/issues/{fingerprint}/details` (from/to/filters/interval_seconds) возвращает `{as_of, issue, servers: top5 {value,count}, other_servers, examples: до 3 новейших событий}`. Известная каталогу проблема без событий в периоде отдаётся с нулями; неизвестная — 404.

`POST /v1/issues/recurrences {after, issues: [{fingerprint, since}]}` проходит один сегмент raw событий после `after`: error/fatal, timestamp ≥ since соответствующей проблемы. Ответ `{watermark, core_watermark, recurrences: [{fingerprint, first_seen, count}]}`. Пустой `issues` или `after ≥ watermark` сразу возвращает текущий watermark. Если watermark меньше `after`, хранилище core начато заново. Endpoint не зависит от готовности каталога.

Ошибки как у паттернов: 400 invalid, 404 `not found in range`, 503 `pattern index is not ready`/capacity, 504 timeout.

## Dock

Статус общий для экземпляра Dock. SQLite: `issue_states(fingerprint, status open|resolved|regressed, resolved_at, resolved_by, resolved_checkpoint, updated_at)`, `issue_history(fingerprint, action resolved|reopened|regressed, actor, at, event_at)`, `issue_watch(checkpoint)`. Проблема без строки считается открытой. Строк статусов не больше 10000, иначе 409 `issue_limit`.

- `POST /api/issues/search` — тело IssueRequest без resolved/regressed; Dock добавляет статусы из SQLite и возвращает каждую проблему с `state {status, resolved_at, resolved_by}`. Ответ core проверяется: решённые есть только во вкладке решённых, суммы корзин равны count.
- `POST /api/issues/{fingerprint}/details` — детали core, state и до 50 последних действий истории.
- `POST /api/issues/{fingerprint}/resolve` и `/reopen` — 204, требуют Origin. Повторное решение ничего не меняет; открыть снова можно только решённую. Ошибки: 404 `issue_not_found`, 503 `issues_not_ready`.

Решение сохраняет `resolved_checkpoint` = checkpoint наблюдателя. Наблюдатель работает без браузера: каждые 5 секунд проходит сегменты core через recurrences для решённых проблем (since = resolved_at). В одной транзакции он переводит проблему в regressed и пишет историю (actor null, event_at = первый случай), если `resolved_checkpoint ≤ after` и `resolved_at ≤ first_seen`, и сдвигает checkpoint. Поэтому повторное чтение и rebuild каталога не создают возврат, а позднее событие со старым временем не открывает проблему. Если core начал хранилище заново, checkpoint и `resolved_checkpoint` опускаются до нового watermark.

## UI

`/errors` по `errors.dc.html`: вкладки статусов со счётчиками, сервис (top-100 сервисов с error/fatal за период), период 1 ч / 24 ч / 7 д, заголовок «N ошибок → M проблем», таблица со статусом, повторами, серверами, последним случаем и sparkline. Раскрытие: сообщение и stack trace последнего случая (12 строк и разворот), фреймы fingerprint, серверы, история, «Все N событий» (`fingerprint:` в /logs), «Поиск первопричины» (Trace последнего случая или логи источника ±5 минут), «Отметить решённой» / «Открыть снова». Waterfall из шаблона ждёт этапа 8, «Алерт на возврат» — этапа 7. Live недоступен для запроса с `fingerprint`.

Проверки: Rust-тесты fingerprint, групп/статусов/лимита/рестарта/фильтра search+summary, recurrences с поздними событиями и сбросом хранилища, Dock-статусы/Origin/история/наблюдатель/ошибки; Playwright UI и мобильная ширина; сквозной стенд core → Dock → браузер с решением, поздним событием и возвратом.
