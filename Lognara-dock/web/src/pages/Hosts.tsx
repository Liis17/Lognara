import { useEffect, useMemo, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import {
  api,
  ApiError,
  errorText,
  type SeriesLine,
  type SeriesMetric,
  type SeriesQuery,
  type SeriesReply,
} from "../api";
import LineChart from "../components/LineChart";
import DateRangePicker from "../components/DateRangePicker";

const DAY = 24 * 60 * 60 * 1000;
const STEP_SECONDS = [10, 30, 60, 300, 600, 1800, 3600, 10800, 21600, 43200, 86400, 604800];
const byteNumber = 1024 ** 3;
const rateNumber = 1024 ** 2;
const periodPresets = [
  { label: "1 ч", milliseconds: 60 * 60 * 1000 },
  { label: "6 ч", milliseconds: 6 * 60 * 60 * 1000 },
  { label: "24 ч", milliseconds: DAY },
  { label: "7 дн", milliseconds: 7 * DAY },
  { label: "30 дн", milliseconds: 30 * DAY },
  { label: "1 год", milliseconds: 365 * DAY },
];

type Host = { name: string; hasData: boolean };
type Scale = "identity" | "gibibytes" | "mebibytes-per-second";
type Presentation = { section: string; chart: string; unit: string; label: string; scale: Scale };
type HostChart = { section: string; title: string; unit: string; buckets: string[]; series: SeriesLine[] };

const fixed: Record<string, Presentation> = {
  "cpu.usage_percent": { section: "CPU", chart: "Загрузка", unit: "%", label: "Использование", scale: "identity" },
  "cpu.iowait_percent": { section: "CPU", chart: "Загрузка", unit: "%", label: "Ожидание ввода-вывода", scale: "identity" },
  "cpu.steal_percent": { section: "CPU", chart: "Загрузка", unit: "%", label: "Steal", scale: "identity" },
  "cpu.cores": { section: "CPU", chart: "Ядра", unit: "ядер", label: "Ядра", scale: "identity" },
  "load.1": { section: "Нагрузка", chart: "Load average", unit: "", label: "1 минута", scale: "identity" },
  "load.5": { section: "Нагрузка", chart: "Load average", unit: "", label: "5 минут", scale: "identity" },
  "load.15": { section: "Нагрузка", chart: "Load average", unit: "", label: "15 минут", scale: "identity" },
  "memory.used_percent": { section: "Память", chart: "Использование", unit: "%", label: "Занято", scale: "identity" },
  "memory.total_bytes": { section: "Память", chart: "Объём", unit: "ГиБ", label: "Всего", scale: "gibibytes" },
  "memory.available_bytes": { section: "Память", chart: "Объём", unit: "ГиБ", label: "Доступно", scale: "gibibytes" },
  "swap.used_percent": { section: "Swap", chart: "Использование", unit: "%", label: "Занято", scale: "identity" },
  "swap.used_bytes": { section: "Swap", chart: "Объём", unit: "ГиБ", label: "Занято", scale: "gibibytes" },
  "proc.count": { section: "Процессы", chart: "Количество", unit: "процессов", label: "Процессы", scale: "identity" },
  "proc.top_cpu_percent": { section: "Процессы", chart: "Максимальная загрузка CPU", unit: "%", label: "Самый активный процесс", scale: "identity" },
};

function presentation(metric: SeriesMetric): Presentation {
  const known = fixed[metric.name];
  if (known) return known;
  let match = metric.name.match(/^fs\.(.+)\.(free_bytes|used_percent)$/u);
  if (match) {
    return {
      section: "Файловые системы",
      chart: match[2] === "used_percent" ? "Заполнение" : "Свободное место",
      unit: match[2] === "used_percent" ? "%" : "ГиБ",
      label: match[1],
      scale: match[2] === "used_percent" ? "identity" : "gibibytes",
    };
  }
  match = metric.name.match(/^disk\.(.+)\.(read_bytes|write_bytes)$/u);
  if (match) {
    return {
      section: "Диски",
      chart: "Скорость чтения и записи",
      unit: "МиБ/с",
      label: `${match[1]} · ${match[2] === "read_bytes" ? "чтение" : "запись"}`,
      scale: "mebibytes-per-second",
    };
  }
  match = metric.name.match(/^net\.(.+)\.(rx_bytes|tx_bytes)$/u);
  if (match) {
    return {
      section: "Сеть",
      chart: "Скорость передачи",
      unit: "МиБ/с",
      label: `${match[1]} · ${match[2] === "rx_bytes" ? "приём" : "передача"}`,
      scale: "mebibytes-per-second",
    };
  }
  match = metric.name.match(/^pressure\.(cpu|memory|io)\.some_avg10$/u);
  if (match) {
    const resource = match[1]!;
    return {
      section: "PSI",
      chart: "Ожидание ресурсов за 10 секунд",
      unit: "%",
      label: { cpu: "CPU", memory: "Память", io: "Ввод-вывод" }[resource]!,
      scale: "identity",
    };
  }
  match = metric.name.match(/^gpu\.(.+)\.(util_percent|memory_used_bytes|memory_used_percent|temperature_c|power_w)$/u);
  if (match) {
    const gpuFields: Record<string, { chart: string; unit: string; label: string; scale: Scale }> = {
      util_percent: { chart: "Загрузка", unit: "%", label: "Использование", scale: "identity" },
      memory_used_bytes: { chart: "Память", unit: "ГиБ", label: "Занято", scale: "gibibytes" },
      memory_used_percent: { chart: "Память, %", unit: "%", label: "Занято", scale: "identity" },
      temperature_c: { chart: "Температура", unit: "°C", label: "Температура", scale: "identity" },
      power_w: { chart: "Потребление", unit: "Вт", label: "Мощность", scale: "identity" },
    };
    const field = gpuFields[match[2]!]!;
    return { section: "GPU", ...field, label: `${match[1]!} · ${field.label}` };
  }
  return {
    section: "Другие метрики",
    chart: metric.definition?.title || metric.name,
    unit: metric.definition?.unit ?? "",
    label: metric.name,
    scale: "identity",
  };
}

function hasSeriesData(metric: SeriesMetric): boolean {
  return metric.first_seen !== null && metric.origin !== "rule";
}

function chartStep(from: string, to: string): number {
  const seconds = (Date.parse(to) - Date.parse(from)) / 1000;
  return STEP_SECONDS.find((step) => seconds / step <= 1000) ?? 604800;
}

function transform(value: number | null, scale: Scale, step: number): number | null {
  if (value === null) return null;
  if (scale === "gibibytes") return value / byteNumber;
  if (scale === "mebibytes-per-second") return value / step / rateNumber;
  return value;
}

function makeCharts(rows: { metric: SeriesMetric; reply: SeriesReply }[], step: number): HostChart[] {
  const charts = new Map<string, HostChart>();
  for (const { metric, reply } of rows) {
    const spec = presentation(metric);
    const key = `${spec.section}\u0000${spec.chart}\u0000${spec.unit}`;
    const line = reply.series[0];
    if (!line || !line.values.some((value) => value !== null)) continue;
    let chart = charts.get(key);
    if (!chart) {
      chart = { section: spec.section, title: spec.chart, unit: spec.unit, buckets: reply.buckets, series: [] };
      charts.set(key, chart);
    }
    chart.series.push({
      label: spec.label,
      values: line.values.map((value) => transform(value, spec.scale, step)),
    });
  }
  const order = ["CPU", "Нагрузка", "Память", "Swap", "Файловые системы", "Диски", "Сеть", "PSI", "GPU", "Процессы", "Другие метрики"];
  return [...charts.values()].sort((a, b) => order.indexOf(a.section) - order.indexOf(b.section) || a.title.localeCompare(b.title, "ru"));
}

function HostChartPanel({ chart, onSelectRange }: { chart: HostChart; onSelectRange: (from: number, to: number) => void }) {
  const [visibleSeries, setVisibleSeries] = useState<ReadonlySet<number>>(() => new Set(chart.series.map((_line, index) => index)));
  const toggleSeries = (index: number) => setVisibleSeries((current) => {
    const next = new Set(current);
    if (next.has(index)) next.delete(index); else next.add(index);
    return next;
  });
  return <article className="host-chart-panel">
    <h3>{chart.title}</h3>
    <LineChart buckets={chart.buckets} series={chart.series} unit={chart.unit} visibleSeries={visibleSeries}
      onToggleSeries={toggleSeries} onSelectRange={onSelectRange} onZoom={() => {}} />
  </article>;
}

export default function Hosts({ onUnauthorized }: { onUnauthorized: () => void }) {
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const [hosts, setHosts] = useState<Host[]>([]);
  const [hostsReady, setHostsReady] = useState(false);
  const [from, setFrom] = useState(() => new Date(Date.now() - DAY).toISOString());
  const [to, setTo] = useState(() => new Date(Date.now()).toISOString());
  const [metrics, setMetrics] = useState<SeriesMetric[]>([]);
  const [charts, setCharts] = useState<HostChart[]>([]);
  const [metricsReady, setMetricsReady] = useState(false);
  const [error, setError] = useState("");
  const hostName = searchParams.get("host") ?? "";
  const host = hosts.find((candidate) => candidate.name === hostName) ?? hosts[0] ?? null;
  const stepSeconds = useMemo(() => chartStep(from, to), [from, to]);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    const registry = api<{ relays: { name: string; kind?: string }[] }>("GET", "/api/relays", undefined, { signal });
    const series = api<{ metrics: SeriesMetric[] }>("GET", `/api/series?${new URLSearchParams({ group_by: "server", service: "host" })}`, undefined, { signal });
    Promise.allSettled([registry, series]).then(([registryReply, seriesReply]) => {
      if (signal.aborted) return;
      const registered = registryReply.status === "fulfilled"
        ? registryReply.value.relays.filter((relay) => relay.kind === "host").map((relay) => relay.name)
        : [];
      const withData = new Set(seriesReply.status === "fulfilled"
        ? seriesReply.value.metrics.filter(hasSeriesData).map((metric) => metric.server)
        : []);
      const names = new Set([...registered, ...withData]);
      setHosts([...names]
        .map((name) => ({ name, hasData: withData.has(name) }))
        .sort((a, b) => Number(b.hasData) - Number(a.hasData) || a.name.localeCompare(b.name, "ru")));
      const failures = [registryReply, seriesReply].flatMap((reply) => reply.status === "rejected" ? [reply.reason] : []);
      if (failures.some((cause) => cause instanceof ApiError && cause.status === 401)) onUnauthorized();
      else if (failures.length > 0) setError(errorText(failures[0]));
      setHostsReady(true);
    });
    return () => controller.abort();
  }, [onUnauthorized]);

  useEffect(() => {
    if (!hostsReady) return;
    if (!host) {
      if (hostName) setSearchParams({}, { replace: true });
      return;
    }
    if (hostName !== host.name) setSearchParams({ host: host.name }, { replace: true });
  }, [host, hostName, hostsReady, setSearchParams]);

  useEffect(() => {
    if (!hostsReady || !host) return;
    const controller = new AbortController();
    setError("");
    setMetrics([]);
    setCharts([]);
    setMetricsReady(false);
    const query = new URLSearchParams({ group_by: "server", server: host.name, service: "host" });
    (async () => {
      try {
        const reply = await api<{ metrics: SeriesMetric[] }>("GET", `/api/series?${query}`, undefined, { signal: controller.signal });
        if (controller.signal.aborted) return;
        const rows = reply.metrics.filter(hasSeriesData);
        setMetrics(rows);
        const results: { metric: SeriesMetric; reply: SeriesReply }[] = [];
        let next = 0;
        await Promise.all(Array.from({ length: Math.min(2, rows.length) }, async () => {
          while (next < rows.length && !controller.signal.aborted) {
            const metric = rows[next++];
            const request: SeriesQuery = {
              server: metric.server,
              service: "host",
              name: metric.name,
              origin: metric.origin,
              from,
              to,
              step_seconds: stepSeconds,
              split: "none",
            };
            try {
              const data = await api<SeriesReply>("POST", "/api/series/query", request, { signal: controller.signal });
              if (!controller.signal.aborted) results.push({ metric, reply: data });
            } catch (cause) {
              if (!controller.signal.aborted) {
                if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
                else setError(errorText(cause));
              }
            }
          }
        }));
        if (!controller.signal.aborted) setCharts(makeCharts(results, stepSeconds));
      } catch (cause) {
        if (!controller.signal.aborted) {
          if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
          else setError(errorText(cause));
        }
      } finally {
        if (!controller.signal.aborted) setMetricsReady(true);
      }
    })();
    return () => controller.abort();
  }, [from, host?.name, hostsReady, onUnauthorized, stepSeconds, to]);

  const setRange = (nextFrom: number, nextTo: number) => {
    setFrom(new Date(nextFrom).toISOString());
    setTo(new Date(nextTo).toISOString());
  };
  const openLogs = (selectedFrom: number, selectedTo: number) => {
    if (!host) return;
    const query = `server:${JSON.stringify(host.name)} AND backend:"host" AND service:"host"`;
    navigate(`/logs?${new URLSearchParams({
      view: "events",
      from: new Date(selectedFrom).toISOString(),
      to: new Date(selectedTo).toISOString(),
      q: query,
    })}`);
  };
  const groups = useMemo(() => {
    const sections = new Map<string, HostChart[]>();
    for (const chart of charts) {
      const group = sections.get(chart.section) ?? [];
      group.push(chart);
      sections.set(chart.section, group);
    }
    return [...sections.entries()];
  }, [charts]);

  return <section className="hosts-page">
    <div className="toolbar hosts-toolbar">
      <div><p className="eyebrow">Системные метрики hwagent</p><h1>Хосты</h1></div>
      {host && (hosts.length > 1 ? <label className="host-picker">Хост
        <select aria-label="Хост" value={host.name} onChange={(event) => setSearchParams({ host: event.target.value })}>
          {hosts.map((item) => <option key={item.name} value={item.name}>{item.hasData ? item.name : `${item.name} · нет данных`}</option>)}
        </select>
      </label> : <p className="muted">Хост <code>{host.name}</code>{host.hasData ? null : " · нет данных"}</p>)}
    </div>
    {error && <p className="error" role="alert">{error}</p>}
    {hostsReady && hosts.length === 0 && <div className="host-empty-state">
      <h2>Хосты ещё не подключены</h2>
      <p className="muted">Добавьте хост и установите на нём lognara-hwagent, чтобы получать метрики и системные логи.</p>
      <Link className="button solid" to="/access">Открыть «Подключения»</Link>
    </div>}
    {host && <>
      <div className="toolbar host-range-toolbar" aria-label="Период метрик">
        {periodPresets.map((preset) => <button key={preset.label} type="button" onClick={() => {
          const end = Date.now();
          setRange(end - preset.milliseconds, end);
        }}>{preset.label}</button>)}
        <DateRangePicker from={from} to={to} onChange={(start, end) => { setFrom(start); setTo(end); }} />
      </div>
      <p className="muted host-series-source">Источник: server <code>{host.name}</code> · service <code>host</code></p>
      {!metricsReady && <p className="muted" role="status">Загружаем метрики хоста…</p>}
      {metricsReady && metrics.length === 0 && !error && <div className="host-empty-state">
        <h2>Метрик пока нет</h2>
        <p className="muted">Core пока не получил ряды hwagent для этого хоста. hwagent отправляет метрики под именем из <code>LOGNARA_SERVER</code> или hostname машины; такие серверы появляются в списке хостов автоматически.</p>
        <Link className="button" to="/access">Проверить «Подключения»</Link>
      </div>}
      {metricsReady && metrics.length > 0 && charts.length === 0 && !error && <div className="host-empty-state">
        <h2>За выбранный период точек нет</h2>
        <p className="muted">Выберите другой период или проверьте, что hwagent продолжает отправлять метрики.</p>
        <Link className="button" to="/access">Проверить «Подключения»</Link>
      </div>}
      {groups.map(([section, items]) => <section className="host-metric-group" key={section} aria-label={section}>
        <h2>{section}</h2>
        <div className="host-chart-grid">
          {items.map((chart) => <HostChartPanel key={`${chart.title}/${chart.unit}`} chart={chart} onSelectRange={openLogs} />)}
        </div>
      </section>)}
      {metricsReady && metrics.length > 0 && charts.length > 0 && <p className="muted host-range-hint">Выделите участок графика, чтобы открыть логи этого хоста за выбранный диапазон.</p>}
    </>}
  </section>;
}
