import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { api, ApiError, errorText } from "../api";
import Histogram from "../components/Histogram";
import type { GroupResponse, HistogramResponse, LogLevel, QueryFilters } from "../types";

const periods = [
  { label: "1 час", milliseconds: 60 * 60 * 1000 },
  { label: "24 часа", milliseconds: 24 * 60 * 60 * 1000 },
  { label: "7 дней", milliseconds: 7 * 24 * 60 * 60 * 1000 },
];
const levels: LogLevel[] = ["info", "warn", "error", "fatal", "debug", "trace", "unknown"];
const integer = new Intl.NumberFormat("ru-RU");
const decimal = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 1 });
const date = new Intl.DateTimeFormat("ru-RU", {
  day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit", second: "2-digit",
});

interface Range { from: string; to: string }
interface Data {
  histogram?: HistogramResponse;
  levels?: GroupResponse;
  services?: GroupResponse;
  previousHistogram?: HistogramResponse;
  previousLevels?: GroupResponse;
}

function rangeFor(duration: number): Range {
  const now = Date.now();
  return { from: new Date(now - duration).toISOString(), to: new Date(now).toISOString() };
}

function total(response: HistogramResponse): number {
  return response.buckets.reduce((sum, bucket) => sum + bucket.count, 0);
}

function errorShare(response: GroupResponse): number | null {
  const count = response.groups.reduce((sum, group) => sum + group.count, 0);
  return count === 0 ? null
    : (response.groups.find((group) => group.values.level === "error")?.count ?? 0) / count * 100;
}

function change(current: number, previous: number): string {
  if (previous === 0) return current === 0 ? "Без изменений" : "В прошлом окне событий не было";
  const value = (current - previous) / previous * 100;
  return `${value > 0 ? "+" : value < 0 ? "−" : ""}${decimal.format(Math.abs(value))}% к прошлому окну`;
}

function Snapshot({ value }: { value?: string }) {
  return value ? <span className="snapshot">Снимок: <time dateTime={value} title={value}>{date.format(new Date(value))}</time></span> : null;
}

export default function Analytics({ onUnauthorized }: { onUnauthorized: () => void }) {
  const navigate = useNavigate();
  const [period, setPeriod] = useState(1);
  const [range, setRange] = useState<Range>(() => rangeFor(periods[1].milliseconds));
  const [service, setService] = useState<string | null>(null);
  const [compare, setCompare] = useState(false);
  const [data, setData] = useState<Data>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [serviceOptions, setServiceOptions] = useState<string[]>([]);
  const serviceCache = useRef<{ key: string; values: string[] } | null>(null);
  const unauthorized = useRef(onUnauthorized);
  unauthorized.current = onUnauthorized;
  const duration = Date.parse(range.to) - Date.parse(range.from);
  const intervalSeconds = [60, 300, 900, 1800, 3600, 7200, 14400, 43200, 86400]
    .find((step) => duration / (step * 1000) <= 72) ?? 86400;
  const previousRange: Range = {
    from: new Date(Date.parse(range.from) - duration).toISOString(),
    to: range.from,
  };

  useEffect(() => {
    const controller = new AbortController();
    const filters: QueryFilters = service === null ? {} : { service: [service] };
    const key = `${range.from}/${range.to}`;
    setLoading(true);
    setError("");
    setData({});
    if (serviceCache.current?.key !== key) setServiceOptions([]);

    async function request<T>(path: string, body: unknown): Promise<T> {
      const result = await api<T>("POST", path, body, { signal: controller.signal });
      if (controller.signal.aborted) throw new DOMException("Отменено", "AbortError");
      return result;
    }

    async function load() {
      // Core имеет ограниченный пул аналитики: запросы одной страницы идут последовательно.
      const histogram = await request<HistogramResponse>("/api/stats/histogram", {
        ...range, filters, interval_seconds: intervalSeconds,
      });
      setData({ histogram });
      const levelData = await request<GroupResponse>("/api/stats/group-by", {
        ...range, filters, group_by: ["level"], limit: 7,
      });
      setData((current) => ({ ...current, levels: levelData }));
      const services = await request<GroupResponse>("/api/stats/group-by", {
        ...range, filters: { ...filters, level: ["error"] }, group_by: ["service"], limit: 5,
      });
      setData((current) => ({ ...current, services }));
      if (serviceCache.current?.key !== key) {
        const choices = await request<GroupResponse>("/api/stats/group-by", {
          ...range, group_by: ["service"], limit: 100,
        });
        const values = choices.groups.flatMap((group) => group.values.service === null ? [] : [group.values.service]);
        serviceCache.current = { key, values };
        setServiceOptions(values);
      }
      if (compare) {
        const previous = {
          from: new Date(Date.parse(range.from) - duration).toISOString(), to: range.from,
        };
        const previousHistogram = await request<HistogramResponse>("/api/stats/histogram", {
          ...previous, filters, interval_seconds: intervalSeconds,
        });
        setData((current) => ({ ...current, previousHistogram }));
        const previousLevels = await request<GroupResponse>("/api/stats/group-by", {
          ...previous, filters, group_by: ["level"], limit: 7,
        });
        setData((current) => ({ ...current, previousLevels }));
      }
    }

    load().catch((failure) => {
      if (controller.signal.aborted) return;
      if (failure instanceof ApiError && failure.status === 401) unauthorized.current();
      else setError(errorText(failure));
    }).finally(() => {
      if (!controller.signal.aborted) setLoading(false);
    });
    return () => controller.abort();
  }, [range, service, compare, duration, intervalSeconds]);

  const eventCount = data.histogram ? total(data.histogram) : null;
  const previousCount = data.previousHistogram ? total(data.previousHistogram) : null;
  const share = data.levels ? errorShare(data.levels) : null;
  const previousShare = data.previousLevels ? errorShare(data.previousLevels) : null;
  const levelCount = data.levels?.groups.reduce((sum, group) => sum + group.count, 0) ?? 0;
  const maxServiceCount = Math.max(1, ...(data.services?.groups.map((group) => group.count) ?? []));
  const placeholder = loading ? "…" : "Нет данных";

  function openLogs(from = range.from, to = range.to, selectedLevel?: LogLevel, selectedService = service) {
    const filters = [
      ...(selectedService === null ? [] : [`service:${JSON.stringify(selectedService)}`]),
      ...(selectedLevel ? [`level:${selectedLevel.toUpperCase()}`] : []),
    ];
    navigate(`/logs?${new URLSearchParams({ view: "events", from, to, q: filters.join(" AND ") })}`);
  }

  return (
    <section className="analytics-page page-stack">
      <div className="page-heading"><h1>Аналитика</h1><p className="muted">Объём событий и распределение ошибок</p></div>
      <div className="toolbar analytics-toolbar">
        <div className="segmented" role="group" aria-label="Период">
          {periods.map((item, index) => <button key={item.label} type="button" aria-pressed={period === index}
            onClick={() => { setPeriod(index); setRange(rangeFor(item.milliseconds)); }}>{item.label}</button>)}
        </div>
        <label className="inline-label"><span>Сервис</span><select value={service === null ? "" : JSON.stringify(service)}
          onChange={(event) => setService(event.target.value === "" ? null : JSON.parse(event.target.value) as string)}>
          <option value="">Все сервисы</option>
          {service !== null && !serviceOptions.includes(service) && <option value={JSON.stringify(service)}>{service || "Пустое имя"}</option>}
          {serviceOptions.map((value) => <option key={value} value={JSON.stringify(value)}>{value || "Пустое имя"}</option>)}
        </select></label>
        <button type="button" aria-pressed={compare} onClick={() => setCompare((value) => !value)}>Сравнить с прошлым окном</button>
        <button type="button" onClick={() => setRange(rangeFor(periods[period].milliseconds))}>Обновить</button>
        {loading && <span className="muted" role="status">Обновление…</span>}
      </div>
      <p className="muted range-caption">{date.format(new Date(range.from))} → {date.format(new Date(range.to))}
        {" · "}{Intl.DateTimeFormat().resolvedOptions().timeZone}. Список выбора содержит до 100 самых активных сервисов этого окна.</p>
      {error && <div className="error-banner" role="alert">{error}. Обновите данные или выберите другой период.</div>}

      <section className="analytics-kpis" aria-label="Показатели">
        <div className="metric"><span className="metric-label">Всего событий</span>
          <button type="button" className="metric-value link-button" disabled={eventCount === null} onClick={() => openLogs()}>{eventCount === null ? placeholder : integer.format(eventCount)}</button>
          <span className="metric-note">{previousCount !== null && eventCount !== null ? change(eventCount, previousCount) : "В выбранном окне"}</span>
          <Snapshot value={data.histogram?.as_of} /></div>
        <div className="metric"><span className="metric-label">В среднем событий в минуту</span>
          <span className="metric-value">{eventCount === null ? placeholder : decimal.format(eventCount / (duration / 60000))}</span>
          <span className="metric-note">Среднее за весь выбранный период</span>
          <Snapshot value={data.histogram?.as_of} /></div>
        <div className="metric"><span className="metric-label">Доля ERROR</span>
          <button type="button" className="metric-value link-button" disabled={!data.levels || share === null} onClick={() => openLogs(range.from, range.to, "error")}>
            {data.levels ? share === null ? "Нет событий" : `${decimal.format(share)}%` : placeholder}</button>
          <span className="metric-note">{compare && data.previousLevels
            ? previousShare === null ? "В прошлом окне событий не было" : share === null ? "В этом окне событий нет"
              : `${share > previousShare ? "+" : share < previousShare ? "−" : ""}${decimal.format(Math.abs(share - previousShare))} п.п. к прошлому окну`
            : "Только ERROR; FATAL учитывается отдельно"}</span>
          <Snapshot value={data.levels?.as_of} /></div>
        <div className="metric metric-unavailable"><span className="metric-label">Длительности и паттерны</span>
          <span className="metric-value">Следующий этап</span>
          <span className="metric-note">Статистика длительностей и паттерны появятся отдельным этапом.</span></div>
      </section>

      <section className="panel" aria-labelledby="analytics-volume">
        <div className="panel-head"><h2 id="analytics-volume">Объём событий</h2><span className="muted">Шаг {decimal.format(intervalSeconds / 60)} мин</span></div>
        <div className="legend"><span><i className="legend-count" />Все уровни</span>
          {data.previousHistogram && <span><i className="legend-previous" />Предыдущее окно</span>}</div>
        <Histogram label="Объём событий за выбранный период" buckets={data.histogram?.buckets ?? []}
          loading={loading} intervalSeconds={intervalSeconds}
          comparisonBuckets={data.previousHistogram?.buckets}
          caption="Нажмите на интервал, чтобы открыть его логи. Время указано в вашем часовом поясе; крайние интервалы учитывают только события внутри выбранного диапазона."
          onBucket={(timestamp) => {
            const start = Math.max(Date.parse(timestamp), Date.parse(range.from));
            const end = Math.min(Date.parse(timestamp) + intervalSeconds * 1000, Date.parse(range.to));
            openLogs(new Date(start).toISOString(), new Date(end).toISOString());
          }} />
        <Snapshot value={data.histogram?.as_of} />
        {compare && <p className="muted comparison-note">Прошлое окно: {date.format(new Date(previousRange.from))} → {date.format(new Date(previousRange.to))}.
          {" "}<Snapshot value={data.previousHistogram?.as_of} />
          {data.previousLevels && <> · Уровни прошлого окна: <time dateTime={data.previousLevels.as_of} title={data.previousLevels.as_of}>{date.format(new Date(data.previousLevels.as_of))}</time></>}
        </p>}
      </section>

      <div className="analytics-grid">
        <section className="panel" aria-labelledby="analytics-levels">
          <div className="panel-head"><h2 id="analytics-levels">Уровни</h2><span className="muted">Доля всех событий</span></div>
          <div className="levels-stack" aria-hidden="true">{levels.map((level) => {
            const count = data.levels?.groups.find((group) => group.values.level === level)?.count ?? 0;
            return count > 0 && <i key={level} className={`level-dot level-${level}`} style={{ width: `${count / levelCount * 100}%` }} />;
          })}</div>
          <div className="levels">{levels.map((level) => {
            const count = data.levels?.groups.find((group) => group.values.level === level)?.count ?? 0;
            return <button key={level} type="button" className="level-row" disabled={!data.levels || count === 0}
              onClick={() => openLogs(range.from, range.to, level)}>
              <i className={`level-dot level-${level}`} /><span>{level.toUpperCase()}</span>
              <span className="muted">{data.levels ? integer.format(count) : "…"}</span>
              <b>{data.levels ? `${decimal.format(levelCount === 0 ? 0 : count / levelCount * 100)}%` : "…"}</b>
            </button>;
          })}</div>
          {data.levels && levelCount === 0 && <p className="muted">В этом окне событий нет.</p>}
          <Snapshot value={data.levels?.as_of} />
        </section>

        <section className="panel" aria-labelledby="analytics-services">
          <div className="panel-head"><h2 id="analytics-services">Сервисы по ERROR</h2><span className="muted">До 5 групп</span></div>
          <div className="top-services">{data.services?.groups.map((group) => {
            const name = group.values.service;
            return <button key={name === null ? "null" : `service:${name}`} type="button" className="service-row"
              disabled={name === null} title={name === null ? "События без имени сервиса пока нельзя выбрать отдельно" : `Открыть ERROR сервиса ${name}`}
              onClick={() => openLogs(range.from, range.to, "error", name)}>
              <span className="service-name">{name === null ? "Не указан" : name || "Пустое имя"}</span>
              <span className="service-track" aria-hidden="true"><i style={{ width: `${group.count / maxServiceCount * 100}%` }} /></span>
              <b>{integer.format(group.count)}</b>
            </button>;
          })}</div>
          {!data.services && <p className="muted">{loading ? "Загрузка сервисов…" : "Данные сервисов не загружены."}</p>}
          {data.services?.groups.length === 0 && <p className="muted">В этом окне событий ERROR нет.</p>}
          <p className="muted">Рейтинг по числу событий ERROR. FATAL учитывается в распределении уровней отдельно.</p>
          <Snapshot value={data.services?.as_of} />
        </section>
      </div>
      <p className="muted snapshot-note">Блоки обновляются независимо. Время снимка указано у каждого показателя; значения соседних блоков могут различаться во время поступления событий.</p>
    </section>
  );
}
