import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { api, ApiError, errorText } from "../api";
import Histogram from "../components/Histogram";
import Patterns from "../components/Patterns";
import { appendHistory, contextBounds, eventBounds, eventsToCsv, LOG_LEVELS, matchesQuery, MAX_LIVE_ROWS,
  parseQuery, prependLive, sourceQuery } from "../logs";
import type { ParsedQuery } from "../logs";
import type { HistogramResponse, LogEvent, SearchRequest, SearchResponse, SummaryResponse } from "../types";

const RANGES = { "1h": 3_600_000, "24h": 86_400_000, "7d": 604_800_000 };
type Range = keyof typeof RANGES;
type Status = "connecting" | "live" | "core_error" | "disconnected";
const STATUS_TEXT: Record<Status, string> = {
  connecting: "Подключение…", live: "Соединение открыто",
  core_error: "Core недоступен", disconnected: "Соединение закрыто",
};
const TRACE_ID = /^[a-fA-F0-9]{32}$/;
const emptyPage: SearchResponse = { events: [], next_cursor: null, as_of: "" };
const clock = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", second: "2-digit", hour12: false });
const calendar = new Intl.DateTimeFormat("ru-RU", { day: "2-digit", month: "2-digit" });

function eventTime(timestamp: string): string {
  const fraction = timestamp.match(/\.(\d+)/)?.[1].slice(0, 3).padEnd(3, "0") ?? "000";
  return `${clock.format(new Date(timestamp))}.${fraction}`;
}

function periodLabel(timestamp: string): string {
  return Number.isFinite(Date.parse(timestamp)) ? `${calendar.format(new Date(timestamp))} ${clock.format(new Date(timestamp))}` : "Некорректное время";
}

function EventDetails({ event, onContext, onTrace, onCopy }: {
  event: LogEvent;
  onContext: (event: LogEvent) => void;
  onTrace: (event: LogEvent) => void;
  onCopy: (event: LogEvent) => void;
}) {
  const fields = ["id", "timestamp", "ingested_at", "environment", "server", "backend", "service",
    "service_instance", "action", "trace_id", "span_id", "parent_span_id", "request_id"] as const;
  return <div className="detail-grid">
    <div><h3>Поля</h3><dl className="fields">
      {fields.map((field) => <Fragment key={field}><dt>{field}</dt><dd>{event[field] ?? "—"}</dd></Fragment>)}
    </dl></div>
    <div><h3>JSON</h3><pre className="json">{JSON.stringify(event, null, 2)}</pre></div>
    <div className="actions">
      <button type="button" className="button solid" onClick={() => onContext(event)}>Контекст ±5 мин</button>
      <button type="button" className="button" disabled={!event.trace_id}
        title={event.trace_id ? "Связанные логи по trace_id" : "У события нет trace_id"}
        onClick={() => onTrace(event)}>Открыть Trace</button>
      <button type="button" className="button" onClick={() => onCopy(event)}>Копировать ссылку</button>
    </div>
  </div>;
}

export default function Logs({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [params, setParams] = useSearchParams();
  const paramsKey = params.toString();
  // Границы фиксируются в URL для всех страниц поиска.
  const location = useMemo(() => {
    const range: Range = Object.hasOwn(RANGES, params.get("range") ?? "") ? params.get("range") as Range : "1h";
    const now = Date.now();
    return {
      view: params.get("view") === "trace" || params.get("view") === "patterns" ? params.get("view") as "trace" | "patterns" : "events" as const,
      q: params.get("q") ?? "", range,
      from: params.get("from") ?? new Date(now - RANGES[range]).toISOString(),
      to: params.get("to") ?? new Date(now).toISOString(),
      traceId: params.get("trace_id") ?? "", selected: params.get("selected"),
      selectedTimestamp: params.get("selected_timestamp"),
      occurrence: /^\d{1,6}$/.test(params.get("selected_occurrence") ?? "") ? Number(params.get("selected_occurrence")) : 0,
    };
  }, [paramsKey]);
  const queryKey = JSON.stringify([location.view, location.q, location.range, location.from, location.to, location.traceId]);
  const parsed = useMemo((): { query: ParsedQuery; error: string } => {
    if (!(Number.isFinite(Date.parse(location.from)) && Number.isFinite(Date.parse(location.to))) ||
        Date.parse(location.from) >= Date.parse(location.to)) {
      return { query: { filters: {} }, error: "Диапазон времени в ссылке некорректен. Выберите период заново." };
    }
    if (location.view === "trace" && location.q) return {
      query: { filters: {} }, error: "Trace принимает только trace_id. Для фильтров запроса перейдите в режим «События».",
    };
    if (location.view === "trace") return TRACE_ID.test(location.traceId) ?
      { query: { filters: { trace_id: [location.traceId.toLowerCase()] } }, error: "" } :
      { query: { filters: {} }, error: "Введите Trace ID: 32 шестнадцатеричных символа." };
    try { return { query: parseQuery(location.q), error: "" }; }
    catch (error) { return { query: { filters: {} }, error: error instanceof Error ? error.message : String(error) }; }
  }, [queryKey]);
  const [draft, setDraft] = useState(location.view === "trace" ? location.traceId : location.q);
  const [page, setPage] = useState<SearchResponse>(emptyPage);
  const [loading, setLoading] = useState(!parsed.error);
  const [paging, setPaging] = useState(false);
  const [searchError, setSearchError] = useState("");
  const [expired, setExpired] = useState(false);
  const [revision, setRevision] = useState(0);
  const [summary, setSummary] = useState<SummaryResponse | null>(null);
  const [histogram, setHistogram] = useState<HistogramResponse | null>(null);
  const [histogramLoading, setHistogramLoading] = useState(false);
  const [histogramError, setHistogramError] = useState("");
  const [selectedEvent, setSelectedEvent] = useState<LogEvent | null>(null);
  const [selectionNote, setSelectionNote] = useState("");
  const [liveEnabled, setLiveEnabled] = useState(false);
  const [liveRows, setLiveRows] = useState<LogEvent[]>([]);
  const [status, setStatus] = useState<Status>("connecting");
  const [paused, setPaused] = useState(false);
  const [pendingCount, setPendingCount] = useState(0);
  const [liveNotice, setLiveNotice] = useState("");
  const [copyNote, setCopyNote] = useState("");
  const [exportsOpen, setExportsOpen] = useState(false);
  const pausedRef = useRef(false);
  const pending = useRef<LogEvent[]>([]);
  const generation = useRef(0);
  const pagingAbort = useRef<AbortController | null>(null);
  const unauthorized = useRef(onUnauthorized);
  unauthorized.current = onUnauthorized;
  const intervalSeconds = Math.min(86400, Math.max(1, Math.ceil((Date.parse(location.to) - Date.parse(location.from)) / 1000 / 60)));
  const intervalMillis = intervalSeconds * 1000;
  // Date.parse отбрасывает наносекунды: верхнюю границу округляем вверх для безопасной проверки лимита.
  const toFraction = location.to.match(/\.(\d+)/)?.[1] ?? "";
  const upperMillis = Date.parse(location.to) + (/[1-9]/.test(toFraction.slice(3)) ? 1 : 0);
  const bucketCount = Math.ceil(upperMillis / intervalMillis) - Math.floor(Date.parse(location.from) / intervalMillis);
  const summaryUnavailable = location.view === "events" && bucketCount > 10000;
  const rows = liveEnabled ? liveRows : page.events;
  const selectedIndex = rows.reduce<number[]>((indices, event, index) => {
    if (event.id === location.selected && (!location.selectedTimestamp || event.timestamp === location.selectedTimestamp)) indices.push(index);
    return indices;
  }, [])[location.occurrence] ?? -1;
  const fixedRange = Date.parse(location.to) - Date.parse(location.from) !== RANGES[location.range];

  const updateLocation = (values: Record<string, string | null>, replace = false) => {
    setParams((current) => {
      const next = new URLSearchParams(current);
      for (const [key, value] of Object.entries(values)) {
        if (value === null || value === "") next.delete(key); else next.set(key, value);
      }
      if (values.selected === null) { next.delete("selected_timestamp"); next.delete("selected_occurrence"); }
      return next;
    }, { replace });
  };
  useEffect(() => {
    if (!params.has("from") || !params.has("to") || !params.has("view") || !params.has("range")) {
      updateLocation({ from: location.from, to: location.to, view: location.view, range: location.range }, true);
    }
  }, [paramsKey]);
  useEffect(() => {
    setDraft(location.view === "trace" ? location.traceId : location.q); setCopyNote("");
  }, [location.q, location.view, location.traceId]);

  const requestPage = (signal: AbortSignal, cursor?: string) => {
    if (location.view === "trace") {
      const query = new URLSearchParams({ from: location.from, to: location.to, limit: "50" });
      if (cursor) query.set("cursor", cursor);
      return api<SearchResponse>("GET", `/api/traces/${encodeURIComponent(location.traceId.toLowerCase())}?${query}`, undefined, { signal });
    }
    const request: SearchRequest = {
      from: location.from, to: location.to, ...parsed.query, limit: 50, ...(cursor ? { cursor } : {}),
    };
    return api<SearchResponse>("POST", "/api/logs/search", request, { signal });
  };
  const handleError = (error: unknown) => {
    if (error instanceof ApiError && error.status === 401) unauthorized.current();
    else { setExpired(error instanceof ApiError && error.status === 410); setSearchError(errorText(error)); }
  };
  useEffect(() => {
    const current = ++generation.current;
    const controller = new AbortController();
    pagingAbort.current?.abort();
    // Паттерны загружает отдельный компонент со своим серверным запросом.
    const enabled = !parsed.error && location.view !== "patterns";
    setPage(emptyPage); setSummary(null); setSearchError(""); setExpired(false); setPaging(false); setLoading(enabled);
    if (enabled) {
      const request = location.view === "events" && !summaryUnavailable
        ? api<SummaryResponse>("POST", "/api/stats/summary", {
          from: location.from, to: location.to, ...parsed.query,
          interval_seconds: intervalSeconds, search_limit: 50,
        }, { signal: controller.signal }).then((response) => {
          if (!controller.signal.aborted && current === generation.current) {
            setSummary(response); setPage(response.page ?? emptyPage);
          }
        })
        : requestPage(controller.signal).then((response) => {
          if (!controller.signal.aborted && current === generation.current) setPage(response);
        });
      request.catch((error) => {
        if (!controller.signal.aborted && current === generation.current) handleError(error);
      }).finally(() => { if (!controller.signal.aborted && current === generation.current) setLoading(false); });
    }
    return () => { controller.abort(); pagingAbort.current?.abort(); };
  }, [queryKey, revision]);
  useEffect(() => {
    const controller = new AbortController();
    setHistogram(null); setHistogramError("");
    const enabled = !parsed.error && location.view === "trace";
    setHistogramLoading(enabled);
    if (enabled) api<HistogramResponse>("POST", "/api/stats/histogram", {
      from: location.from, to: location.to, filters: parsed.query.filters, interval_seconds: intervalSeconds,
    }, { signal: controller.signal }).then((response) => {
      if (!controller.signal.aborted) setHistogram(response);
    }).catch((error) => {
      if (controller.signal.aborted) return;
      if (error instanceof ApiError && error.status === 401) unauthorized.current(); else setHistogramError(errorText(error));
    }).finally(() => { if (!controller.signal.aborted) setHistogramLoading(false); });
    return () => controller.abort();
  }, [queryKey, revision]);

  // Ссылки на последующие страницы восстанавливают событие отдельным точным поиском.
  useEffect(() => {
    const controller = new AbortController();
    setSelectedEvent(null); setSelectionNote("");
    const selected = location.selected;
    if (!selected || parsed.error || liveEnabled || loading || selectedIndex !== -1 || location.view === "patterns") {
      return () => controller.abort();
    }
    setSelectionNote("Загружаем событие из ссылки…");
    let bounds = { from: location.from, to: location.to };
    try {
      if (location.selectedTimestamp) bounds = eventBounds(location.selectedTimestamp);
    } catch (error) {
      setSelectionNote(errorText(error)); return () => controller.abort();
    }
    api<SearchResponse>("POST", "/api/logs/search", {
      ...bounds, ...parsed.query,
      filters: { ...parsed.query.filters, id: [selected] }, limit: 1000,
    }, { signal: controller.signal }).then((response) => {
      if (controller.signal.aborted) return;
      const candidates = location.view === "trace" ? [...response.events].reverse() : response.events;
      const found = location.view === "trace" && response.next_cursor ? undefined : candidates[location.occurrence];
      setSelectedEvent(found ?? null);
      setSelectionNote(location.view === "trace" && response.next_cursor ? "Найдено больше 1000 совпадений. Точный порядок Trace восстановить нельзя; автоматическое выделение не выполнено." :
        !found ? "Событие не найдено среди первых 1000 совпадений. Возможно, оно удалено по сроку хранения." :
        response.events.length > 1 ? "Найдено несколько событий с этими признаками. Выбор по порядку может измениться при повторном поступлении; для точной идентичности нужен уникальный ID события." :
        "Событие из ссылки показано отдельно от текущей страницы.");
    }).catch((error) => {
      if (controller.signal.aborted) return;
      if (error instanceof ApiError && error.status === 401) unauthorized.current(); else setSelectionNote(errorText(error));
    });
    return () => controller.abort();
  }, [location.selected, location.selectedTimestamp, location.occurrence, selectedIndex, queryKey, page.events, loading, liveEnabled, parsed.error]);
  useEffect(() => {
    pausedRef.current = false; setPaused(false); pending.current = []; setPendingCount(0); setLiveRows([]); setLiveNotice("");
    if (location.view !== "events" || parsed.error || parsed.query.filters.pattern_id) { setLiveEnabled(false); return; }
    if (!liveEnabled) return;
    const source = new EventSource("/api/logs/stream");
    const seen = new Set<string>();
    let checkingSession = false;
    let disposed = false;
    let matchedCount = 0;
    setStatus("connecting");
    source.addEventListener("logs", (message) => {
      let batch: LogEvent[];
      try { batch = JSON.parse(message.data) as LogEvent[]; }
      catch { setLiveNotice("Не удалось прочитать Live. Обновите историю для проверки событий."); return; }
      setStatus((current) => current === "core_error" ? current : "live");
      const oldest = Date.now() - RANGES[location.range];
      const fresh = batch.filter((event) => {
        if (seen.has(event.id)) return false;
        seen.add(event.id);
        return Date.parse(event.timestamp) >= oldest && matchesQuery(event, parsed.query);
      });
      matchedCount += fresh.length;
      while (seen.size > MAX_LIVE_ROWS * 2) seen.delete(seen.values().next().value!);
      if (pausedRef.current) {
        const merged = prependLive(pending.current, fresh); pending.current = merged.rows; setPendingCount(merged.rows.length);
        if (merged.trimmed) setLiveNotice("Буфер паузы достиг 2000 строк. Для полной истории запустите поиск заново.");
      } else {
        setLiveRows((current) => prependLive(current, fresh).rows);
        if (matchedCount > MAX_LIVE_ROWS) setLiveNotice("Live хранит не больше 2000 строк. Для полной истории используйте поиск.");
      }
    });
    source.addEventListener("lagged", () => setLiveNotice("Live пропустил пачки событий. Обновите историю, чтобы проверить диапазон."));
    source.addEventListener("core_error", () => setStatus("core_error"));
    source.addEventListener("core_ok", () => setStatus("live"));
    source.onerror = () => {
      setStatus(source.readyState === EventSource.CLOSED ? "disconnected" : "connecting");
      if (checkingSession) return;
      checkingSession = true;
      api("GET", "/api/me").catch((error) => {
        if (!disposed && error instanceof ApiError && error.status === 401) { source.close(); unauthorized.current(); }
      }).finally(() => { checkingSession = false; });
    };
    return () => { disposed = true; source.close(); };
  }, [liveEnabled, queryKey]);
  const togglePause = () => {
    if (pausedRef.current) {
      const buffered = [...pending.current].reverse();
      setLiveRows((current) => prependLive(current, buffered).rows); pending.current = []; setPendingCount(0);
    }
    pausedRef.current = !pausedRef.current; setPaused(pausedRef.current);
  };
  useEffect(() => {
    if (!liveEnabled) return;
    const keydown = (event: KeyboardEvent) => {
      const target = event.target;
      if (event.code !== "Space" || event.repeat || event.ctrlKey || event.metaKey || event.altKey ||
          (target instanceof HTMLElement && target.closest("input,textarea,select,button,a,[role=button],[contenteditable=true]"))) return;
      event.preventDefault(); togglePause();
    };
    window.addEventListener("keydown", keydown);
    return () => window.removeEventListener("keydown", keydown);
  }, [liveEnabled]);
  const loadMore = () => {
    if (!page.next_cursor || paging || loading || expired) return;
    const current = generation.current;
    const controller = new AbortController(); pagingAbort.current = controller; setPaging(true); setSearchError("");
    requestPage(controller.signal, page.next_cursor).then((response) => {
      if (current === generation.current) setPage((previous) => ({ ...response, events: appendHistory(previous.events, response.events) }));
    }).catch((error) => {
      if (!controller.signal.aborted && current === generation.current) handleError(error);
    }).finally(() => { if (current === generation.current) setPaging(false); });
  };
  const restart = () => { setLiveEnabled(false); setRevision((current) => current + 1); };
  const setRange = (range: Range) => {
    const to = Date.now(); updateLocation({ range, from: new Date(to - RANGES[range]).toISOString(), to: new Date(to).toISOString(), selected: null });
  };
  const refreshHistory = () => { setRange(location.range); restart(); };
  const openContext = (event: LogEvent) => {
    setLiveEnabled(false);
    updateLocation({ view: "events", q: sourceQuery(event), trace_id: null, selected: null, ...contextBounds(event.timestamp) });
  };
  const openTrace = (event: LogEvent) => {
    if (!event.trace_id) return;
    setLiveEnabled(false);
    updateLocation({ view: "trace", q: null, trace_id: event.trace_id, selected: null, ...(liveEnabled ? contextBounds(event.timestamp) : {}) });
  };
  const copyLink = async (event: LogEvent) => {
    const query = new URLSearchParams({ view: location.view, range: location.range, q: location.q,
      from: location.from, to: location.to, selected: event.id });
    query.set("selected_timestamp", event.timestamp);
    query.set("selected_occurrence", String(location.occurrence));
    if (location.view === "trace") query.set("trace_id", location.traceId);
    if (liveEnabled) for (const [key, value] of Object.entries(contextBounds(event.timestamp))) query.set(key, value);
    try { await navigator.clipboard.writeText(`${window.location.origin}/logs?${query}`); setCopyNote("Ссылка скопирована."); }
    catch { setCopyNote("Не удалось скопировать ссылку. Проверьте доступ к буферу обмена."); }
  };
  const exportRows = (format: "json" | "csv") => {
    const payload = format === "csv" ? eventsToCsv(rows) : JSON.stringify({
      scope: "loaded_rows", exported_at: new Date().toISOString(), view: liveEnabled ? "live" : location.view,
      query: location.q, trace_id: location.view === "trace" ? location.traceId : null,
      from: liveEnabled ? null : location.from, to: liveEnabled ? null : location.to,
      as_of: liveEnabled ? null : page.as_of, events: rows,
    }, null, 2);
    const url = URL.createObjectURL(new Blob([payload], { type: format === "csv" ? "text/csv;charset=utf-8" : "application/json" }));
    const link = document.createElement("a"); link.href = url;
    link.download = `lognara-loaded-${new Date().toISOString().replaceAll(":", "-")}.${format}`;
    document.body.append(link); link.click(); link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000); setExportsOpen(false);
  };
  const details = (event: LogEvent) => <EventDetails event={event} onContext={openContext} onTrace={openTrace} onCopy={copyLink} />;
  const total = location.view === "events" ? summary?.total : histogram?.buckets.reduce((sum, bucket) => sum + bucket.count, 0);
  const chart = location.view === "events" ? summary : histogram;

  return <section className="logs-page">
    <div className="filter-bar">
      <div className="segmented" role="group" aria-label="Вид логов">
        <button type="button" aria-pressed={location.view === "events"} onClick={() => updateLocation({ view: "events", selected: null })}>События</button>
        <button type="button" aria-pressed={location.view === "patterns"} onClick={() => {
          setLiveEnabled(false); updateLocation({ view: "patterns", trace_id: null, selected: null });
        }}>Паттерны</button>
        <button type="button" aria-pressed={location.view === "trace"} onClick={() => {
          setLiveEnabled(false); updateLocation({ view: "trace", q: null, selected: null });
        }}>Trace</button>
      </div>
      <form onSubmit={(event) => {
        event.preventDefault(); updateLocation({ [location.view === "trace" ? "trace_id" : "q"]: draft.trim(), selected: null });
        setRevision((current) => current + 1);
      }}>
        <label className="sr-only" htmlFor="log-query">{location.view === "trace" ? "Trace ID" : "Поисковый запрос"}</label>
        <input id="log-query" value={draft} onChange={(event) => setDraft(event.target.value)} spellCheck={false}
          placeholder={location.view === "trace" ? "32 символа trace_id" : "service:billing AND level:(ERROR OR WARN)"} />
        <button type="submit" className="button solid">Найти</button>
      </form>
      <label className="sr-only" htmlFor="log-range">Период</label>
      <select id="log-range" value={fixedRange ? "fixed" : location.range} onChange={(event) => setRange(event.target.value as Range)}>
        {fixedRange && <option value="fixed" disabled>Точный диапазон</option>}
        <option value="1h">1 час</option><option value="24h">24 часа</option><option value="7d">7 дней</option>
      </select>
      <button type="button" className="button" aria-pressed={liveEnabled} disabled={location.view !== "events" || !!parsed.error || !!parsed.query.filters.pattern_id}
        title={location.view === "trace" ? "Trace показывает историю связанных логов" :
          location.view === "patterns" ? "Паттерны строятся по обработанной истории" :
          parsed.query.filters.pattern_id ? "pattern_id вычисляет core: фильтр доступен только в истории" : "Переключить отдельную ленту Live"}
        onClick={() => setLiveEnabled((current) => !current)}>Live {liveEnabled ? "вкл" : "выкл"}</button>
      <button type="button" className="button" aria-expanded={exportsOpen} disabled={!rows.length || location.view === "patterns"}
        onClick={() => setExportsOpen((current) => !current)}>Экспорт</button>
    </div>
    {exportsOpen && <div className="export-menu panel"><span>Только загруженные строки: {rows.length}.</span>
      <button type="button" className="button" onClick={() => exportRows("json")}>Скачать JSON</button>
      <button type="button" className="button" onClick={() => exportRows("csv")}>Скачать CSV</button>
      <span className="muted">CSV экранирует формулы; JSON сохраняет исходные значения.</span>
    </div>}
    <details className="search-help"><summary>Синтаксис поиска</summary><p>
      Точные поля: service, service_instance, environment, server, backend, action, level, id, trace_id, span_id, parent_span_id, request_id, pattern_id.
      AND соединяет поля, OR работает внутри одного поля: <code>level:(ERROR OR WARN)</code>.
      Слова ищутся все; <code>message:"connection refused"</code> или фраза в двойных кавычках ищутся подряд.
      NOT и OR между полями пока недоступны.</p></details>
    {location.view === "patterns" ? <Patterns from={location.from} to={location.to} range={location.range} q={location.q}
      query={parsed.query} error={parsed.error} intervalSeconds={intervalSeconds} revision={revision} onUnauthorized={onUnauthorized} /> : <>
    <div className="summary"><h1>{location.view === "trace" ? "Связанные логи Trace" : "События"}</h1>
      <div className="chips"><span className="chip">Загружено {rows.length}</span>
        {!liveEnabled && total !== undefined && <span className="chip">В диапазоне {total.toLocaleString("ru-RU")}</span>}
        {LOG_LEVELS.map((level) => {
          const count = rows.filter((event) => event.level === level).length;
          return count ? <span key={level} className={`chip level-${level}`}>{level.toUpperCase()} {count}</span> : null;
        })}
      </div><p>Уровни посчитаны по загруженным строкам.</p>
    </div>
    {liveEnabled ? <div className="live-toolbar actions">
      <span className={`status ${status}`} role="status">{STATUS_TEXT[status]}</span>
      <button type="button" className="button" aria-pressed={paused} onClick={togglePause}>{paused ? "Продолжить" : "Пауза"}</button>
      {paused && <span className="muted">На паузе · в буфере {pendingCount} строк</span>}
      <button type="button" className="button" onClick={refreshHistory}>Обновить историю</button>
    </div> : <p className="page-note">Период: <time dateTime={location.from} title={location.from}>{periodLabel(location.from)}</time>
      {" → "}<time dateTime={location.to} title={location.to}>{periodLabel(location.to)}</time>
      {" · "}{Intl.DateTimeFormat().resolvedOptions().timeZone}
      {page.as_of && <> · Обновлено <time dateTime={page.as_of} title={page.as_of}>{clock.format(new Date(page.as_of))}</time></>}</p>}
    {liveEnabled && <p className="page-note">Live показывает новые поступления из общего потока, до 2000 строк. Возможны пропуски при высокой нагрузке;
      для полного диапазона используйте историю. Пауза по Space вне элементов управления.</p>}
    {liveEnabled && liveNotice && <p className="page-note" role="status">{liveNotice}</p>}
    {(parsed.error || searchError) && <div className="error" role="alert">{parsed.error || searchError}
      {expired && <button type="button" className="button" onClick={restart}>Начать поиск заново</button>}
    </div>}
    {!liveEnabled && !parsed.error && summaryUnavailable && <p className="page-note" role="status">
      Сводка недоступна для такого длинного диапазона, сузьте период. Показаны только загруженные строки.
    </p>}
    {!liveEnabled && !parsed.error && !summaryUnavailable && <section className="panel" aria-label="События по времени">
        <div className="panel-head"><h2>События по времени</h2><span className="muted">Шаг {Math.round(intervalSeconds / 60 * 10) / 10} мин</span></div>
        <Histogram buckets={chart?.buckets ?? []} label="События по времени" loading={location.view === "events" ? loading : histogramLoading}
          intervalSeconds={intervalSeconds} caption={chart ? location.view === "events"
            ? `Общий снимок сводки и первой страницы ${chart.as_of}. Подписи времени локальные.`
            : `Независимый снимок аналитики ${chart.as_of}. Подписи времени локальные.` : undefined}
          onBucket={(timestamp) => {
            const from = Math.max(Date.parse(location.from), Date.parse(timestamp));
            const to = Math.min(Date.parse(location.to), Date.parse(timestamp) + intervalSeconds * 1000);
            updateLocation({ from: new Date(from).toISOString(), to: new Date(to).toISOString(), selected: null });
          }} />
        {histogramError && <p className="error" role="status">Гистограмма: {histogramError}</p>}
      </section>}
    {location.view === "trace" && <p className="page-note">Логи упорядочены от ранних к поздним. Это корреляция по trace_id;
      spans и их длительности появятся отдельным этапом.</p>}
    {copyNote && <p className="page-note" role="status">{copyNote}</p>}
    {location.selected && rows.filter((event) => event.id === location.selected && (!location.selectedTimestamp || event.timestamp === location.selectedTimestamp)).length > 1 &&
      <p className="page-note" role="status">Несколько строк имеют одинаковые признаки записи. Выбор в ссылке зависит от их порядка; для точной идентичности нужен уникальный ID события.</p>}
    {selectionNote && <p className="page-note" role="status">{selectionNote}</p>}
    {selectedEvent && <section className="panel event-details" aria-label="Событие из ссылки">
      <h2>{selectedEvent.level.toUpperCase()} · {selectedEvent.timestamp}</h2><p>{selectedEvent.message}</p>{details(selectedEvent)}
    </section>}
    <section className="panel table-scroll" aria-label="События">
      <table className="logs-table"><thead><tr>
        <th scope="col">Время</th><th scope="col">Уровень</th><th scope="col">Источник</th><th scope="col">Сообщение</th>
      </tr></thead><tbody>{rows.map((event, index) => <Fragment key={`${event.id}:${index}`}>
        <tr className={`event-row level-${event.level} ${selectedIndex === index ? "open" : ""}`}>
          <td className="time" title={event.timestamp}>
            {Date.parse(location.to) - Date.parse(location.from) >= 86_400_000 && <span className="date">{calendar.format(new Date(event.timestamp))}</span>}
            <time dateTime={event.timestamp}>{eventTime(event.timestamp)}</time>
          </td>
          <td><span className={`level level-${event.level}`}>{event.level.toUpperCase()}</span></td>
          <td className="source">{event.service}<br /><span className="muted">{event.service_instance ?? event.server}</span></td>
          <td className="message"><button type="button" className="row-button" aria-expanded={selectedIndex === index}
            onClick={() => {
              const occurrence = rows.slice(0, index).filter((row) => row.id === event.id && row.timestamp === event.timestamp).length;
              updateLocation({ selected: selectedIndex === index ? null : event.id,
                selected_timestamp: selectedIndex === index ? null : event.timestamp,
                selected_occurrence: selectedIndex === index ? null : String(occurrence) }, true);
            }}>
            <span className="caret" aria-hidden="true">{selectedIndex === index ? "▾" : "▸"}</span><code>{event.message}</code>
          </button></td>
        </tr>
        {selectedIndex === index && <tr className="event-details"><td colSpan={4}>{details(event)}</td></tr>}
      </Fragment>)}</tbody></table>
      {!rows.length && <p className="empty-state" role="status">{!liveEnabled && loading ? "Загружаем события…" :
        liveEnabled ? "Ожидаем события Live, соответствующие запросу." : parsed.error || searchError ? "Поиск не выполнен." : "В выбранном диапазоне событий нет."}</p>}
    </section>
    <div className="table-footer"><span>Показано {rows.length} загруженных строк{!liveEnabled && page.next_cursor ? " · есть следующая страница" : ""}.</span>
      {!liveEnabled && <div className="actions">
        {page.next_cursor && <button type="button" className="button" disabled={paging || expired} onClick={loadMore}>{paging ? "Загрузка…" : "Загрузить ещё 50"}</button>}
        <button type="button" className="button" disabled={loading || paging || !!parsed.error} onClick={restart}>Обновить поиск</button>
      </div>}
    </div></>}
  </section>;
}
