import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { api, ApiError, errorText } from "../api";
import { withPatternFilter } from "../logs";
import type { ParsedQuery } from "../logs";
import type { HistogramBucket, MessagePattern, PatternDetailsResponse, PatternPart, PatternRequest, PatternResponse, PatternSort } from "../types";

const number = new Intl.NumberFormat("ru-RU");
const percent = new Intl.NumberFormat("ru-RU", { style: "percent", maximumFractionDigits: 1 });
const date = new Intl.DateTimeFormat("ru-RU", { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit" });
const stamp = (value: string) => date.format(new Date(value));
const growth = (pattern: MessagePattern) => pattern.previous_count === 0 ? "Нет в прошлом окне" :
  `${(pattern.growth_percent ?? 0) > 0 ? "+" : ""}${number.format(pattern.growth_percent ?? 0)}%`;

export function Template({ parts }: { parts: PatternPart[] }) {
  return <code className="pattern-template">{parts.map((part, index) => part.kind === "literal"
    ? <Fragment key={index}>{part.text}</Fragment>
    : <var key={index} className="pattern-variable" title={part.variable_type}>{`<${part.name}>`}</var>)}</code>;
}

/** tone — класс уровня, задающий цвет линии. */
export function Sparkline({ buckets, count, tone }: { buckets: HistogramBucket[]; count: number; tone: string }) {
  const peak = Math.max(1, ...buckets.map((bucket) => bucket.count));
  const points = buckets.map((bucket, index) => `${buckets.length > 1 ? index / (buckets.length - 1) * 120 : 60},${27 - bucket.count / peak * 25}`).join(" ");
  return <svg className={`pattern-spark level-${tone}`} viewBox="0 0 120 28" role="img"
    aria-label={`Динамика: ${number.format(count)} событий, ${buckets.length} корзин`}>
    <title>{buckets.map((bucket) => `${bucket.timestamp}: ${bucket.count}`).join("; ")}</title>
    {buckets.length === 1 ? <circle cx="60" cy={27 - buckets[0].count / peak * 25} r="2" /> :
      <polyline points={points} fill="none" strokeWidth="2" strokeLinejoin="round" />}
  </svg>;
}

function PatternDetails({ details, eventLink }: { details: PatternDetailsResponse; eventLink: string }) {
  const { pattern } = details;
  return <>
    <dl className="pattern-metadata">
      <div><dt>События</dt><dd>{number.format(pattern.count)} · {percent.format(pattern.share)} всех совпадений</dd></div>
      <div><dt>Прошлое окно</dt><dd>{number.format(pattern.previous_count)} · {growth(pattern)}</dd></div>
      <div><dt>Первое в известной истории</dt><dd><time dateTime={pattern.first_seen} title={pattern.first_seen}>{stamp(pattern.first_seen)}</time></dd></div>
      <div><dt>Последнее в диапазоне</dt><dd><time dateTime={pattern.last_seen} title={pattern.last_seen}>{stamp(pattern.last_seen)}</time></dd></div>
      <div><dt>Новизна</dt><dd>{pattern.is_new === null ? "Не установлена: история неполна" : pattern.is_new ? "Новый в выбранном диапазоне" : "Встречался раньше"}</dd></div>
      <div><dt>Область</dt><dd>{pattern.environment === null ? "environment не задан" : `environment: ${JSON.stringify(pattern.environment)}`} · {pattern.service} · {pattern.level.toUpperCase()}</dd></div>
    </dl>
    {pattern.exact && <p className="page-note">Точное сообщение: нормализация не применялась из-за размера или числа переменных. Предпросмотр может быть сокращён; ID учитывает полный текст.</p>}
    <div className="pattern-detail-grid">
      <div><h3>Значения переменных</h3>
        {!details.variables.length && <p className="muted">В этом паттерне нет нормализованных переменных.</p>}
        {details.variables.map((variable) => <div className="pattern-values" key={variable.name}>
          <h4>{variable.name} <span className="muted">· {variable.variable_type} · {number.format(variable.total)} значений</span></h4>
          <ul>{variable.values.map((value) => <li key={value.value}>
            <code>{value.value}</code><span className="pattern-track" aria-hidden="true"><i style={{ width: `${variable.total ? value.count / variable.total * 100 : 0}%` }} /></span>
            <span className="pattern-value-count">{number.format(value.count)} <span className="muted">({percent.format(variable.total ? value.count / variable.total : 0)})</span></span>
          </li>)}</ul>
          <p className="page-note">Остальные значения: {number.format(variable.other_count)}. Топ значений рассчитан по всем событиям этого паттерна в диапазоне.</p>
        </div>)}
      </div>
      <div><h3>Реальные примеры · до 3 событий</h3>
        <div className="pattern-examples">{details.examples.map((event, index) => <details key={`${event.id}:${index}`}>
          <summary><time dateTime={event.timestamp} title={event.timestamp}>{stamp(event.timestamp)}</time><code>{event.message}</code></summary>
          <pre className="json">{JSON.stringify(event, null, 2)}</pre>
        </details>)}</div>
        {!details.examples.length && <p className="muted">Примеров нет.</p>}
        <details className="histogram-data"><summary>Числа динамики</summary><table><thead><tr><th>Время UTC</th><th>События</th></tr></thead>
          <tbody>{pattern.buckets.map((bucket) => <tr key={bucket.timestamp}><td>{bucket.timestamp}</td><td>{number.format(bucket.count)}</td></tr>)}</tbody>
        </table></details>
      </div>
      <div className="actions"><Link className="button solid" to={eventLink}>Все события ({number.format(pattern.count)})</Link></div>
    </div>
    <p className="page-note">Снимок деталей: <time dateTime={details.as_of}>{details.as_of}</time>. Примеры не используются для подсчёта частот.</p>
  </>;
}

interface Props {
  from: string; to: string; range: string; q: string; query: ParsedQuery; error: string;
  intervalSeconds: number; revision: number; onUnauthorized: () => void;
}

export default function Patterns({ from, to, range, q, query, error, intervalSeconds, revision, onUnauthorized }: Props) {
  const [params, setParams] = useSearchParams();
  const sort: PatternSort = ["count", "growth", "last_seen"].includes(params.get("sort") ?? "") ? params.get("sort") as PatternSort : "count";
  const showHidden = params.get("show_hidden") === "true";
  const [response, setResponse] = useState<PatternResponse | null>(null);
  const [hidden, setHidden] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [details, setDetails] = useState<PatternDetailsResponse | null>(null);
  const [detailsLoading, setDetailsLoading] = useState(false);
  const [detailsError, setDetailsError] = useState("");
  const [actionError, setActionError] = useState("");
  const [actionPending, setActionPending] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const actionController = useRef<AbortController | null>(null);
  const unauthorized = useRef(onUnauthorized); unauthorized.current = onUnauthorized;
  const requestKey = JSON.stringify([from, to, query, intervalSeconds]);
  const request = useMemo<PatternRequest>(() => ({ from, to, ...query, interval_seconds: intervalSeconds }), [requestKey]);
  const rangeTooWide = Math.ceil(Date.parse(to) / (intervalSeconds * 1000)) - Math.floor(Date.parse(from) / (intervalSeconds * 1000)) > 10000;
  const validation = error || (rangeTooWide ? "Диапазон превышает 10 000 временных корзин. Сузьте период для паттернов." : "");
  const fail = (cause: unknown, setter: (message: string) => void) => {
    if (cause instanceof ApiError && cause.status === 401) unauthorized.current(); else setter(errorText(cause));
  };
  useEffect(() => {
    const controller = new AbortController();
    setResponse(null); setHidden([]); setListError(""); setSelected(null); setActionError(""); setLoading(!validation);
    if (!validation) (async () => {
      const visibility = await api<{ pattern_ids: string[] }>("GET", "/api/patterns/hidden", undefined, { signal: controller.signal });
      if (controller.signal.aborted) return;
      setHidden(visibility.pattern_ids);
      const data = await api<PatternResponse>("POST", `/api/patterns/search?show_hidden=${showHidden}`, { ...request, limit: 100, sort }, { signal: controller.signal });
      if (!controller.signal.aborted) setResponse(data);
    })().catch((cause) => { if (!controller.signal.aborted) fail(cause, setListError); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [requestKey, sort, showHidden, revision, refresh, validation]);
  useEffect(() => {
    const controller = new AbortController();
    setDetails(null); setDetailsError(""); setDetailsLoading(!!selected);
    if (selected) api<PatternDetailsResponse>("POST", `/api/patterns/${selected}/details`, request, { signal: controller.signal })
      .then((data) => { if (!controller.signal.aborted) setDetails(data); })
      .catch((cause) => { if (!controller.signal.aborted) fail(cause, setDetailsError); })
      .finally(() => { if (!controller.signal.aborted) setDetailsLoading(false); });
    return () => controller.abort();
  }, [selected, requestKey]);
  useEffect(() => () => actionController.current?.abort(), []);
  const update = (key: string, value: string) => setParams((current) => {
    const next = new URLSearchParams(current); next.set(key, value); return next;
  });
  const setVisibility = async (id: string, isHidden: boolean) => {
    if (actionPending) return;
    const controller = new AbortController(); actionController.current = controller;
    setActionPending(id); setActionError("");
    try {
      await api(isHidden ? "DELETE" : "POST", `/api/patterns/${id}/hide`, undefined, { signal: controller.signal });
      if (!controller.signal.aborted) setRefresh((value) => value + 1);
    } catch (cause) { if (!controller.signal.aborted) fail(cause, setActionError); }
    finally { if (!controller.signal.aborted) setActionPending(null); }
  };
  const eventLink = (id: string) => `/logs?${new URLSearchParams({ view: "events", range, from, to, q: withPatternFilter(q, id) })}`;

  return <div className="patterns-view">
    <div className="toolbar pattern-toolbar">
      <label className="inline-label" htmlFor="pattern-sort">Сортировка <select id="pattern-sort" value={sort} onChange={(event) => update("sort", event.target.value)}>
        <option value="count">По числу событий</option><option value="growth">По росту</option><option value="last_seen">По последнему событию</option>
      </select></label>
      <label className="inline-label"><input type="checkbox" checked={showHidden} onChange={(event) => update("show_hidden", String(event.target.checked))} />Показывать скрытые</label>
      <button type="button" className="button" disabled={loading || !!validation} onClick={() => setRefresh((value) => value + 1)}>Обновить паттерны</button>
    </div>
    <div className="summary"><h1>Паттерны</h1>{response && <div className="chips">
      <span className="chip">{number.format(response.total_events)} событий</span><span className="chip">{number.format(response.total_patterns)} паттернов</span>
      <span className="chip">Новых: {response.new_patterns === null ? "не установлено" : number.format(response.new_patterns)}</span>
    </div>}</div>
    <p className="page-note">Период: <time dateTime={from} title={from}>{Number.isFinite(Date.parse(from)) ? stamp(from) : from}</time> → <time dateTime={to} title={to}>{Number.isFinite(Date.parse(to)) ? stamp(to) : to}</time> · {Intl.DateTimeFormat().resolvedOptions().timeZone}.</p>
    <p className="page-note">Доли считаются от всех совпавших событий, включая скрытые паттерны. Сравнение — с соседним прошлым окном такой же длины и тем же запросом. Отсутствие в прошлом окне не означает новизну.</p>
    <p className="page-note">Паттерны показывают историю. Live недоступен: группировка выполняется сервером по обработанному снимку.</p>
    {response && <div className="pattern-coverage" role="status">
      <p>Известная история: {response.history_from ? <time dateTime={response.history_from} title={response.history_from}>{stamp(response.history_from)}</time> : "ещё не обработана"} · {response.history_complete ? "полная" : "неполная: новизна части паттернов не установлена"}.</p>
      <p>Обработано до <time dateTime={response.as_of}>{response.as_of}</time> · watermark {number.format(response.watermark)} / {number.format(response.core_watermark)}.
        {response.core_watermark > response.watermark ? ` Есть отставание индекса: ${number.format(response.core_watermark - response.watermark)} публикаций.` : " Индекс догнал доступные публикации."}</p>
    </div>}
    {(validation || listError) && <p className="error" role="alert">{validation || listError}</p>}
    {actionError && <p className="error" role="alert">Не удалось изменить видимость: {actionError}</p>}
    <section className="panel table-scroll" aria-label="Паттерны сообщений">
      <table className="patterns-table"><thead><tr><th>Уровень</th><th>Паттерн сообщения</th><th>Событий</th>
        <th className="pattern-secondary">Доля</th><th className="pattern-secondary">Рост</th><th className="pattern-secondary">Динамика</th></tr></thead>
        <tbody>{response?.patterns.map((pattern) => <Fragment key={pattern.pattern_id}>
          <tr className={`pattern-row level-${pattern.level} ${selected === pattern.pattern_id ? "open" : ""}`}>
            <td><span className={`level level-${pattern.level}`}>{pattern.level.toUpperCase()}</span></td>
            <td className="pattern-message"><button type="button" className="row-button" aria-expanded={selected === pattern.pattern_id}
              onClick={() => setSelected((current) => current === pattern.pattern_id ? null : pattern.pattern_id)}>
              <span className="caret" aria-hidden="true">{selected === pattern.pattern_id ? "▾" : "▸"}</span><Template parts={pattern.parts} />
            </button><div className="pattern-source">{pattern.service}{hidden.includes(pattern.pattern_id) && <span className="chip">Скрыт</span>}
              {pattern.is_new === true && <span className="pattern-new">Новый</span>}{pattern.is_new === null && <span className="muted">Новизна неизвестна</span>}</div></td>
            <td className="pattern-number">{number.format(pattern.count)}</td>
            <td className="pattern-secondary"><div className="pattern-share"><span className="pattern-track" aria-hidden="true"><i style={{ width: `${pattern.share * 100}%` }} /></span><span>{percent.format(pattern.share)}</span></div></td>
            <td className="pattern-secondary pattern-growth">{growth(pattern)}</td><td className="pattern-secondary"><Sparkline buckets={pattern.buckets} count={pattern.count} tone={pattern.level} /></td>
          </tr>
          {selected === pattern.pattern_id && <tr className="pattern-details"><td colSpan={6}>
            {detailsLoading && <p role="status">Загружаем детали паттерна…</p>}
            {detailsError && <p className="error" role="alert">{detailsError}</p>}
            {details && <PatternDetails details={details} eventLink={eventLink(pattern.pattern_id)} />}
            <button type="button" className="button" disabled={actionPending !== null} onClick={() => setVisibility(pattern.pattern_id, hidden.includes(pattern.pattern_id))}>
              {actionPending === pattern.pattern_id ? "Сохраняем…" : hidden.includes(pattern.pattern_id) ? "Показать паттерн" : "Скрыть паттерн"}
            </button>
          </td></tr>}
        </Fragment>)}</tbody>
      </table>
      {!response?.patterns.length && <p className="empty-state" role="status">{loading ? "Загружаем паттерны…" : validation || listError ? "Запрос паттернов не выполнен." :
        response && response.visible_patterns === 0 && response.total_patterns > 0 ? "Все совпавшие паттерны скрыты. Включите «Показывать скрытые»." : "В выбранном диапазоне паттернов нет."}</p>}
    </section>
    {response && <p className="page-note">Показано {response.patterns.length} из {number.format(response.visible_patterns)} видимых паттернов · максимум 100.
      {response.visible_patterns > response.patterns.length ? " Измените запрос, чтобы найти остальные." : ""} Алгоритм v{response.algorithm_version}.</p>}
  </div>;
}
