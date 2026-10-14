import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router";
import { api, ApiError, errorText } from "../api";
import { Sparkline, Template } from "../components/Patterns";
import { contextBounds, sourceQuery, withFilter } from "../logs";
import type { GroupResponse, Issue, IssueDetailsResponse, IssueResponse, IssueStatus, QueryFilters } from "../types";

const PERIODS = {
  "1h": { label: "1 час", milliseconds: 3_600_000, interval: 300 },
  "24h": { label: "24 часа", milliseconds: 86_400_000, interval: 3600 },
  "7d": { label: "7 дней", milliseconds: 604_800_000, interval: 21_600 },
};
type Period = keyof typeof PERIODS;
const TABS: { status: IssueStatus; label: string }[] = [
  { status: "open", label: "Открытые" }, { status: "new", label: "Новые" },
  { status: "regressed", label: "Возвращаются" }, { status: "resolved", label: "Решённые" },
];
const STACK_LINES = 12;
const number = new Intl.NumberFormat("ru-RU");
const date = new Intl.DateTimeFormat("ru-RU", { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" });
const stamp = (value: string) => date.format(new Date(value));

function plural(count: number, forms: [string, string, string]): string {
  const tens = count % 100, ones = count % 10;
  const form = tens >= 11 && tens <= 14 ? forms[2] : ones === 1 ? forms[0] : ones >= 2 && ones <= 4 ? forms[1] : forms[2];
  return `${number.format(count)} ${form}`;
}

/** Статус SQLite Dock и новизна core дают метку строки. */
function badge(issue: Issue): { label: string; tone: string; line: string } {
  if (issue.state.status === "resolved") return { label: "Решена", tone: "done", line: "unknown" };
  if (issue.state.status === "regressed") return { label: "Возвращается", tone: "back", line: "error" };
  if (issue.is_new) return { label: "Новая", tone: "new", line: "error" };
  return { label: "Открыта", tone: "open", line: "warn" };
}

function Title({ issue }: { issue: Pick<Issue, "exception_type" | "parts" | "service" | "action" | "fingerprint"> }) {
  return <span className="issue-title">
    {issue.exception_type ? <code className="pattern-template">{issue.exception_type}</code> : <Template parts={issue.parts} />}
    <small>{issue.service}{issue.action ? ` · ${issue.action}` : ""} · fp {issue.fingerprint.slice(3, 9)}</small>
  </span>;
}

function StackTrace({ details }: { details: IssueDetailsResponse }) {
  const [open, setOpen] = useState(false);
  const latest = details.examples[0];
  const stack = latest?.attributes["exception.stacktrace"];
  const lines = typeof stack === "string" ? stack.split("\n") : [];
  if (!details.issue.exception_type) return <p className="muted">Проблема сгруппирована по шаблону сообщения: события не содержат атрибут exception.type.</p>;
  if (!latest) return <p className="muted">В выбранном периоде событий нет, stack trace последнего случая недоступен.</p>;
  if (!lines.length) return <p className="muted">Приложение не передало exception.stacktrace.</p>;
  return <>
    <pre className="issue-stack">{(open ? lines : lines.slice(0, STACK_LINES)).join("\n")}</pre>
    {lines.length > STACK_LINES && <button type="button" className="issue-fold" aria-expanded={open} onClick={() => setOpen((value) => !value)}>
      {open ? "Свернуть" : `+ ${plural(lines.length - STACK_LINES, ["строка", "строки", "строк"])}`}</button>}
  </>;
}

function History({ details }: { details: IssueDetailsResponse }) {
  const actor = (name: string | null) => name ?? "система";
  return <ul className="issue-history">
    <li><b><time dateTime={details.issue.first_seen} title={details.issue.first_seen}>{stamp(details.issue.first_seen)}</time></b>впервые в известной истории</li>
    {[...details.history].reverse().map((item, index) => <li key={index}>
      <b><time dateTime={item.at} title={item.at}>{stamp(item.at)}</time></b>
      {item.action === "resolved" ? `отмечена решённой · ${actor(item.actor)}` : item.action === "reopened" ? `открыта снова · ${actor(item.actor)}` :
        <>вернулась: случай <time dateTime={item.event_at ?? ""} title={item.event_at ?? ""}>{item.event_at ? stamp(item.event_at) : "—"}</time></>}
    </li>)}
  </ul>;
}

export default function Errors({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [params, setParams] = useSearchParams();
  const status = TABS.some((tab) => tab.status === params.get("status")) ? params.get("status") as IssueStatus : "open";
  const period: Period = Object.hasOwn(PERIODS, params.get("period") ?? "") ? params.get("period") as Period : "24h";
  const service = params.get("service");
  const [reload, setReload] = useState(0);
  const range = useMemo(() => {
    const now = Date.now();
    return { from: new Date(now - PERIODS[period].milliseconds).toISOString(), to: new Date(now).toISOString() };
  }, [period, reload]);
  const filters = useMemo((): QueryFilters => service ? { service: [service] } : {}, [service]);
  const request = { ...range, filters, interval_seconds: PERIODS[period].interval };
  const [response, setResponse] = useState<IssueResponse | null>(null);
  const [services, setServices] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [listError, setListError] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [details, setDetails] = useState<IssueDetailsResponse | null>(null);
  const [detailsError, setDetailsError] = useState("");
  const [pending, setPending] = useState(false);
  const [actionError, setActionError] = useState("");
  const unauthorized = useRef(onUnauthorized); unauthorized.current = onUnauthorized;
  const fail = (cause: unknown, setter: (message: string) => void) => {
    if (cause instanceof ApiError && cause.status === 401) unauthorized.current(); else setter(errorText(cause));
  };

  useEffect(() => {
    const controller = new AbortController();
    setResponse(null); setListError(""); setLoading(true);
    (async () => {
      // Пул аналитики core ограничен: список и каталог сервисов идут последовательно.
      const data = await api<IssueResponse>("POST", "/api/issues/search", { ...request, limit: 100, status }, { signal: controller.signal });
      if (controller.signal.aborted) return;
      setResponse(data);
      const groups = await api<GroupResponse>("POST", "/api/stats/group-by",
        { ...range, filters: { level: ["error", "fatal"] }, group_by: ["service"], limit: 100 }, { signal: controller.signal });
      if (!controller.signal.aborted) setServices(groups.groups.flatMap((group) => group.values.service ?? []));
    })().catch((cause) => { if (!controller.signal.aborted) fail(cause, setListError); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    return () => controller.abort();
  }, [range, filters, status]);

  useEffect(() => {
    const controller = new AbortController();
    setDetails(null); setDetailsError(""); setActionError("");
    if (selected) api<IssueDetailsResponse>("POST", `/api/issues/${selected}/details`, request, { signal: controller.signal })
      .then((data) => { if (!controller.signal.aborted) setDetails(data); })
      .catch((cause) => { if (!controller.signal.aborted) fail(cause, setDetailsError); });
    return () => controller.abort();
  }, [selected, range, filters]);

  const update = (key: string, value: string | null) => {
    setSelected(null);
    setParams((current) => {
      const next = new URLSearchParams(current);
      if (value === null) next.delete(key); else next.set(key, value);
      return next;
    });
  };
  const change = async (fingerprint: string, action: "resolve" | "reopen") => {
    setPending(true); setActionError("");
    try {
      await api("POST", `/api/issues/${fingerprint}/${action}`);
      setReload((value) => value + 1);
    } catch (cause) { fail(cause, setActionError); } finally { setPending(false); }
  };
  const logsLink = (query: Record<string, string>) => `/logs?${new URLSearchParams(query)}`;
  const eventsLink = (issue: Issue) => logsLink({ view: "events", range: period, ...range, q: withFilter("", "fingerprint", issue.fingerprint) });
  const causeLink = (details: IssueDetailsResponse) => {
    const latest = details.examples[0];
    if (!latest) return null;
    return latest.trace_id ? logsLink({ view: "trace", trace_id: latest.trace_id, ...range }) :
      logsLink({ view: "events", q: sourceQuery(latest), ...contextBounds(latest.timestamp) });
  };
  const count = response ? status === "new" ? response.counts.new : response.counts[status] : null;

  return <section className="errors-page page-stack">
    <div className="toolbar issue-toolbar">
      <div className="segmented" role="group" aria-label="Статус">{TABS.map((tab) => {
        const value = response ? tab.status === "new" ? response.counts.new : response.counts[tab.status] : null;
        return <button key={tab.status} type="button" aria-pressed={status === tab.status} onClick={() => update("status", tab.status)}
          title={tab.status === "new" && response?.counts.new === null ? "Новизна не установлена: история индекса неполна" : undefined}>
          {tab.label}<span className="issue-count">{value === null ? "—" : number.format(value)}</span></button>;
      })}</div>
      <label className="inline-label" htmlFor="issue-service">Сервис <select id="issue-service" value={service ?? ""} onChange={(event) => update("service", event.target.value || null)}>
        <option value="">Все сервисы</option>
        {service && !services.includes(service) && <option value={service}>{service}</option>}
        {services.map((name) => <option key={name} value={name}>{name}</option>)}
      </select></label>
      <label className="inline-label" htmlFor="issue-period">Период <select id="issue-period" value={period} onChange={(event) => update("period", event.target.value)}>
        {Object.entries(PERIODS).map(([key, value]) => <option key={key} value={key}>{value.label}</option>)}
      </select></label>
      <button type="button" className="button" disabled={loading} onClick={() => setReload((value) => value + 1)}>Обновить</button>
    </div>
    <div className="summary issue-summary">
      <h1>{response && count !== null ? <>{plural(response.status_events, ["ошибка", "ошибки", "ошибок"])} → <em>{plural(count, ["проблема", "проблемы", "проблем"])}</em></> : "Ошибки"}</h1>
      <p className="muted">error и fatal сгруппированы по fingerprint: тип исключения и фреймы, без исключения — шаблон сообщения.</p>
    </div>
    <p className="page-note">Статус общий для всех пользователей Dock. Решённая проблема возвращается, когда после решения поступает событие со временем не раньше момента решения; опоздавшие события со старым временем её не открывают.</p>
    {response && <p className="page-note" role="status">Обработано до <time dateTime={response.as_of}>{response.as_of}</time> · watermark {number.format(response.watermark)} / {number.format(response.core_watermark)}
      {response.history_complete ? "" : " · история неполна: новизна не установлена"}.</p>}
    {listError && <p className="error" role="alert">{listError}</p>}
    {actionError && <p className="error" role="alert">Не удалось изменить статус: {actionError}</p>}
    <section className="panel table-scroll" aria-label="Проблемы">
      <table className="issues-table"><thead><tr><th>Проблема</th><th>Статус</th><th className="issue-number">Повторов</th>
        <th className="pattern-secondary">Серверы</th><th className="pattern-secondary">Последняя</th><th className="pattern-secondary">{PERIODS[period].label}</th></tr></thead>
        <tbody>{response?.issues.map((issue) => {
          const mark = badge(issue);
          const open = selected === issue.fingerprint;
          return <Fragment key={issue.fingerprint}>
            <tr className={`pattern-row ${open ? "open" : ""}`}>
              <td className="pattern-message"><button type="button" className="row-button" aria-expanded={open}
                onClick={() => setSelected(open ? null : issue.fingerprint)}>
                <span className="caret" aria-hidden="true">{open ? "▾" : "▸"}</span><Title issue={issue} />
              </button></td>
              <td><span className={`issue-status ${mark.tone}`}>{mark.label}</span></td>
              <td className="pattern-number">{number.format(issue.count)}</td>
              <td className="pattern-secondary issue-number">{number.format(issue.servers)}</td>
              <td className="time pattern-secondary"><time dateTime={issue.last_seen} title={issue.last_seen}>{stamp(issue.last_seen)}</time></td>
              <td className="pattern-secondary"><Sparkline buckets={issue.buckets} count={issue.count} tone={mark.line} /></td>
            </tr>
            {open && <tr className="pattern-details"><td colSpan={6}>
              {!details && !detailsError && <p role="status">Загружаем детали проблемы…</p>}
              {detailsError && <p className="error" role="alert">{detailsError}</p>}
              {details && <div className="issue-detail-grid">
                <div><h3>Stack trace последнего случая</h3>
                  {details.examples[0] && <code className="issue-message">{details.examples[0].message}</code>}
                  <StackTrace details={details} />
                  {details.issue.frames.length > 0 && <details className="issue-frames"><summary>Фреймы fingerprint · {details.issue.frames.length}</summary>
                    <ol>{details.issue.frames.map((frame, index) => <li key={index}><code>{frame}</code></li>)}</ol></details>}
                </div>
                <div><h3>Серверы · {number.format(details.issue.servers)}</h3>
                  {details.servers.length ? <ul className="issue-servers">{details.servers.map((server) => <li key={server.value}><code>{server.value}</code><span>{number.format(server.count)}</span></li>)}
                    {details.other_servers > 0 && <li><span className="muted">остальные</span><span>{number.format(details.other_servers)}</span></li>}</ul> : <p className="muted">В периоде событий нет.</p>}
                  <h3>История</h3><History details={details} />
                </div>
                <div className="actions issue-actions">
                  <Link className="button solid" to={eventsLink(issue)}>Все {plural(details.issue.count, ["событие", "события", "событий"])}</Link>
                  {causeLink(details) ? <Link className="button" to={causeLink(details)!} title={details.examples[0]?.trace_id ? "Связанные логи trace последнего случая" : "Логи источника ±5 минут вокруг последнего случая"}>Поиск первопричины</Link> :
                    <button type="button" className="button" disabled title="В периоде нет случаев">Поиск первопричины</button>}
                  {details.state.status === "resolved"
                    ? <button type="button" className="button" disabled={pending} onClick={() => change(issue.fingerprint, "reopen")}>{pending ? "Сохраняем…" : "Открыть снова"}</button>
                    : <button type="button" className="button" disabled={pending} onClick={() => change(issue.fingerprint, "resolve")}>{pending ? "Сохраняем…" : "Отметить решённой"}</button>}
                  {details.state.status === "resolved" && details.state.resolved_at && <p className="page-note">Решена <time dateTime={details.state.resolved_at}>{stamp(details.state.resolved_at)}</time> · {details.state.resolved_by}</p>}
                </div>
              </div>}
            </td></tr>}
          </Fragment>;
        })}</tbody>
      </table>
      {!response?.issues.length && <p className="empty-state" role="status">{loading ? "Загружаем проблемы…" : listError ? "Запрос проблем не выполнен." :
        status === "new" && response?.counts.new === null ? "Новизна не установлена: история индекса неполна." : "Проблем с этим статусом в периоде нет."}</p>}
    </section>
    {response && count !== null && response.issues.length < count && <p className="page-note">Показано {response.issues.length} из {number.format(count)} · максимум 100. Уточните сервис или период.</p>}
  </section>;
}
