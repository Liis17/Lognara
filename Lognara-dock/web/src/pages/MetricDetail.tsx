import { useEffect, useState } from "react";
import { Link, useSearchParams } from "react-router";
import {
  api,
  ApiError,
  errorText,
  type SeriesMetric,
  type SeriesQuery,
  type SeriesReply,
  type Split,
  type MetricAlertKey,
} from "../api";
import Chart from "../components/Chart";
import MetricDialog from "../components/MetricDialog";
import MetricEditor, { aggregationText, kindText } from "./MetricEditor";
import MetricAlertEditor from "./MetricAlertEditor";

const DAY = 86400000;
const ranges = [
  { label: "1 час", ms: 3600000 },
  { label: "24 часа", ms: DAY },
  { label: "7 дней", ms: 7 * DAY },
  { label: "30 дней", ms: 30 * DAY },
  { label: "1 год", ms: 365 * DAY },
  { label: "Всё время", ms: null },
];
const steps = [
  10, 30, 60, 300, 600, 1800, 3600, 10800, 21600, 43200, 86400, 604800,
];

export default function MetricDetail({
  onUnauthorized,
}: {
  onUnauthorized: () => void;
}) {
  const [params] = useSearchParams();
  const identity = params.toString();
  const alertKey: MetricAlertKey = {
    server: params.get("server") ?? "",
    service: params.get("service") ?? "",
    name: params.get("name") ?? "",
    origin: (params.get("origin") ?? "counter") as MetricAlertKey["origin"],
  };
  const alertKeyValid =
    !!alertKey.server && !!alertKey.service && !!alertKey.name &&
    ["counter", "gauge", "rule"].includes(alertKey.origin);
  const [metric, setMetric] = useState<SeriesMetric | null>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState("");
  const [range, setRange] = useState(ranges[1]);
  const [environment, setEnvironment] = useState("");
  const [split, setSplit] = useState<Split>("none");
  const [chart, setChart] = useState<SeriesReply | null>(null);
  const [loading, setLoading] = useState(false);
  const [chartError, setChartError] = useState("");
  const [editing, setEditing] = useState(false);
  const [generation, setGeneration] = useState(0);
  const fail = (e: unknown, setter: (text: string) => void) => {
    if (e instanceof ApiError && e.status === 401) onUnauthorized();
    else setter(errorText(e));
  };
  useEffect(() => {
    setEnvironment("");
    setSplit("none");
    setEditing(false);
  }, [identity]);
  useEffect(() => {
    const controller = new AbortController();
    setReady(false);
    setMetric(null);
    setError("");
    api<{ metrics: SeriesMetric[] }>(
      "POST",
      "/api/series/lookup",
      { keys: [{ server: params.get("server"), service: params.get("service"), name: params.get("name"), origin: params.get("origin") }] },
      { signal: controller.signal },
    )
      .then((reply) =>
        setMetric(
          reply.metrics.find(
            (m) =>
              m.server === params.get("server") &&
              m.service === params.get("service") &&
              m.name === params.get("name") &&
              m.origin === params.get("origin"),
          ) ?? null,
        ),
      )
      .catch((e) => {
        if (!controller.signal.aborted) fail(e, setError);
      })
      .finally(() => {
        if (!controller.signal.aborted) setReady(true);
      });
    return () => controller.abort();
  }, [identity, generation]);
  useEffect(() => {
    if (!metric) return;
    const controller = new AbortController();
    const to = Date.now();
    const from =
      range.ms === null
        ? metric.first_seen
          ? Date.parse(metric.first_seen)
          : to - 365 * DAY
        : to - range.ms;
    setLoading(true);
    setChart(null);
    setChartError("");
    const query: SeriesQuery = {
      server: metric.server,
      service: metric.service,
      name: metric.name,
      origin: metric.origin,
      from: new Date(from).toISOString(),
      to: new Date(to).toISOString(),
      step_seconds: steps.find((s) => (to - from) / 1000 / s <= 1000) ?? 604800,
      split,
      environment: environment || undefined,
    };
    api<SeriesReply>("POST", "/api/series/query", query, {
      signal: controller.signal,
    })
      .then((reply) => {
        if (!controller.signal.aborted) setChart(reply);
      })
      .catch((e) => {
        if (!controller.signal.aborted) fail(e, setChartError);
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [metric, range, split, environment]);
  return (
    <section className="metrics metric-detail">
      <Link to="/metrics">← Все метрики</Link>
      {alertKeyValid && (
        <MetricAlertEditor
          key={identity}
          keyValue={alertKey}
          metric={metric}
          onUnauthorized={onUnauthorized}
        />
      )}
      {!ready && <p className="muted">Загрузка…</p>}
      {error && <p className="error">{error}</p>}
      {ready && !metric && !error && (
        <p>Метрика не найдена на выбранном сервере.</p>
      )}
      {metric && (
        <section className="chart-panel">
          <div className="toolbar">
            <h1>{metric.definition?.title || metric.name}</h1>
            <button onClick={() => setEditing(true)}>Настроить</button>
            <button onClick={() => setGeneration((g) => g + 1)}>
              Обновить
            </button>
          </div>
          <p className="muted">
            {metric.server} / {metric.service} / {metric.name}
          </p>
          {metric.definition?.description && (
            <p>{metric.definition.description}</p>
          )}
          {metric.definition?.enabled === false && (
            <p className="muted">
              Определение отключено. Исторические точки доступны.
            </p>
          )}
          {chart && (
            <p className="muted">
              {kindText[chart.kind]}, агрегация:{" "}
              {aggregationText[chart.aggregation]}
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
            <select
              aria-label="Разбиение"
              value={split}
              onChange={(e) => setSplit(e.target.value as Split)}
            >
              <option value="none">Сумма</option>
              <option value="instance">По экземплярам</option>
              <option value="labels">По меткам</option>
            </select>
            {metric.environments.length > 0 && (
              <select
                aria-label="Окружение"
                value={environment}
                onChange={(e) => setEnvironment(e.target.value)}
              >
                <option value="">Все окружения</option>
                {metric.environments.map((env) => (
                  <option key={env}>{env}</option>
                ))}
              </select>
            )}
          </div>
          {loading && <p className="muted">Загрузка…</p>}
          {chartError && <p className="error">{chartError}</p>}
          {chart &&
            (chart.series.some((line) =>
              line.values.some((value) => value !== null),
            ) ? (
              <Chart
                buckets={chart.buckets}
                series={chart.series}
                unit={chart.unit}
              />
            ) : (
              <p className="muted">За выбранный период точек нет</p>
            ))}
          {editing && (
            <MetricDialog
              title="Настройка метрики"
              onClose={() => setEditing(false)}
            >
              <MetricEditor
                definition={metric.definition}
                start={{
                  server: metric.server,
                  service: metric.service,
                  name: metric.name,
                  kind: metric.origin === "gauge" ? "gauge" : "delta",
                  rule: metric.origin === "rule",
                }}
                onUnauthorized={onUnauthorized}
                onDone={() => {
                  setEditing(false);
                  setGeneration((g) => g + 1);
                }}
                onCancel={() => setEditing(false)}
              />
            </MetricDialog>
          )}
        </section>
      )}
    </section>
  );
}
