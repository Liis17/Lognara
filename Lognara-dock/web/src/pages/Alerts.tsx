import { useEffect, useMemo, useState, type FormEvent, type ReactNode } from "react";
import { Link, useSearchParams } from "react-router";
import {
  api,
  ApiError,
  errorText,
  nodeScopeLabel,
  type AlertMessagePreviewReply,
  type LogAlert,
  type LogAlertInput,
  type LogAlertList,
  type MetricAlertDefinition,
  type MetricAlertSettings,
  type SeriesMetric,
} from "../api";
import type { SummaryResponse } from "../types";
import { type LogAlertForm, logAlertInput } from "../logs";
import MetricAlertEditor, { definitionDraft } from "./MetricAlertEditor";
import MetricAlertSettingsPage from "./MetricAlertSettings";

type Tab = "rules" | "recipients" | "telegram";

const iconPaths: Record<"rename" | "edit" | "power" | "delete", ReactNode> = {
  rename: <><path d="M4 20h4L19 9l-4-4L4 16v4Z" /><path d="M13 7l4 4" /></>,
  edit: <><path d="M4 7h10M18 7h2M4 17h4M12 17h8" /><circle cx="16" cy="7" r="2" /><circle cx="10" cy="17" r="2" /></>,
  power: <><path d="M12 3v8" /><path d="M6.4 6.6a8 8 0 1 0 11.2 0" /></>,
  delete: <><path d="M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13" /></>,
};

function IconButton({ icon, label, disabled, onClick }: { icon: keyof typeof iconPaths; label: string; disabled: boolean; onClick: () => void }) {
  return <button type="button" className="icon-button" aria-label={label} title={label} disabled={disabled} onClick={onClick}>
    <svg viewBox="0 0 24 24" aria-hidden="true">{iconPaths[icon]}</svg>
  </button>;
}
type RuleType = "metric" | "logs";
type RuleStatus = "ok" | "firing" | "disabled" | "no-recipients" | "no-data";

const logFields = ["name", "count", "query", "samples", "time", "status"];
const messageFieldOptions = [
  ["name", "Имя"], ["title", "Название"], ["description", "Описание"], ["server", "Сервер"],
  ["service", "Сервис"], ["environment", "Окружение"], ["condition", "Условие"], ["value", "Значение"],
  ["count", "Число совпадений"], ["query", "Запрос"], ["samples", "Примеры строк"],
  ["time", "Время"], ["last_point", "Последняя точка"], ["status", "Статус"],
] as const;

const statusText: Record<LogAlertList["core_status"], string> = {
  unknown: "", available: "", busy: "Core занят: проверка повторится через минуту.",
  unavailable: "Core недоступен: алерты на логи пока не проверяются.",
  incompatible: "Core не поддерживает запрос алертов на логи; обновите Core.",
};

function metricKey(metric: SeriesMetric) {
  return `${metric.server}\u0000${metric.service}\u0000${metric.name}\u0000${metric.origin}`;
}

function sameMetric(alert: MetricAlertDefinition, metric: SeriesMetric) {
  return alert.key.server === metric.server && alert.key.service === metric.service
    && alert.key.name === metric.name && alert.key.origin === metric.origin;
}

function fallbackMetric(key: { server: string; service: string; name: string; origin: SeriesMetric["origin"] }): SeriesMetric {
  return { ...key, definition: null, environments: [], first_seen: null, last_seen: null };
}

function metricTitle(metric: SeriesMetric) {
  return metric.definition?.title || metric.name;
}

function MetricCombobox({
  value,
  metrics,
  onChange,
}: {
  value: SeriesMetric | null;
  metrics: SeriesMetric[];
  onChange: (metric: SeriesMetric) => void;
}) {
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState(false);
  const [focused, setFocused] = useState(false);
  const visible = useMemo(() => {
    const needle = search.trim().toLocaleLowerCase();
    return metrics.filter((metric) => {
      const definition = metric.definition;
      const text = [metricTitle(metric), metric.name, metric.server, metric.service, definition?.description ?? ""]
        .join(" ").toLocaleLowerCase();
      return !needle || text.includes(needle);
    }).slice(0, 40);
  }, [metrics, search]);
  return <div className="alert-metric-combobox">
    <label htmlFor="alert-metric-search">Метрика</label>
    <input
      id="alert-metric-search"
      role="combobox"
      aria-autocomplete="list"
      aria-expanded={open}
      aria-controls="alert-metric-options"
      value={focused ? search : value ? `${metricTitle(value)} · ${value.server} · ${value.service}` : ""}
      placeholder="Найдите метрику по имени, серверу или описанию"
      onFocus={() => { setFocused(true); setSearch(""); setOpen(true); }}
      onBlur={() => { setFocused(false); setOpen(false); }}
      onChange={(event) => { setSearch(event.target.value); setOpen(true); }}
      onKeyDown={(event) => {
        if (event.key === "Escape") setOpen(false);
        if (event.key === "Enter" && visible[0]) {
          event.preventDefault();
          onChange(visible[0]);
          setOpen(false);
          setFocused(false);
        }
      }}
    />
    {open && <div className="alert-metric-options" id="alert-metric-options" role="listbox" aria-label="Результаты поиска метрик">
      {visible.map((metric) => <button
        type="button"
        role="option"
        aria-selected={value ? metricKey(value) === metricKey(metric) : false}
        key={metricKey(metric)}
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => { onChange(metric); setOpen(false); setFocused(false); }}
      >
        <strong>{metricTitle(metric)}</strong>
        <code>{metric.name}</code>
        <span>{metric.server} · {metric.service}</span>
        {metric.definition?.description && <small>{metric.definition.description}</small>}
      </button>)}
      {!visible.length && <p className="muted">Метрики не найдены</p>}
    </div>}
  </div>;
}

function formatDuration(seconds: number) {
  if (seconds % 86400 === 0) return `${seconds / 86400} дн`;
  if (seconds % 3600 === 0) return `${seconds / 3600} ч`;
  if (seconds % 60 === 0) return `${seconds / 60} мин`;
  return `${seconds} сек`;
}

function metricCondition(alert: MetricAlertDefinition) {
  const joiner = alert.combine === "and" ? "все" : "любое";
  const aggregates: Record<string, string> = {
    avg: "среднее", min: "минимум", max: "максимум", sum: "сумма", count: "число точек", increase: "прирост",
  };
  return `${joiner}: ${alert.triggers.map((trigger) => `${aggregates[trigger.aggregate] ?? trigger.aggregate} за ${formatDuration(trigger.window_seconds)} ${trigger.comparator} ${trigger.threshold}`).join(" · ")}`;
}

function getMetricStatus(alert: MetricAlertDefinition): RuleStatus {
  if (!alert.enabled) return "disabled";
  if (alert.no_recipients) return "no-recipients";
  if (alert.loss_registered || alert.state === "unknown") return "no-data";
  return alert.state === "firing" ? "firing" : "ok";
}

function getLogStatus(alert: LogAlert): RuleStatus {
  if (!alert.enabled) return "disabled";
  if (alert.no_recipients) return "no-recipients";
  if (!alert.last_checked_at) return "no-data";
  return alert.last_count >= alert.threshold ? "firing" : "ok";
}

const statusLabels: Record<RuleStatus, string> = {
  ok: "В норме", firing: "Сработал", disabled: "Отключён", "no-recipients": "Нет получателей", "no-data": "Нет данных",
};

function initialLogForm(query = ""): LogAlertForm {
  return { name: "", enabled: true, query, threshold: 1, cooldownMinutes: 5, groupIds: [] };
}

type LogNodeFilters = { relay_id: string; agent_id: string };

function nodeFiltersFromParams(params: URLSearchParams): LogNodeFilters {
  return { relay_id: params.getAll("relay_id").join(", "), agent_id: params.getAll("agent_id").join(", ") };
}

function nodeFilterValues(value: string): string[] {
  return [...new Set(value.split(/[\s,]+/).map((id) => id.trim()).filter(Boolean))];
}

function logAlertInputWithNodes(form: LogAlertForm, nodeFilters: LogNodeFilters): LogAlertInput {
  const input = logAlertInput(form);
  for (const field of ["relay_id", "agent_id"] as const) {
    const values = nodeFilterValues(nodeFilters[field]);
    if (values.length) input.filters[field] = [...new Set([...(input.filters[field] ?? []), ...values])];
  }
  return input;
}

function logSearchParams(query: string, nodeFilters: LogNodeFilters): URLSearchParams {
  const params = new URLSearchParams({ q: query });
  for (const field of ["relay_id", "agent_id"] as const) {
    for (const value of nodeFilterValues(nodeFilters[field])) params.append(field, value);
  }
  return params;
}

export default function Alerts({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [params, setParams] = useSearchParams();
  const requestedKey = params.get("server") && params.get("service") && params.get("name")
    ? {
      server: params.get("server")!, service: params.get("service")!, name: params.get("name")!,
      origin: (params.get("origin") ?? "counter") as SeriesMetric["origin"],
    }
    : null;
  const [settings, setSettings] = useState<MetricAlertSettings | null>(null);
  const [logList, setLogList] = useState<LogAlertList | null>(null);
  const [metrics, setMetrics] = useState<SeriesMetric[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [renaming, setRenaming] = useState<{ id: string; value: string } | null>(null);
  const [search, setSearch] = useState("");
  const [typeFilter, setTypeFilter] = useState(params.get("type") ?? "all");
  const [statusFilter, setStatusFilter] = useState("all");
  const [newMenuOpen, setNewMenuOpen] = useState(false);
  const [editorType, setEditorType] = useState<RuleType | null>(() => {
    if (params.get("type") === "metric" && requestedKey) return "metric";
    return params.get("type") === "logs" && params.has("q") ? "logs" : null;
  });
  const [selectedMetric, setSelectedMetric] = useState<SeriesMetric | null>(() => requestedKey ? fallbackMetric(requestedKey) : null);
  const [editingLog, setEditingLog] = useState<LogAlert | null>(null);
  const [logForm, setLogForm] = useState<LogAlertForm>(() => initialLogForm(params.get("q") ?? ""));
  const [logNodeFilters, setLogNodeFilters] = useState<LogNodeFilters>(() => nodeFiltersFromParams(params));
  const [logTemplate, setLogTemplate] = useState("");
  const [logMessageFields, setLogMessageFields] = useState<string[]>([...logFields]);
  const [logPreview, setLogPreview] = useState("");
  const [messagePreview, setMessagePreview] = useState<AlertMessagePreviewReply | null>(null);
  const tab = (params.get("tab") ?? "rules") as Tab;

  const fail = (cause: unknown) => {
    if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
    else setError(errorText(cause));
  };
  const load = async () => {
    setError("");
    try {
      const [alertSettings, logs] = await Promise.all([
        api<MetricAlertSettings>("GET", "/api/alerts"),
        api<LogAlertList>("GET", "/api/log-alerts"),
      ]);
      setSettings(alertSettings);
      setLogList(logs);
    } catch (cause) {
      fail(cause);
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => { void load(); }, []);
  useEffect(() => {
    const query = new URLSearchParams({ group_by: "server" });
    const controller = new AbortController();
    api<{ metrics: SeriesMetric[] }>("GET", `/api/series?${query}`, undefined, { signal: controller.signal })
      .then((reply) => setMetrics(reply.metrics))
      .catch((cause) => { if (!controller.signal.aborted) fail(cause); });
    return () => controller.abort();
  }, []);
  useEffect(() => {
    if (!requestedKey || !metrics.length) return;
    const match = metrics.find((metric) => metric.server === requestedKey.server && metric.service === requestedKey.service
      && metric.name === requestedKey.name && metric.origin === requestedKey.origin);
    if (match) setSelectedMetric(match);
  }, [requestedKey?.server, requestedKey?.service, requestedKey?.name, requestedKey?.origin, metrics]);

  const changeTab = (next: Tab) => {
    setParams((current) => {
      const result = new URLSearchParams(current);
      result.set("tab", next);
      return result;
    });
  };
  const closeEditor = () => {
    setEditorType(null);
    setSelectedMetric(null);
    setEditingLog(null);
    setMessagePreview(null);
    setLogNodeFilters({ relay_id: "", agent_id: "" });
    setParams((current) => {
      const result = new URLSearchParams(current);
      result.set("tab", "rules");
      for (const key of ["type", "server", "service", "name", "origin", "q", "relay_id", "agent_id"]) result.delete(key);
      return result;
    }, { replace: true });
  };
  const openMetricEditor = (metric: SeriesMetric | null) => {
    setEditorType("metric");
    setSelectedMetric(metric);
    setEditingLog(null);
    setNewMenuOpen(false);
    setParams((current) => {
      const result = new URLSearchParams(current);
      result.set("tab", "rules");
      result.set("type", "metric");
      if (metric) {
        result.set("server", metric.server);
        result.set("service", metric.service);
        result.set("name", metric.name);
        result.set("origin", metric.origin);
      } else {
        for (const key of ["server", "service", "name", "origin"]) result.delete(key);
      }
      return result;
    }, { replace: true });
  };
  const openNewLog = () => {
    setEditorType("logs");
    setEditingLog(null);
    setLogForm(initialLogForm(params.get("q") ?? ""));
    setLogNodeFilters(nodeFiltersFromParams(params));
    setLogTemplate("");
    setLogMessageFields([...logFields]);
    setLogPreview("");
    setNewMenuOpen(false);
  };
  const editLog = (alert: LogAlert) => {
    setEditorType("logs");
    setEditingLog(alert);
    setLogForm({
      name: alert.name,
      enabled: alert.enabled,
      query: alert.query,
      threshold: alert.threshold,
      cooldownMinutes: Math.ceil(alert.cooldown_seconds / 60),
      groupIds: alert.group_ids,
    });
    setLogNodeFilters({
      relay_id: (alert.filters.relay_id ?? []).join(", "),
      agent_id: (alert.filters.agent_id ?? []).join(", "),
    });
    setLogTemplate(alert.message_template ?? "");
    setLogMessageFields(alert.message_fields ?? [...logFields]);
    setLogPreview("");
  };
  const saveLog = async (event: FormEvent) => {
    event.preventDefault();
    setError("");
    setBusy(true);
    try {
      const input: LogAlertInput = {
        ...logAlertInputWithNodes(logForm, logNodeFilters),
        message_template: logTemplate,
        message_fields: logMessageFields,
      };
      if (editingLog) await api("PUT", `/api/log-alerts/${editingLog.id}`, input);
      else await api("POST", "/api/log-alerts", input);
      closeEditor();
      await load();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  };
  const checkLog = async () => {
    setError("");
    setBusy(true);
    try {
      const input = logAlertInputWithNodes(logForm, logNodeFilters);
      const to = new Date();
      const summary = await api<SummaryResponse>("POST", "/api/stats/summary", {
        from: new Date(to.getTime() - 3_600_000).toISOString(),
        to: to.toISOString(),
        filters: input.filters,
        ...(input.text ? { text: input.text } : {}),
        interval_seconds: 300,
        search_limit: 3,
      });
      setLogPreview([`Совпадений за последний час: ${summary.total}`,
        ...(summary.page?.events ?? []).map((event) => `${event.timestamp} ${event.message}`)].join("\n"));
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  };
  const previewLogMessage = async () => {
    setError("");
    try {
      const reply = await api<AlertMessagePreviewReply>("POST", "/api/alerts/preview", {
        kind: "logs",
        name: logForm.name || "Новый алерт",
        title: logForm.name || "Новый алерт",
        query: logForm.query,
        condition: `не меньше ${logForm.threshold} совпадений за проверку`,
        value: String(logForm.threshold),
        count: logForm.threshold,
        time: new Date().toISOString(),
        status: "firing",
        message_template: logTemplate,
        message_fields: logMessageFields,
      });
      setMessagePreview(reply);
    } catch (cause) {
      fail(cause);
    }
  };

  const rows = useMemo(() => {
    const metricRows = (settings?.alerts ?? []).map((alert) => {
      const status = getMetricStatus(alert);
      const isMcp = alert.node_scope.kind !== "all";
      return {
        id: `metric:${alert.id}`, type: "metric" as const, name: alert.title || alert.key.name,
        scope: `${alert.key.server} · ${alert.key.service} · ${alert.key.name}`,
        condition: metricCondition(alert), enabled: alert.enabled, status, source: isMcp
          ? `Создано через MCP · ${nodeScopeLabel(alert.node_scope)} · изменяется через MCP` : "Создано в Dock",
        groups: alert.group_ids.map((id) => settings?.groups.find((group) => group.id === id)?.name).filter(Boolean).join(", ") || "нет",
        metric: alert, log: null as LogAlert | null,
      };
    });
    const logRows = (logList?.alerts ?? []).map((alert) => ({
      id: `logs:${alert.id}`, type: "logs" as const, name: alert.name, scope: [
        "Логи",
        ...(alert.filters.relay_id ?? []).map((id) => `relay_id=${id}`),
        ...(alert.filters.agent_id ?? []).map((id) => `agent_id=${id}`),
      ].join(" · "),
      condition: `≥ ${alert.threshold} совпадений · пауза ${formatDuration(alert.cooldown_seconds)}`,
      enabled: alert.enabled, status: getLogStatus(alert), source: "Создано в Dock",
      groups: alert.group_ids.map((id) => settings?.groups.find((group) => group.id === id)?.name).filter(Boolean).join(", ") || "нет",
      metric: null as MetricAlertDefinition | null, log: alert,
    }));
    return [...metricRows, ...logRows];
  }, [settings, logList]);
  const visibleRows = rows.filter((row) => {
    const matchesType = typeFilter === "all" || row.type === typeFilter;
    const matchesStatus = statusFilter === "all" || row.status === statusFilter;
    const needle = search.trim().toLocaleLowerCase();
    const matchesSearch = !needle || `${row.name} ${row.scope} ${row.condition} ${row.groups}`.toLocaleLowerCase().includes(needle);
    return matchesType && matchesStatus && matchesSearch;
  });

  const removeRule = async (alert: MetricAlertDefinition | LogAlert, type: RuleType) => {
    if (!window.confirm(`Удалить алерт «${type === "metric" ? (alert as MetricAlertDefinition).key.name : (alert as LogAlert).name}»?`)) return;
    setBusy(true);
    setError("");
    try {
      if (type === "metric") await api("DELETE", `/api/alerts/rules/${encodeURIComponent((alert as MetricAlertDefinition).id)}`);
      else await api("DELETE", `/api/log-alerts/${(alert as LogAlert).id}`);
      await load();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  };
  const saveRename = async (row: { metric: MetricAlertDefinition | null; log: LogAlert | null }) => {
    const name = renaming?.value.trim();
    if (!name) return;
    setBusy(true);
    setError("");
    try {
      if (row.metric) await api("PUT", "/api/alerts/definition", { ...definitionDraft(row.metric), title: name });
      else if (row.log) {
        const { id: _id, revision: _revision, no_recipients: _noRecipients, last_count: _lastCount,
          last_match_at: _lastMatch, last_checked_at: _lastChecked, last_sent_at: _lastSent, ...input } = row.log;
        await api("PUT", `/api/log-alerts/${row.log.id}`, { ...input, name });
      }
      setRenaming(null);
      await load();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  };
  const toggleLog = async (alert: LogAlert) => {
    setBusy(true);
    setError("");
    try {
      const { id: _id, revision: _revision, no_recipients: _noRecipients, last_count: _lastCount,
        last_match_at: _lastMatch, last_checked_at: _lastChecked, last_sent_at: _lastSent, ...input } = alert;
      await api("PUT", `/api/log-alerts/${alert.id}`, { ...input, enabled: !alert.enabled });
      await load();
    } catch (cause) {
      fail(cause);
    } finally {
      setBusy(false);
    }
  };
  const toggleField = (field: string, checked: boolean) => setLogMessageFields((current) => checked
    ? [...current, field]
    : current.filter((item) => item !== field));

  return <section className="alerts-page">
    <header className="alerts-heading">
      <div><h1>Алерты</h1><p className="muted">Правила для метрик и логов, получатели и доставка в Telegram.</p></div>
    </header>
    <nav className="alerts-tabs" aria-label="Разделы алертов">
      {([["rules", "Правила"], ["recipients", "Получатели и группы"], ["telegram", "Telegram"]] as const).map(([value, label]) =>
        <button type="button" key={value} className={tab === value ? "active" : ""} aria-current={tab === value ? "page" : undefined}
          onClick={() => changeTab(value)}>{label}</button>)}
    </nav>

    {tab === "recipients" && <MetricAlertSettingsPage onUnauthorized={onUnauthorized} view="recipients" />}
    {tab === "telegram" && <MetricAlertSettingsPage onUnauthorized={onUnauthorized} view="telegram" logCoreStatus={logList?.core_status} />}
    {tab === "rules" && <>
      {settings && !settings.enabled && <p className="alert-global-hint" role="status">Глобальная отправка выключена. Настроить её можно во вкладке Telegram.</p>}
      {logList && statusText[logList.core_status] && <p className="error" role="status">{statusText[logList.core_status]}</p>}
      <div className="alerts-toolbar">
        <label>Поиск<input value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Имя, метрика, сервис, сервер" /></label>
        <label>Тип<select value={typeFilter} onChange={(event) => setTypeFilter(event.target.value)}>
          <option value="all">Все типы</option><option value="metric">Метрики</option><option value="logs">Логи</option>
        </select></label>
        <label>Статус<select value={statusFilter} onChange={(event) => setStatusFilter(event.target.value)}>
          <option value="all">Все статусы</option><option value="ok">В норме</option><option value="firing">Сработал</option>
          <option value="disabled">Отключён</option><option value="no-recipients">Нет получателей</option><option value="no-data">Нет данных</option>
        </select></label>
        <div className="new-alert-control">
          <button type="button" className="solid" aria-expanded={newMenuOpen} onClick={() => setNewMenuOpen((open) => !open)}>Новый алерт</button>
          {newMenuOpen && <div className="new-alert-menu" role="group" aria-label="Тип нового алерта">
            <button type="button" onClick={() => openMetricEditor(null)}>Алерт метрики</button>
            <button type="button" onClick={openNewLog}>Алерт логов</button>
          </div>}
        </div>
      </div>
      {loading ? <p className="muted">Загрузка правил…</p> : <div className="alert-rule-list">
        {visibleRows.map((row) => {
          const editable = row.log !== null || row.metric?.node_scope.kind === "all";
          const isRenaming = renaming?.id === row.id;
          return <article className="alert-rule-row" key={row.id} aria-label={`Алерт ${row.name}`}>
            <div className="alert-rule-head">
              <div className="alert-rule-title-line">
                <span className={`alert-type-tag alert-type-${row.type}`}>{row.type === "metric" ? "Метрика" : "Логи"}</span>
                {isRenaming ? <form className="alert-rename-form" onSubmit={(event) => { event.preventDefault(); void saveRename(row); }}>
                  <input aria-label="Публичное имя алерта" autoFocus maxLength={100} value={renaming.value}
                    onChange={(event) => setRenaming({ id: row.id, value: event.target.value })}
                    onKeyDown={(event) => { if (event.key === "Escape") setRenaming(null); }} />
                  <button type="submit" disabled={busy || !renaming.value.trim()}>Сохранить</button>
                  <button type="button" className="quiet" disabled={busy} onClick={() => setRenaming(null)}>Отмена</button>
                </form> : <strong>{row.name}</strong>}
                <span className={`alert-status-chip alert-status-${row.status}`}>{statusLabels[row.status]}</span>
              </div>
              {!isRenaming && <div className="alert-rule-actions">
                {editable && <IconButton icon="rename" label="Переименовать" disabled={busy} onClick={() => setRenaming({ id: row.id, value: row.name })} />}
                {row.metric && editable && <IconButton icon="edit" label="Изменить" disabled={busy} onClick={() => {
                  const match = metrics.find((metric) => sameMetric(row.metric!, metric));
                  openMetricEditor(match ?? fallbackMetric(row.metric!.key));
                }} />}
                {row.log && <>
                  <IconButton icon="edit" label="Изменить" disabled={busy} onClick={() => editLog(row.log!)} />
                  <IconButton icon="power" label={row.log.enabled ? "Отключить" : "Включить"} disabled={busy} onClick={() => void toggleLog(row.log!)} />
                </>}
                {(row.log || !editable) && <IconButton icon="delete" label="Удалить" disabled={busy}
                  onClick={() => void (row.log ? removeRule(row.log, "logs") : removeRule(row.metric!, "metric"))} />}
              </div>}
            </div>
            <p className="alert-rule-scope">{row.scope}</p>
            <p className="alert-rule-condition">{row.condition}</p>
            <p className="muted alert-rule-meta"><span>Получатели: {row.groups}</span> · <span>{row.source}</span></p>
          </article>;
        })}
        {!visibleRows.length && <p className="muted">Подходящих алертов нет.</p>}
      </div>}
    </>}
    {error && <p className="error" role="alert">{error}</p>}

    {editorType === "metric" && <dialog open className="alert-editor-dialog" aria-label="Редактор алерта метрики">
      <button type="button" className="alert-editor-close" aria-label="Закрыть редактор" onClick={closeEditor}>×</button>
      {!selectedMetric ? <div className="alert-metric-picker-panel">
        <h2>Новый алерт метрики</h2>
        <MetricCombobox metrics={metrics} value={selectedMetric} onChange={setSelectedMetric} />
        <div className="toolbar"><button type="button" onClick={closeEditor}>Отмена</button></div>
      </div> : <>
        <MetricAlertEditor key={metricKey(selectedMetric)}
          picker={<MetricCombobox metrics={metrics} value={selectedMetric} onChange={setSelectedMetric} />} keyValue={{
          server: selectedMetric.server, service: selectedMetric.service, name: selectedMetric.name, origin: selectedMetric.origin,
        }} metric={selectedMetric} onUnauthorized={onUnauthorized} onCancel={closeEditor} onDone={() => { closeEditor(); void load(); }} />
      </>}
    </dialog>}

    {editorType === "logs" && <dialog open className="alert-editor-dialog" aria-label={editingLog ? "Изменить алерт логов" : "Новый алерт логов"}>
      <button type="button" className="alert-editor-close" aria-label="Закрыть редактор" onClick={closeEditor}>×</button>
      <form className="log-alert-form" onSubmit={saveLog}>
        <h2>{editingLog ? "Редактор алерта логов" : "Новый алерт логов"}</h2>
        <label>Имя<input aria-label="Имя" maxLength={100} value={logForm.name} onChange={(event) => setLogForm((current) => ({ ...current, name: event.target.value }))} required /></label>
        <label>Запрос<textarea aria-label="Запрос" value={logForm.query} spellCheck={false} onChange={(event) => setLogForm((current) => ({ ...current, query: event.target.value }))} required /></label>
        <fieldset className="log-alert-numbers"><legend>Фильтры узлов</legend>
          <label>Relay ID<input aria-label="Relay ID" autoComplete="off" value={logNodeFilters.relay_id}
            onChange={(event) => setLogNodeFilters((current) => ({ ...current, relay_id: event.target.value }))} placeholder="например, 4" /></label>
          <label>Agent ID<input aria-label="Agent ID" autoComplete="off" value={logNodeFilters.agent_id}
            onChange={(event) => setLogNodeFilters((current) => ({ ...current, agent_id: event.target.value }))} placeholder="например, 9" /></label>
        </fieldset>
        <Link to={`/logs?${logSearchParams(logForm.query, logNodeFilters)}`} target="_blank">Открыть в логах</Link>
        <div className="log-alert-numbers">
          <label>Порог совпадений<input type="number" min={1} max={1_000_000} value={logForm.threshold} onChange={(event) => setLogForm((current) => ({ ...current, threshold: Number(event.target.value) }))} required /></label>
          <label>Пауза, минут<input type="number" min={0} max={1440} value={logForm.cooldownMinutes} onChange={(event) => setLogForm((current) => ({ ...current, cooldownMinutes: Number(event.target.value) }))} required /></label>
        </div>
        <fieldset className="alert-group-picker"><legend>Группы получателей</legend>
          {(settings?.groups ?? []).map((group) => <label key={group.id}><input type="checkbox" checked={logForm.groupIds.includes(group.id)} onChange={(event) => setLogForm((current) => ({
            ...current, groupIds: event.target.checked ? [...current.groupIds, group.id] : current.groupIds.filter((id) => id !== group.id),
          }))} />{group.name} · {group.recipient_ids.length} получателей</label>)}
          {!settings?.groups.length && <span className="muted">Сначала создайте группу получателей.</span>}
        </fieldset>
        <label className="switch-label"><input type="checkbox" checked={logForm.enabled} onChange={(event) => setLogForm((current) => ({ ...current, enabled: event.target.checked }))} />Включён</label>
        <label>Свой текст сообщения Telegram<textarea rows={3} maxLength={2000} value={logTemplate} onChange={(event) => { setMessagePreview(null); setLogTemplate(event.target.value); }} placeholder="Например: %name%: найдено %count% совпадений" /></label>
        <p className="muted">Плейсхолдеры для логов: %count%, %query%, %name%, %title%, %description%, %service%, %server%, %environment%, %condition%, %time%, %last_point%, %status%.</p>
        <fieldset className="alert-message-fields"><legend>Поля сообщения</legend>
          {messageFieldOptions.map(([field, label]) => <label key={field}><input type="checkbox" checked={logMessageFields.includes(field)} onChange={(event) => toggleField(field, event.target.checked)} />{label}</label>)}
        </fieldset>
        <div className="toolbar">
          <button type="button" disabled={busy} onClick={() => void checkLog()}>Проверить за час</button>
          <button type="button" disabled={busy} onClick={() => void previewLogMessage()}>Предпросмотр Telegram</button>
        </div>
        {logPreview && <pre className="log-alert-preview" aria-label="Предпросмотр">{logPreview}</pre>}
        {messagePreview && <div className="telegram-preview" aria-label="Предпросмотр сообщения Telegram"><div dangerouslySetInnerHTML={{ __html: messagePreview.html }} />{messagePreview.button && <span className="telegram-preview-button">Открыть в Dock</span>}</div>}
        <div className="toolbar">
          <button type="submit" className="solid" disabled={busy}>{editingLog ? "Сохранить алерт" : "Создать алерт"}</button>
          <button type="button" disabled={busy} onClick={closeEditor}>Отмена</button>
        </div>
      </form>
    </dialog>}
  </section>;
}
