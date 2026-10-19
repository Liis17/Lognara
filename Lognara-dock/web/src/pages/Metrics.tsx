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
  type Entry = {
    controller: AbortController;
    promise: Promise<SeriesReply>;
    users: number;
    settled: boolean;
    start: () => void;
  };
  const cache = new Map<string, Entry>();
  const queue: Entry[] = [];
  let active = 0;
  const pump = () => {
    while (active < 2 && queue.length) {
      const entry = queue.shift()!;
      if (!entry.controller.signal.aborted) entry.start();
    }
  };
  return {
    close() {
      for (const entry of cache.values()) entry.controller.abort();
      cache.clear();
      queue.length = 0;
    },
    load(query: SeriesQuery, signal: AbortSignal) {
      const key = JSON.stringify(query);
      let entry = cache.get(key);
      if (!entry) {
        const controller = new AbortController();
        let start!: () => void;
        const promise = new Promise<SeriesReply>((resolve, reject) => {
          controller.signal.addEventListener(
            "abort",
            () => reject(new DOMException("Aborted", "AbortError")),
            { once: true },
          );
          start = () => {
            active++;
            api<SeriesReply>("POST", "/api/series/query", query, {
              signal: controller.signal,
            })
              .then(resolve, reject)
              .finally(() => {
                active--;
                pump();
              });
          };
        });
        entry = { controller, promise, users: 0, settled: false, start };
        const current = entry;
        promise.then(
          () => {
            current.settled = true;
          },
          () => {
            current.settled = true;
            if (cache.get(key) === current) cache.delete(key);
          },
        );
        cache.set(key, entry);
        queue.push(entry);
      }
      const current = entry;
      current.users++;
      const release = () => {
        signal.removeEventListener("abort", release);
        if (--current.users === 0 && !current.settled) {
          if (cache.get(key) === current) cache.delete(key);
          current.controller.abort();
          const index = queue.indexOf(current);
          if (index >= 0) queue.splice(index, 1);
        }
      };
      signal.addEventListener("abort", release, { once: true });
      if (signal.aborted) release();
      pump();
      return current.promise;
    },
  };
}

export type MetricKey = Pick<
  SeriesMetric,
  "server" | "service" | "name" | "origin"
>;
export const metricKey = (metric: MetricKey) =>
  JSON.stringify([metric.server, metric.service, metric.name, metric.origin]);
const keyOf = ({ server, service, name, origin }: MetricKey): MetricKey => ({
  server,
  service,
  name,
  origin,
});
type ListedMetric = SeriesMetric & { missing?: boolean };

const originText = { counter: "счётчик", gauge: "показатель", rule: "regex" };

function Tile({
  metric,
  loader,
  to,
  onUnauthorized,
  favorite,
  busy,
  onFavorite,
}: {
  metric: ListedMetric;
  favorite: boolean;
  busy: boolean;
  onFavorite: (metric: ListedMetric) => void;
  loader: ReturnType<typeof chartLoader>;
  to: number;
  onUnauthorized: () => void;
}) {
  const ref = useRef<HTMLElement>(null);
  const [visible, setVisible] = useState(false);
  const [chart, setChart] = useState<SeriesReply | null>(null);
  const [error, setError] = useState("");
  const disabled = metric.definition?.enabled === false || metric.missing;
  useEffect(() => {
    const observer = new IntersectionObserver((entries) => {
      setVisible(entries.some((e) => e.isIntersecting));
    });
    observer.observe(ref.current!);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!visible || disabled) return;
    const controller = new AbortController();
    setChart(null);
    setError("");
    loader
      .load(
        {
          server: metric.server,
          service: metric.service,
          name: metric.name,
          origin: metric.origin,
          from: new Date(to - 86400000).toISOString(),
          to: new Date(to).toISOString(),
          step_seconds: 1800,
          split: "none",
        },
        controller.signal,
      )
      .then((reply) => {
        if (!controller.signal.aborted) setChart(reply);
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          if (e instanceof ApiError && e.status === 401) onUnauthorized();
          else setError(errorText(e));
        }
      });
    return () => {
      controller.abort();
    };
  }, [visible, disabled, metric, loader, to]);
  const hasValues = chart?.series.some((line) =>
    line.values.some((value) => value !== null),
  );
  return (
    <article
      ref={ref}
      className={"metric-tile" + (disabled ? " disabled" : "")}
    >
      <button
        className={"metric-favorite" + (favorite ? " selected" : "")}
        aria-label={favorite ? "Убрать из избранного" : "Добавить в избранное"}
        aria-pressed={favorite}
        disabled={busy}
        onClick={() => onFavorite(metric)}
      >
        {favorite ? "★" : "☆"}
      </button>
      <Link to={metricUrl(metric)} className="metric-tile-link">
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
            <span className="muted">
              {metric.missing ? "Метрика недоступна" : "Отключена"}
            </span>
          ) : error ? (
            <span className="error">{error}</span>
          ) : !chart ? (
            <span className="muted">
              {visible ? "Загрузка…" : "За 24 часа"}
            </span>
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
    </article>
  );
}

type TileContext = {
  loader: ReturnType<typeof chartLoader>;
  to: number;
  onUnauthorized: () => void;
  favorites: Set<string>;
  busy: Set<string>;
  onFavorite: (metric: ListedMetric) => void;
};
function Grid({
  items,
  context,
}: {
  items: ListedMetric[];
  context: TileContext;
}) {
  return (
    <div className="metric-grid">
      {items.map((metric) => (
        <Tile
          key={metricKey(metric)}
          metric={metric}
          loader={context.loader}
          to={context.to}
          onUnauthorized={context.onUnauthorized}
          favorite={context.favorites.has(metricKey(metric))}
          busy={context.busy.has(metricKey(metric))}
          onFavorite={context.onFavorite}
        />
      ))}
    </div>
  );
}
function ServiceCatalog({
  server,
  service,
  generation,
  context,
}: {
  server: string;
  service: string;
  generation: number;
  context: TileContext;
}) {
  const [items, setItems] = useState<SeriesMetric[] | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    setItems(null);
    api<{ metrics: SeriesMetric[] }>(
      "GET",
      "/api/series?" +
        new URLSearchParams({ group_by: "server", server, service }),
      undefined,
      { signal: controller.signal },
    )
      .then((reply) => {
        if (!controller.signal.aborted) setItems(reply.metrics);
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          if (e instanceof ApiError && e.status === 401)
            context.onUnauthorized();
          else setError(errorText(e));
        }
      });
    return () => controller.abort();
  }, [server, service, generation]);
  const others = items?.filter(
    (item) => !context.favorites.has(metricKey(item)),
  );
  return (
    <>
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : !others ? (
        <p className="muted">Загрузка…</p>
      ) : others.length ? (
        <Grid items={others} context={context} />
      ) : (
        <p className="muted">Остальных метрик нет.</p>
      )}
    </>
  );
}
function ServiceSection(props: {
  server: string;
  service: string;
  generation: number;
  context: TileContext;
}) {
  const [open, setOpen] = useState(false);
  return (
    <section className="metric-service">
      <h4>
        <button
          className="metric-disclosure"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        >
          {open ? "▾" : "▸"} {props.service}
        </button>
      </h4>
      {open && <ServiceCatalog {...props} />}
    </section>
  );
}
function ServerSection({
  server,
  services,
  generation,
  context,
}: {
  server: string;
  services: string[];
  generation: number;
  context: TileContext;
}) {
  const [open, setOpen] = useState(false);
  return (
    <section className="metric-server">
      <h3>
        <button
          className="metric-disclosure"
          aria-expanded={open}
          onClick={() => setOpen(!open)}
        >
          {open ? "▾" : "▸"} {server || "Сервер без имени"}
        </button>
      </h3>
      {open &&
        services.map((service) => (
          <ServiceSection
            key={service}
            server={server}
            service={service}
            generation={generation}
            context={context}
          />
        ))}
    </section>
  );
}
function OtherMetrics({
  generation,
  context,
}: {
  generation: number;
  context: TileContext;
}) {
  const [sources, setSources] = useState<{ server: string; service: string }[]>(
    [],
  );
  const [ready, setReady] = useState(false);
  const [error, setError] = useState("");
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    api<{ sources: typeof sources }>("GET", "/api/series/sources", undefined, {
      signal: controller.signal,
    })
      .then((reply) => {
        if (!controller.signal.aborted) {
          setSources(reply.sources);
          setReady(true);
        }
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          if (e instanceof ApiError && e.status === 401)
            context.onUnauthorized();
          else setError(errorText(e));
        }
      });
    return () => controller.abort();
  }, [generation]);
  const servers = [...new Set(sources.map((s) => s.server))].sort();
  return (
    <>
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {!ready && !error && <p className="muted">Загрузка…</p>}
      {ready && !sources.length && (
        <p className="muted">Источников метрик пока нет.</p>
      )}
      {servers.map((server) => (
        <ServerSection
          key={server}
          server={server}
          services={[
            ...new Set(
              sources.filter((s) => s.server === server).map((s) => s.service),
            ),
          ].sort()}
          generation={generation}
          context={context}
        />
      ))}
    </>
  );
}
export default function Metrics({
  onUnauthorized,
}: {
  onUnauthorized: () => void;
}) {
  const [items, setItems] = useState<ListedMetric[]>([]);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState("");
  const [creating, setCreating] = useState(false);
  const [generation, setGeneration] = useState(0);
  const [othersOpen, setOthersOpen] = useState(false);
  const [busy, setBusy] = useState(new Set<string>());
  const pending = useRef(new Set<string>());
  const [to, setTo] = useState(() => Math.floor(Date.now() / 60000) * 60000);
  const loader = useMemo(chartLoader, [generation]);
  useEffect(() => {
    return () => loader.close();
  }, [loader]);
  const fail = (e: unknown) => {
    if (e instanceof ApiError && e.status === 401) onUnauthorized();
    else setError(errorText(e));
  };
  useEffect(() => {
    const controller = new AbortController();
    setError("");
    setReady(false);
    (async () => {
      const { keys } = await api<{ keys: MetricKey[] }>(
        "GET",
        "/api/series/favorites",
        undefined,
        { signal: controller.signal },
      );
      const reply = keys.length
        ? await api<{ metrics: SeriesMetric[] }>(
            "POST",
            "/api/series/lookup",
            { keys },
            { signal: controller.signal },
          )
        : { metrics: [] };
      const found = new Map(reply.metrics.map((m) => [metricKey(m), m]));
      if (!controller.signal.aborted) {
        setItems(
          keys.map(
            (key) =>
              found.get(metricKey(key)) ?? {
                ...key,
                definition: null,
                environments: [],
                first_seen: null,
                last_seen: null,
                missing: true,
              },
          ),
        );
        setReady(true);
      }
    })().catch((e) => {
      if (!controller.signal.aborted) {
        fail(e);
        setReady(true);
      }
    });
    return () => controller.abort();
  }, [generation]);
  const refresh = () => {
    setGeneration((g) => g + 1);
    setTo(Math.floor(Date.now() / 60000) * 60000);
  };
  const favorites = new Set(items.map(metricKey));
  const onFavorite = async (metric: ListedMetric) => {
    const key = metricKey(metric);
    if (pending.current.has(key)) return;
    pending.current.add(key);
    setBusy(new Set(pending.current));
    setError("");
    const removing = favorites.has(key);
    try {
      await api(
        removing ? "DELETE" : "PUT",
        "/api/series/favorites",
        keyOf(metric),
      );
      setItems((previous) =>
        removing
          ? previous.filter((m) => metricKey(m) !== key)
          : [...previous.filter((m) => metricKey(m) !== key), metric],
      );
    } catch (e) {
      fail(e);
    } finally {
      pending.current.delete(key);
      setBusy(new Set(pending.current));
    }
  };
  const context: TileContext = {
    loader,
    to,
    onUnauthorized,
    favorites,
    busy,
    onFavorite,
  };
  const servers = [...new Set(items.map((m) => m.server))].sort();
  return (
    <section className="metrics metrics-overview">
      <div className="toolbar">
        <h1>Метрики</h1>
        <button onClick={() => setCreating(true)}>Новая метрика</button>
        <button onClick={refresh} disabled={busy.size > 0}>
          Обновить
        </button>
        <span className="muted">Миниграфики за 24 часа</span>
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <section aria-label="Избранные метрики">
        <h2>Избранные метрики</h2>
        {!ready && <p className="muted">Загрузка…</p>}
        {ready && !error && !items.length && (
          <p className="muted">
            Избранных метрик пока нет. Раскройте остальные метрики и нажмите
            звёздочку на нужной карточке.
          </p>
        )}
        {servers.map((server) => (
          <section className="metric-server" key={server}>
            <h3>{server || "Сервер без имени"}</h3>
            {[
              ...new Set(
                items.filter((m) => m.server === server).map((m) => m.service),
              ),
            ]
              .sort()
              .map((service) => (
                <section className="metric-service" key={service}>
                  <h4>{service}</h4>
                  <Grid
                    items={items.filter(
                      (m) => m.server === server && m.service === service,
                    )}
                    context={context}
                  />
                </section>
              ))}
          </section>
        ))}
      </section>
      <section aria-label="Остальные метрики">
        <h2>
          <button
            className="metric-disclosure"
            aria-expanded={othersOpen}
            onClick={() => setOthersOpen(!othersOpen)}
          >
            {othersOpen ? "▾" : "▸"} Остальные метрики
          </button>
        </h2>
        {othersOpen && (
          <OtherMetrics generation={generation} context={context} />
        )}
      </section>
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
