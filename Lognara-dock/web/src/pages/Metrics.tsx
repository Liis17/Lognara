import { useEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router";
import {
  api,
  ApiError,
  errorText,
  type SeriesMetric,
  type SeriesQuery,
  type SeriesReply,
  type MetricAlertSummary,
} from "../api";
import Chart from "../components/Chart";
import MetricCreate from "./MetricCreate";

let alertSummaryRequest: Promise<MetricAlertSummary> | null = null;
function loadAlertSummary() {
  if (!alertSummaryRequest) {
    alertSummaryRequest = api<MetricAlertSummary>("GET", "/api/alerts/summary").finally(
      () => {
        alertSummaryRequest = null;
      },
    );
  }
  return alertSummaryRequest;
}

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
const MINI_PERIODS = {
  "1h": { label: "1 час", milliseconds: 60 * 60 * 1000, stepSeconds: 60 },
  "24h": { label: "24 часа", milliseconds: 24 * 60 * 60 * 1000, stepSeconds: 1800 },
  "7d": { label: "7 дней", milliseconds: 7 * 24 * 60 * 60 * 1000, stepSeconds: 3600 },
} as const;
type MiniPeriod = keyof typeof MINI_PERIODS;
type MetricSource = { server: string; service: string };
const metricNumber = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 3 });

function groupMetrics(items: ListedMetric[]) {
  const servers = new Map<string, Map<string, ListedMetric[]>>();
  for (const item of items) {
    const services = servers.get(item.server) ?? new Map<string, ListedMetric[]>();
    const metrics = services.get(item.service) ?? [];
    metrics.push(item);
    services.set(item.service, metrics);
    servers.set(item.server, services);
  }
  return [...servers.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([server, services]) => ({
    server,
    services: [...services.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([service, metrics]) => ({ service, metrics })),
  }));
}

function groupSources(sources: MetricSource[]) {
  const servers = new Map<string, Set<string>>();
  for (const { server, service } of sources) {
    const services = servers.get(server) ?? new Set<string>();
    services.add(service);
    servers.set(server, services);
  }
  return [...servers.entries()].sort(([a], [b]) => a.localeCompare(b)).map(([server, services]) => ({
    server,
    services: [...services].sort(),
  }));
}

function Tile({
  metric,
  loader,
  to,
  period,
  onUnauthorized,
  favorite,
  busy,
  alert,
  alertsEnabled,
  onFavorite,
  showSource = false,
}: {
  metric: ListedMetric;
  favorite: boolean;
  busy: boolean;
  alert: MetricAlertSummary["alerts"][number] | undefined;
  alertsEnabled: boolean;
  onFavorite: (metric: ListedMetric) => void;
  showSource?: boolean;
  loader: ReturnType<typeof chartLoader>;
  to: number;
  period: MiniPeriod;
  onUnauthorized: () => void;
}) {
  const ref = useRef<HTMLElement>(null);
  const [visible, setVisible] = useState(false);
  const [chart, setChart] = useState<SeriesReply | null>(null);
  const [error, setError] = useState("");
  const disabled = metric.definition?.enabled === false || metric.missing;
  const miniPeriod = MINI_PERIODS[period];
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
          from: new Date(to - miniPeriod.milliseconds).toISOString(),
          to: new Date(to).toISOString(),
          step_seconds: miniPeriod.stepSeconds,
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
  }, [visible, disabled, metric, loader, to, miniPeriod]);
  const hasValues = chart?.series.some((line) =>
    line.values.some((value) => value !== null),
  );
  const latestValue = chart?.series[0]?.values.slice().reverse().find((value) => value !== null && Number.isFinite(value));
  return (
    <article
      ref={ref}
      className={"metric-tile" + (disabled ? " disabled" : "") + (alert?.enabled ? " has-alert" : "")}
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
      {alert?.enabled && (
        <Link
          to={metricUrl(metric)}
          className={"metric-alert-indicator" + (!alertsEnabled ? " paused" : "")}
          aria-label={
            !alertsEnabled
              ? "Алерт включён в настройке, но глобально приостановлен"
              : alert.no_recipients
                ? "Алерт включён, но нет получателей"
                : "Настроить алерт метрики"
          }
          title={
            !alertsEnabled
              ? "Алерт приостановлен: глобальная отправка выключена"
              : alert.no_recipients
                ? "Алерт включён, но для него нет получателей"
                : "Алерт метрики включён"
          }
        >
          <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false">
            <path d="M2 12s3.6-6 10-6 10 6 10 6-3.6 6-10 6S2 12 2 12Z" />
            <circle cx="12" cy="12" r="3" />
          </svg>
        </Link>
      )}
      <Link to={metricUrl(metric)} className="metric-tile-link">
        <div className="metric-tile-heading">
          <strong>{metric.definition?.title || metric.name}</strong>
          <span className="tag">{originText[metric.origin]}</span>
        </div>
        {showSource && <span className="metric-tile-source">{metric.server} · {metric.service}</span>}
        <span className="muted">
          {metric.name}
          {metric.definition?.unit ? " · " + metric.definition.unit : ""}
        </span>
        <div className="metric-tile-value" aria-label="Последнее значение">
          <span>Последнее</span><strong>{latestValue == null ? "—" : metricNumber.format(latestValue)}</strong>
          <span>{chart?.unit || metric.definition?.unit || ""}</span>
        </div>
        <div className="metric-sparkline" aria-label={`Миниграфик за ${miniPeriod.label}`}>
          {disabled ? (
            <span className="muted">
              {metric.missing ? "Метрика недоступна" : "Отключена"}
            </span>
          ) : error ? (
            <span className="error">{error}</span>
          ) : !chart ? (
            <span className="muted">
              {visible ? "Загрузка…" : miniPeriod.label}
            </span>
          ) : hasValues ? (
            <Chart
              compact
              buckets={chart.buckets}
              series={chart.series}
              unit={chart.unit}
            />
          ) : (
            <span className="muted">За выбранный период данных нет</span>
          )}
        </div>
      </Link>
    </article>
  );
}

type TileContext = {
  loader: ReturnType<typeof chartLoader>;
  to: number;
  period: MiniPeriod;
  onUnauthorized: () => void;
  favorites: Set<string>;
  busy: Set<string>;
  alerts: Map<string, MetricAlertSummary["alerts"][number]>;
  alertsEnabled: boolean;
  onFavorite: (metric: ListedMetric) => void;
};
function Grid({
  items,
  context,
  showSource = false,
}: {
  items: ListedMetric[];
  context: TileContext;
  showSource?: boolean;
}) {
  return (
    <div className="metric-grid">
      {items.map((metric) => (
        <Tile
          key={metricKey(metric)}
          metric={metric}
          loader={context.loader}
          to={context.to}
          period={context.period}
          onUnauthorized={context.onUnauthorized}
          favorite={context.favorites.has(metricKey(metric))}
          busy={context.busy.has(metricKey(metric))}
          alert={context.alerts.get(metricKey(metric))}
          alertsEnabled={context.alertsEnabled}
          onFavorite={context.onFavorite}
          showSource={showSource}
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
  return <details className="metric-service-group" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>{props.service}</summary>
    {open && <div className="metric-service-content"><ServiceCatalog {...props} /></div>}
  </details>;
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
  return <details className="metric-server-group" onToggle={(event) => setOpen(event.currentTarget.open)}>
    <summary>{server || "Сервер без имени"}<span className="muted"> · {services.length} сервисов</span></summary>
    {open && <div className="metric-server-services">{services.map((service) => <ServiceSection
      key={service} server={server} service={service} generation={generation} context={context}
    />)}</div>}
  </details>;
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
  const [sources, setSources] = useState<MetricSource[]>([]);
  const [sourcesReady, setSourcesReady] = useState(false);
  const [search, setSearch] = useState("");
  const [searchCatalog, setSearchCatalog] = useState<SeriesMetric[] | null>(null);
  const [searchReady, setSearchReady] = useState(false);
  const [serverFilter, setServerFilter] = useState("");
  const [period, setPeriod] = useState<MiniPeriod>("24h");
  const [busy, setBusy] = useState(new Set<string>());
  const [alertSummary, setAlertSummary] = useState<MetricAlertSummary>({
    enabled: false,
    alerts: [],
  });
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
  useEffect(() => {
    const controller = new AbortController();
    setSourcesReady(false);
    api<{ sources: MetricSource[] }>("GET", "/api/series/sources", undefined, { signal: controller.signal })
      .then((reply) => {
        if (!controller.signal.aborted) {
          setSources(reply.sources);
          setSourcesReady(true);
        }
      })
      .catch((e) => {
        if (!controller.signal.aborted) {
          if (e instanceof ApiError && e.status === 401) onUnauthorized();
          else setError(errorText(e));
        }
      });
    return () => controller.abort();
  }, [generation, onUnauthorized]);
  useEffect(() => {
    const needle = search.trim();
    if (!needle) {
      setSearchCatalog(null);
      setSearchReady(true);
      return;
    }
    const controller = new AbortController();
    setSearchReady(false);
    const timeout = window.setTimeout(() => {
      const query = new URLSearchParams({ group_by: "server" });
      if (serverFilter) query.set("server", serverFilter);
      api<{ metrics: SeriesMetric[] }>("GET", `/api/series?${query}`, undefined, { signal: controller.signal })
        .then((reply) => {
          if (!controller.signal.aborted) {
            setSearchCatalog(reply.metrics);
            setSearchReady(true);
          }
        })
        .catch((e) => {
          if (!controller.signal.aborted) {
            if (e instanceof ApiError && e.status === 401) onUnauthorized();
            else setError(errorText(e));
            setSearchReady(true);
          }
        });
    }, 180);
    return () => {
      window.clearTimeout(timeout);
      controller.abort();
    };
  }, [search, serverFilter, generation, onUnauthorized]);
  useEffect(() => {
    let active = true;
    loadAlertSummary()
      .then((summary) => {
        if (active) setAlertSummary(summary);
      })
      .catch((cause) => {
        if (!active) return;
        if (cause instanceof ApiError && cause.status === 401) onUnauthorized();
        else setError(errorText(cause));
      });
    return () => {
      active = false;
    };
  }, [generation, onUnauthorized]);
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
    period,
    onUnauthorized,
    favorites,
    busy,
    // Плитка показывает только глобальное правило: скоупленные (через MCP) доступны лишь для чтения в настройках.
    alerts: new Map(
      alertSummary.alerts
        .filter((alert) => alert.node_scope.kind === "all")
        .map((alert) => [metricKey(alert.key), alert]),
    ),
    alertsEnabled: alertSummary.enabled,
    onFavorite,
  };
  const needle = search.trim().toLocaleLowerCase();
  const matches = (metric: SeriesMetric) => !needle || [metric.name, metric.definition?.title,
    metric.definition?.description, metric.service, metric.server]
    .some((value) => value?.toLocaleLowerCase().includes(needle));
  const visibleFavorites = items.filter((metric) => (!serverFilter || metric.server === serverFilter) && matches(metric));
  const visibleSources = sources.filter((source) => !serverFilter || source.server === serverFilter);
  const sourceGroups = groupSources(visibleSources);
  const serverOptions = [...new Set([...sources.map((source) => source.server), ...items.map((item) => item.server)])].sort();
  const searchMatches = (searchCatalog ?? []).filter((metric) => (!serverFilter || metric.server === serverFilter) && matches(metric));
  const searchResults = searchMatches.filter((metric) => !favorites.has(metricKey(metric)));
  return (
    <section className="metrics metrics-overview">
      <div className="toolbar metrics-toolbar">
        <h1>Метрики</h1>
        <button onClick={() => setCreating(true)}>Новая метрика</button>
        <button onClick={refresh} disabled={busy.size > 0}>
          Обновить
        </button>
      </div>
      <div className="metric-filters">
        <label>Поиск метрики, сервиса или сервера<input type="search" aria-label="Поиск метрики, сервиса или сервера"
          value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Например, requests или billing" /></label>
        <label>Сервер<select aria-label="Фильтр по серверу" value={serverFilter} onChange={(event) => setServerFilter(event.target.value)}>
          <option value="">Все серверы</option>
          {serverOptions.map((server) => <option key={server} value={server}>{server || "Сервер без имени"}</option>)}
        </select></label>
        <label>Период миниграфиков<select aria-label="Период миниграфиков" value={period} onChange={(event) => setPeriod(event.target.value as MiniPeriod)}>
          {Object.entries(MINI_PERIODS).map(([key, value]) => <option key={key} value={key}>{value.label}</option>)}
        </select></label>
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      <section className="metric-favorites" aria-label="Избранные метрики">
        <h2>Избранное</h2>
        {!ready && <p className="muted">Загрузка…</p>}
        {ready && !error && !visibleFavorites.length && (
          <p className="muted">
            {items.length ? "По выбранным условиям избранных метрик нет." : "Избранных метрик пока нет. Отметьте звёздочкой нужную карточку в списке."}
          </p>
        )}
        {visibleFavorites.length > 0 && <Grid items={visibleFavorites} context={context} showSource />}
      </section>
      {needle ? <section className="metric-search-results" aria-label="Результаты поиска">
        <h2>Результаты поиска</h2>
        {!searchReady && <p className="muted" role="status">Ищем метрики…</p>}
        {searchReady && <p className="muted metric-search-summary">Найдено {searchMatches.length} метрик; избранные показаны выше.</p>}
        {searchReady && !searchResults.length && <p className="muted">Других метрик по этому запросу нет.</p>}
        {groupMetrics(searchResults).map(({ server, services }) => <details className="metric-server-group" key={server} open>
          <summary>{server || "Сервер без имени"}<span className="muted"> · {services.reduce((count, service) => count + service.metrics.length, 0)} метрик</span></summary>
          <div className="metric-server-services">{services.map(({ service, metrics }) => <details className="metric-service-group" key={service} open>
            <summary>{service}<span className="muted"> · {metrics.length} метрик</span></summary>
            <div className="metric-service-content"><Grid items={metrics} context={context} /></div>
          </details>)}</div>
        </details>)}
      </section> : <section className="metric-catalog" aria-label="Все метрики">
        <h2>Все метрики</h2>
        {!sourcesReady && <p className="muted" role="status">Загружаем серверы и сервисы…</p>}
        {sourcesReady && !sourceGroups.length && <p className="muted">Источников метрик пока нет.</p>}
        {sourceGroups.map(({ server, services }) => <ServerSection key={server} server={server}
          services={services} generation={generation} context={context} />)}
      </section>}
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
