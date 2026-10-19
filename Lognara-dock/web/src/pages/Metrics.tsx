import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import {
  api,
  ApiError,
  errorText,
  type SeriesMetric,
  type SeriesQuery,
  type SeriesReply,
} from "../api";
import Chart from "../components/Chart";
import MetricCreate from "./MetricCreate";

export function metricUrl(metric: SeriesMetric): string {
  return (
    "/metrics/detail?" +
    new URLSearchParams({
      server: metric.server,
      service: metric.service,
      name: metric.name,
      origin: metric.origin,
    })
  );
}

function chartLoader() {
  const cache = new Map<string, Promise<SeriesReply>>();
  const queue: (() => void)[] = [];
  let controller = new AbortController();
  let active = 0;
  const pump = () => {
    while (active < 2 && queue.length) queue.shift()!();
  };
  return {
    open() {
      if (controller.signal.aborted) {
        controller = new AbortController();
        cache.clear();
      }
    },
    close() {
      controller.abort();
      pump();
    },
    load(query: SeriesQuery) {
      const key = JSON.stringify(query);
      let promise = cache.get(key);
      if (!promise) {
        promise = new Promise<SeriesReply>((resolve, reject) => {
          queue.push(() => {
            active++;
            api<SeriesReply>("POST", "/api/series/query", query, {
              signal: controller.signal,
            })
              .then(resolve, reject)
              .finally(() => {
                active--;
                pump();
              });
          });
          pump();
        });
        cache.set(key, promise);
      }
      return promise;
    },
  };
}

const originText = { counter: "счётчик", gauge: "показатель", rule: "regex" };

function Tile({
  metric,
  loader,
  to,
  onUnauthorized,
}: {
  metric: SeriesMetric;
  loader: ReturnType<typeof chartLoader>;
  to: number;
  onUnauthorized: () => void;
}) {
  const ref = useRef<HTMLAnchorElement>(null);
  const [visible, setVisible] = useState(false);
  const [chart, setChart] = useState<SeriesReply | null>(null);
  const [error, setError] = useState("");
  const disabled = metric.definition?.enabled === false;
  useEffect(() => {
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        setVisible(true);
        observer.disconnect();
      }
    });
    observer.observe(ref.current!);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!visible || disabled) return;
    let cancelled = false;
    setChart(null);
    setError("");
    loader
      .load({
        server: metric.server,
        service: metric.service,
        name: metric.name,
        origin: metric.origin,
        from: new Date(to - 86400000).toISOString(),
        to: new Date(to).toISOString(),
        step_seconds: 1800,
        split: "none",
      })
      .then((reply) => {
        if (!cancelled) setChart(reply);
      })
      .catch((e) => {
        if (!cancelled) {
          if (e instanceof ApiError && e.status === 401) onUnauthorized();
          else setError(errorText(e));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [visible, disabled, metric, loader, to]);
  const hasValues = chart?.series.some((line) =>
    line.values.some((value) => value !== null),
  );
  return (
    <Link
      ref={ref}
      to={metricUrl(metric)}
      className={"metric-tile" + (disabled ? " disabled" : "")}
    >
      <div className="metric-tile-heading">
        <strong>{metric.definition?.title || metric.name}</strong>
        <span className="tag">{originText[metric.origin]}</span>
      </div>
      <span className="muted">
        {metric.name}
        {metric.definition?.unit ? " · " + metric.definition.unit : ""}
      </span>
      <div className="metric-sparkline" aria-label="Миниграфик за 24 часа">
        {disabled ? (
          <span className="muted">Отключена</span>
        ) : error ? (
          <span className="error">{error}</span>
        ) : !chart ? (
          <span className="muted">{visible ? "Загрузка…" : "За 24 часа"}</span>
        ) : hasValues ? (
          <Chart
            compact
            buckets={chart.buckets}
            series={chart.series}
            unit={chart.unit}
          />
        ) : (
          <span className="muted">За 24 часа данных нет</span>
        )}
      </div>
    </Link>
  );
}

export default function Metrics({
  onUnauthorized,
}: {
  onUnauthorized: () => void;
}) {
  const [metrics, setMetrics] = useState<SeriesMetric[]>([]);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState("");
  const [creating, setCreating] = useState(false);
  const [generation, setGeneration] = useState(0);
  const [to, setTo] = useState(() => Math.floor(Date.now() / 60000) * 60000);
  const loader = useMemo(chartLoader, [generation]);
  useEffect(() => {
    loader.open();
    return () => loader.close();
  }, [loader]);
  useEffect(() => {
    const controller = new AbortController();
    api<{ metrics: SeriesMetric[] }>(
      "GET",
      "/api/series?group_by=server",
      undefined,
      { signal: controller.signal },
    )
      .then((r) => {
        setMetrics(r.metrics);
        setError("");
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          if (e instanceof ApiError && e.status === 401) onUnauthorized();
          else setError(errorText(e));
        }
      })
      .finally(() => {
        if (!controller.signal.aborted) setReady(true);
      });
    return () => controller.abort();
  }, [generation]);
  const refresh = () => {
    setGeneration((g) => g + 1);
    setTo(Math.floor(Date.now() / 60000) * 60000);
  };
  const servers = new Map<string, Map<string, SeriesMetric[]>>();
  for (const metric of metrics) {
    if (!servers.has(metric.server)) servers.set(metric.server, new Map());
    const services = servers.get(metric.server)!;
    if (!services.has(metric.service)) services.set(metric.service, []);
    services.get(metric.service)!.push(metric);
  }
  return (
    <section className="metrics metrics-overview">
      <div className="toolbar">
        <h1>Метрики</h1>
        <button onClick={() => setCreating(true)}>Новая метрика</button>
        <button onClick={refresh}>Обновить</button>
        <span className="muted">Миниграфики за 24 часа</span>
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {!ready && <p className="muted">Загрузка…</p>}
      {ready && !error && metrics.length === 0 && (
        <p className="muted">
          Метрик пока нет. Создайте правило или выберите готовый шаблон.
        </p>
      )}
      {[...servers]
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([server, services]) => (
          <section className="metric-server" key={server}>
            <h2>{server || "Сервер без имени"}</h2>
            {[...services]
              .sort(([a], [b]) => a.localeCompare(b))
              .map(([service, items]) => (
                <section className="metric-service" key={service}>
                  <h3>{service}</h3>
                  <div className="metric-grid">
                    {items.map((metric) => (
                      <Tile
                        key={metric.origin + ":" + metric.name}
                        metric={metric}
                        loader={loader}
                        to={to}
                        onUnauthorized={onUnauthorized}
                      />
                    ))}
                  </div>
                </section>
              ))}
          </section>
        ))}
      {creating && (
        <MetricCreate
          onClose={() => setCreating(false)}
          onDone={refresh}
          onUnauthorized={onUnauthorized}
        />
      )}
    </section>
  );
}
