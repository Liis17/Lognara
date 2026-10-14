import { type ReactNode, useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { api, ApiError, errorText } from "../api";
import { sourceQuery } from "../logs";
import type { InfraNode, InfraStorage, InfrastructureResponse, NodeStatus } from "../types";

const REFRESH_MS = 10_000;
const number = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 1 });
const clock = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", second: "2-digit" });
const day = new Intl.DateTimeFormat("ru-RU", { day: "numeric", month: "short" });
const STATUS: Record<NodeStatus, { label: string; tone: string }> = {
  offline: { label: "Отключён", tone: "err" }, degraded: { label: "Деградирован", tone: "warn" },
  unknown: { label: "Нет данных", tone: "mute" }, idle: { label: "Нет новых логов", tone: "mute" },
  online: { label: "Онлайн", tone: "ok" },
};
const REASON: Record<string, string> = {
  no_heartbeat: "heartbeat ещё не приходил", relay_silent: "relay агента не на связи",
  spool: "спул заполнен", delivery: "ошибки доставки",
};

function plural(count: number, forms: [string, string, string]): string {
  const tens = count % 100, ones = count % 10;
  return tens >= 11 && tens <= 14 ? forms[2] : ones === 1 ? forms[0] : ones >= 2 && ones <= 4 ? forms[1] : forms[2];
}

function bytes(value: number): string {
  const units = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit += 1; }
  return `${number.format(value)} ${units[unit]}`;
}

/** Возраст отметки по часам core. */
function age(value: string | null, asOf: string): string {
  if (!value) return "—";
  const seconds = Math.max(0, (Date.parse(asOf) - Date.parse(value)) / 1000);
  if (seconds < 1) return "<1 с";
  if (seconds < 60) return `${Math.floor(seconds)} с`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} мин`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)} ч`;
  return `${Math.floor(seconds / 86_400)} д`;
}

const percent = (spool: InfraNode["spool"]) => spool && spool.limit_bytes > 0 ? Math.min(100, Math.round(spool.used_bytes * 100 / spool.limit_bytes)) : null;

function Meter({ value, hot }: { value: number; hot: boolean }) {
  return <span className="meter" aria-hidden="true"><i className={hot ? "hot" : undefined} style={{ width: `${value}%` }} /></span>;
}

function Chip({ tone, children }: { tone: string; children: ReactNode }) {
  return <span className={`node-status ${tone}`}>{children}</span>;
}

/** Самый заполненный spool группы узлов. */
function SpoolLine({ nodes, warn }: { nodes: InfraNode[]; warn: number }) {
  const fullest = nodes.flatMap((node) => { const value = percent(node.spool); return value === null ? [] : [{ node, value }]; })
    .sort((a, b) => b.value - a.value)[0];
  if (!fullest) return <div className="stage-sub">спул: нет данных</div>;
  return <>
    <div className="stage-sub">спул макс. {fullest.value}% · {fullest.node.name}</div>
    <Meter value={fullest.value} hot={fullest.value >= warn} />
  </>;
}

function groupChip(nodes: InfraNode[]): { tone: string; text: string } {
  const count = (status: NodeStatus) => nodes.filter((node) => node.status === status).length;
  if (!nodes.length) return { tone: "mute", text: "Нет узлов" };
  if (count("offline")) return { tone: "err", text: `${count("offline")} отключено` };
  if (count("degraded")) return { tone: "warn", text: `${count("degraded")} деградировано` };
  if (count("unknown")) return { tone: "mute", text: `${count("unknown")} без данных` };
  return { tone: "ok", text: "Онлайн" };
}

function Storage({ storage, wal }: { storage: InfraStorage; wal: number }) {
  const used = storage.disk_total_bytes - storage.disk_free_bytes;
  const other = Math.max(0, used - storage.parquet_bytes - storage.index_bytes - wal);
  const parts = [
    { label: "Parquet-сегменты", value: storage.parquet_bytes, color: "var(--info)" },
    { label: "Индексы", value: storage.index_bytes, color: "var(--accent)" },
    { label: "WAL", value: wal, color: "var(--warn)" },
    { label: "Прочее на диске", value: other, color: "var(--ink3)" },
    { label: "Свободно", value: storage.disk_free_bytes, color: "var(--line)" },
  ];
  const share = (value: number) => storage.disk_total_bytes > 0 ? value * 100 / storage.disk_total_bytes : 0;
  return <>
    <div className="disk" role="img" aria-label={parts.map((part) => `${part.label} ${Math.round(share(part.value))}%`).join(", ")}>
      {parts.map((part) => <i key={part.label} style={{ width: `${share(part.value)}%`, background: part.color }} />)}
    </div>
    <div className="disk-legend">{parts.map((part) => <div key={part.label}>
      <i style={{ background: part.color }} /><span>{part.label}</span>
      <b>{part.label === "Свободно" ? `${Math.round(share(part.value))}% · ${bytes(part.value)}` : bytes(part.value)}</b>
    </div>)}</div>
    <p className="page-note">{storage.oldest_event_at ? `Самое раннее событие: ${day.format(new Date(storage.oldest_event_at))}` : "Событий пока нет"} · {number.format(storage.segments)} {plural(storage.segments, ["сегмент", "сегмента", "сегментов"])}. Прогноз заполнения не показывается: нет истории измерений.</p>
  </>;
}

export default function Infrastructure({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [data, setData] = useState<InfrastructureResponse | null>(null);
  const [loadedAt, setLoadedAt] = useState<Date | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const [reload, setReload] = useState(0);
  const unauthorized = useRef(onUnauthorized); unauthorized.current = onUnauthorized;

  useEffect(() => {
    const controller = new AbortController();
    setLoading(true);
    let timer: ReturnType<typeof setTimeout> | undefined;
    api<InfrastructureResponse>("GET", "/api/infrastructure", undefined, { signal: controller.signal })
      .then((response) => { if (!controller.signal.aborted) { setData(response); setLoadedAt(new Date()); setError(""); } })
      .catch((cause) => {
        if (controller.signal.aborted) return;
        if (cause instanceof ApiError && cause.status === 401) unauthorized.current(); else setError(errorText(cause));
      })
      .finally(() => {
        if (!controller.signal.aborted) {
          setLoading(false);
          timer = setTimeout(() => setReload((value) => value + 1), REFRESH_MS);
        }
      });
    return () => { controller.abort(); clearTimeout(timer); };
  }, [reload]);

  const nodes = data?.nodes ?? [];
  const agents = nodes.filter((node) => node.kind === "agent");
  const relays = nodes.filter((node) => node.kind === "relay");
  const attention = nodes.filter((node) => node.status === "offline" || node.status === "degraded").length;
  const fresh = (list: InfraNode[]) => list.filter((node) => node.status !== "offline" && node.status !== "unknown");
  const warn = data?.thresholds.spool_warn_percent ?? 80;
  const core = data?.core;
  const coreChip = core?.status === "online" ? { tone: "ok", text: "Онлайн" } : core?.status === "not_ready" ? { tone: "warn", text: "Не готов" } : { tone: "err", text: "Недоступен" };
  const walShare = core?.wal_bytes != null && core.wal_limit_bytes ? Math.min(100, Math.round(core.wal_bytes * 100 / core.wal_limit_bytes)) : null;
  const rates = fresh(relays).flatMap((node) => node.events_per_second === null ? [] : [node.events_per_second]);
  const rate = rates.length ? rates.reduce((sum, value) => sum + value, 0) : null;
  const counts = relays.flatMap((node) => node.delivery_failures === null ? [] : [node.delivery_failures]);
  const failures = counts.length ? counts.reduce((sum, value) => sum + value, 0) : "нет данных";
  const unknown = nodes.filter((node) => node.status === "unknown").length;
  const headline = !data ? "Инфраструктура" : error ? "Данные инфраструктуры устарели"
    : core?.status === "unavailable" ? "Core недоступен" : core?.status === "not_ready" ? "Core не готов"
    : !data.registry ? "Состояние узлов недоступно" : !nodes.length ? "Нет зарегистрированных узлов"
    : attention ? `${attention} ${plural(attention, ["узел требует", "узла требуют", "узлов требуют"])} внимания`
    : unknown ? `Состояние ${unknown} ${plural(unknown, ["узла", "узлов", "узлов"])} неизвестно` : "Узлы на связи";
  const logsLink = (node: InfraNode) => node.source && `/logs?${new URLSearchParams({ view: "events", range: "1h", q: sourceQuery(node.source) })}`;

  return <section className="infra-page page-stack">
    <div className="summary infra-summary">
      <div>
        <h1>{headline}</h1>
        <p className="muted">Agent → Relay → Core → Dock{core?.visibility_lag_ms != null ? ` · lag публикации в Core ${number.format(core.visibility_lag_ms / 1000)} с` : ""}</p>
      </div>
      <div className="infra-actions">
        <Link className="button" to="/access">Управление подключениями</Link>
        <button type="button" className="button" disabled={loading} onClick={() => setReload((value) => value + 1)}>
          {loadedAt ? `Обновлено ${clock.format(loadedAt)}` : "Обновить"}</button>
      </div>
    </div>
    {error && <p className="error" role="alert">Не удалось обновить состояние: {error}. Последний успешный снимок {loadedAt ? clock.format(loadedAt) : "ещё не получен"}; статусы ниже не подтверждены.</p>}
    {data && <div className={`infra-data${error ? " infra-stale" : ""}`} aria-label={error ? "Устаревший снимок" : "Текущий снимок"}>
      <section className="pipe" aria-label="Цепочка доставки">
        <div className="stage">
          <div className="stage-name"><b>Agent</b><Chip tone={groupChip(agents).tone}>{groupChip(agents).text}</Chip></div>
          <div className="stage-big">{agents.length}<small>{plural(agents.length, ["узел", "узла", "узлов"])}</small></div>
          <div className="stage-sub">{fresh(agents).length} на связи · {agents.filter((node) => node.status === "offline").length} отключено</div>
          <SpoolLine nodes={fresh(agents)} warn={warn} />
        </div>
        <div className="stage">
          <div className="stage-name"><b>Relay</b><Chip tone={groupChip(relays).tone}>{groupChip(relays).text}</Chip></div>
          <div className="stage-big">{rate === null ? "—" : number.format(rate)}<small>событий/с</small></div>
          <div className="stage-sub">{relays.length} {plural(relays.length, ["узел", "узла", "узлов"])} · сбоев доставки с запуска {failures}</div>
          <SpoolLine nodes={fresh(relays)} warn={warn} />
        </div>
        <div className="stage">
          <div className="stage-name"><b>Core</b><Chip tone={coreChip.tone}>{coreChip.text}</Chip></div>
          <div className="stage-big">{core?.visibility_lag_ms != null ? number.format(core.visibility_lag_ms / 1000) : "—"}<small>с lag публикации</small></div>
          <div className="stage-sub">{core?.version ? `v${core.version}` : "версия неизвестна"}</div>
          <div className="stage-sub">WAL {core?.wal_bytes != null ? bytes(core.wal_bytes) : "—"}{core?.wal_limit_bytes ? ` из ${bytes(core.wal_limit_bytes)}` : ""}</div>
          {walShare !== null && <Meter value={walShare} hot={walShare >= warn} />}
        </div>
        <div className="stage">
          <div className="stage-name"><b>Dock</b><Chip tone="ok">Онлайн</Chip></div>
          <div className="stage-big">{data.dock.sse_clients}<small>SSE-{plural(data.dock.sse_clients, ["клиент", "клиента", "клиентов"])}</small></div>
          <div className="stage-sub">v{data.dock.version} · 1 инстанс</div>
          <div className="stage-sub">активных сессий {data.dock.sessions ?? "нет данных"}</div>
        </div>
      </section>
      <div className="infra-grid">
        <section className="panel table-scroll" aria-label="Узлы">
          <div className="panel-head"><h2>Узлы <small className="muted">сначала проблемные</small></h2></div>
          {!data.registry ? <p className="empty">Не удалось получить список узлов из Core.</p> :
            !nodes.length ? <p className="empty">Нет зарегистрированных relay и агентов. Создайте их в <Link to="/access">Подключениях</Link>.</p> :
            <table className="nodes-table"><thead><tr><th>Узел</th><th>Статус</th><th>Heartbeat</th><th>Последнее событие</th><th>Спул</th><th>Версия</th></tr></thead>
              <tbody>{nodes.map((node) => {
                const status = STATUS[node.status], spool = percent(node.spool), link = logsLink(node);
                return <tr key={`${node.kind}-${node.id}`}>
                  <td className="node-name"><code>{node.kind} {node.name}</code>
                    {(node.relay || link) && <small>{node.relay && `через ${node.relay}`}{node.relay && link && " · "}{link && <Link to={link} title="Фильтр по конфигурации агента; принадлежность события агенту не проверяется">фильтр {node.source?.server}</Link>}</small>}</td>
                  <td><Chip tone={status.tone}>{status.label}</Chip>{node.reason && <small className="node-reason">{REASON[node.reason]}</small>}</td>
                  <td className="node-time">{age(node.heartbeat_at, data.as_of)}</td>
                  <td className="node-time">{age(node.last_event_at, data.as_of)}</td>
                  <td>{spool === null ? <span className="node-time">—</span> : <span className="node-spool"><Meter value={spool} hot={spool >= warn} />{spool}%</span>}</td>
                  <td className="node-time">{node.version ?? "—"}</td>
                </tr>;
              })}</tbody></table>}
          <p className="page-note node-note">Показатели узла относятся к последнему heartbeat. Ссылки строят фильтр по конфигурации, а не по подтверждённому agent_id. «Нет новых логов»: heartbeat приходит, а событий нет дольше {data.thresholds.idle_seconds / 60} мин. «Отключён»: heartbeat нет дольше {data.thresholds.heartbeat_ttl_seconds} с. «Нет данных»: heartbeat не приходил или relay агента не на связи. «Деградирован»: спул от {warn}% или ошибки доставки после последнего успеха.</p>
        </section>
        <section className="panel storage-card" aria-label="Хранилище">
          <div className="panel-head"><h2>Хранилище Core {data.storage && <small className="muted">retention {number.format(data.storage.retention_seconds / 86_400)} дн</small>}</h2></div>
          <div className="storage-body">{data.storage ? <Storage storage={data.storage} wal={core?.wal_bytes ?? 0} /> : <p className="muted">Core недоступен: размеры хранилища неизвестны.</p>}</div>
        </section>
      </div>
    </div>}
  </section>;
}
