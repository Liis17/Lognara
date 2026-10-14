import { useEffect, useState } from "react";
import {
  api,
  ApiError,
  errorText,
  type Definition,
  type SeriesMetric,
  type SeriesQuery,
  type SeriesReply,
  type Split,
} from "../api";
import Chart from "../components/Chart";
import MetricEditor, { aggregationText, kindText, type EditorStart } from "./MetricEditor";

const DAY = 86_400_000;

/** Пресеты диапазона; ms: null означает "от первой точки метрики". */
const ranges: { label: string; ms: number | null }[] = [
  { label: "1 час", ms: 3_600_000 },
  { label: "24 часа", ms: DAY },
  { label: "7 дней", ms: 7 * DAY },
  { label: "30 дней", ms: 30 * DAY },
  { label: "1 год", ms: 365 * DAY },
  { label: "Всё время", ms: null },
];

const steps = [10, 30, 60, 300, 600, 1800, 3600, 10800, 21600, 43200, 86400, 604800];

/** Самый мелкий шаг, при котором точек не больше 1000. */
function pickStep(seconds: number): number {
  return steps.find((step) => seconds / step <= 1000) ?? steps[steps.length - 1];
}

const originText: Record<SeriesMetric["origin"], string> = {
  counter: "счётчик",
  gauge: "показатель",
  rule: "regex",
};

const splitText: Record<Split, string> = {
  none: "Сумма",
  instance: "По экземплярам",
  labels: "По меткам",
};

interface Editor {
  definition: Definition | null;
  start: EditorStart;
}

export default function Metrics({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [metrics, setMetrics] = useState<SeriesMetric[]>([]);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState("");
  const [selected, setSelected] = useState<SeriesMetric | null>(null);
  const [range, setRange] = useState(ranges[1]);
  const [split, setSplit] = useState<Split>("none");
  /** Пустая строка означает все окружения. */
  const [environment, setEnvironment] = useState("");
  const [chart, setChart] = useState<SeriesReply | null>(null);
  const [chartError, setChartError] = useState("");
  const [loading, setLoading] = useState(false);
  const [editor, setEditor] = useState<Editor | null>(null);

  // Выбор хранится как объект метрики, а сама метрика берётся из списка: после перечитывания данные свежие.
  const metric =
    selected &&
    metrics.find(
      (item) => item.service === selected.service && item.name === selected.name && item.origin === selected.origin,
    );

  const load = () =>
    api<{ metrics: SeriesMetric[] }>("GET", "/api/series")
      .then((reply) => {
        setMetrics(reply.metrics);
        setError("");
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) onUnauthorized();
        else setError(errorText(e));
      })
      .finally(() => setReady(true));

  useEffect(() => {
    load();
  }, []);

  useEffect(() => {
    if (!metric) return;
    let cancelled = false;
    const to = Date.now();
    const from =
      range.ms !== null ? to - range.ms : metric.first_seen ? Date.parse(metric.first_seen) : to - 365 * DAY;
    setLoading(true);
    setChartError("");
    const query: SeriesQuery = {
      service: metric.service,
      name: metric.name,
      from: new Date(from).toISOString(),
      to: new Date(to).toISOString(),
      step_seconds: pickStep((to - from) / 1000),
      split,
      environment: environment || undefined,
    };
    api<SeriesReply>("POST", "/api/series/query", query)
      .then((reply) => {
        if (!cancelled) setChart(reply);
      })
      .catch((e) => {
        if (cancelled) return;
        setChart(null);
        if (e instanceof ApiError && e.status === 401) onUnauthorized();
        else setChartError(errorText(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [metric, range, split, environment]);

  const select = (item: SeriesMetric) => {
    setSelected(item);
    setEnvironment("");
    setChart(null);
    setEditor(null);
  };

  const configure = (item: SeriesMetric) =>
    setEditor({
      definition: item.definition,
      start: {
        service: item.service,
        name: item.name,
        kind: item.origin === "gauge" ? "gauge" : "delta",
        rule: false,
      },
    });

  const newFromLogs = () =>
    setEditor({ definition: null, start: { service: "", name: "", kind: "delta", rule: true } });

  const done = () => {
    setEditor(null);
    load();
  };

  const groups = new Map<string, SeriesMetric[]>();
  for (const item of metrics) groups.set(item.service, [...(groups.get(item.service) ?? []), item]);

  return (
    <section className="metrics">
      <aside className="metrics-list">
        <div className="toolbar">
          <strong>Метрики</strong>
          <button onClick={newFromLogs}>Новая метрика из текста логов</button>
        </div>
        {error && <p className="error">{error}</p>}
        {!ready && <p className="muted">Загрузка…</p>}
        {ready && metrics.length === 0 && !error && <p className="muted">Метрик пока нет</p>}
        {[...groups].map(([service, items]) => (
          <div key={service}>
            <h3>{service}</h3>
            <ul className="metric-items">
              {items.map((item) => {
                const disabled = item.definition !== null && !item.definition.enabled;
                return (
                  <li key={`${item.origin}:${item.name}`}>
                    <button
                      className={`metric${item === metric ? " active" : ""}${disabled ? " muted" : ""}`}
                      onClick={() => select(item)}
                    >
                      <span className="title">{item.definition?.title || item.name}</span>
                      <span className="tag">{originText[item.origin]}</span>
                      {item.definition?.unit && <span className="muted">{item.definition.unit}</span>}
                    </button>
                  </li>
                );
              })}
            </ul>
          </div>
        ))}
      </aside>
      <div className="metrics-main">
        {editor && (
          <MetricEditor
            key={`${editor.definition?.id ?? "new"}/${editor.start.service}/${editor.start.name}/${editor.start.rule}`}
            definition={editor.definition}
            start={editor.start}
            onUnauthorized={onUnauthorized}
            onDone={done}
            onCancel={() => setEditor(null)}
          />
        )}
        {metric ? (
          <section className="chart-panel">
            <div className="toolbar">
              <h2>{metric.definition?.title || metric.name}</h2>
              <button onClick={() => configure(metric)}>Настроить</button>
            </div>
            <p className="muted">
              {metric.service} / {metric.name}
            </p>
            {metric.definition?.description && <p>{metric.definition.description}</p>}
            {chart && (
              <p className="muted">
                {kindText[chart.kind]}, агрегация: {aggregationText[chart.aggregation]}
              </p>
            )}
            <div className="toolbar">
              {ranges.map((item) => (
                <button
                  key={item.label}
                  className={item === range ? "active" : ""}
                  onClick={() => setRange(item)}
                >
                  {item.label}
                </button>
              ))}
              <select value={split} onChange={(e) => setSplit(e.target.value as Split)}>
                {Object.entries(splitText).map(([value, label]) => (
                  <option key={value} value={value}>
                    {label}
                  </option>
                ))}
              </select>
              {metric.environments.length > 0 && (
                <select aria-label="Окружение" value={environment} onChange={(e) => setEnvironment(e.target.value)}>
                  <option value="">Все окружения</option>
                  {metric.environments.map((item) => (
                    <option key={item} value={item}>
                      {item}
                    </option>
                  ))}
                </select>
              )}
            </div>
            {loading && <p className="muted">Загрузка…</p>}
            {chartError && <p className="error">{chartError}</p>}
            {chart && chart.buckets.length > 0 && (
              <Chart buckets={chart.buckets} series={chart.series} unit={chart.unit} />
            )}
            {chart && chart.buckets.length === 0 && <p className="muted">За выбранный период точек нет</p>}
          </section>
        ) : (
          <p className="muted">Выберите метрику слева</p>
        )}
      </div>
    </section>
  );
}
