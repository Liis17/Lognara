import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { api, ApiError, errorText } from "../api";
import Histogram from "../components/Histogram";
import type { Duration, GroupResponse, LogLevel, QueryFilters, SummaryResponse } from "../types";

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
  summary?: SummaryResponse;
  previous?: SummaryResponse;
}

function rangeFor(duration: number): Range {
  const now = Date.now();
  return { from: new Date(now - duration).toISOString(), to: new Date(now).toISOString() };
}

function errorShare(response: SummaryResponse): number | null {
  return response.total === 0 ? null : response.levels.error / response.total * 100;
}

function DurationNote({ value }: { value?: Duration }) {
  return <span className="metric-note">{value
    ? `Покрытие ${decimal.format(value.coverage * 100)}% · ${integer.format(value.measured)} измерений · приблизительно t-digest`
    : "Атрибут duration_ms · приблизительно t-digest"}</span>;
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
      const summary = await request<SummaryResponse>("/api/stats/summary", {
        ...range, filters, interval_seconds: intervalSeconds,
      });
      setData({ summary });
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
        const previousSummary = await request<SummaryResponse>("/api/stats/summary", {
          ...previous, filters, interval_seconds: intervalSeconds,
        });
        setData((current) => ({ ...current, previous: previousSummary }));
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

  const eventCount = data.summary?.total ?? null;
  const previousCount = data.previous?.total ?? null;
  const share = data.summary ? errorShare(data.summary) : null;
  const previousShare = data.previous ? errorShare(data.previous) : null;
  const levelCount = data.summary?.total ?? 0;
  const maxServiceCount = Math.max(1, ...(data.summary?.error_services.map((group) => group.count) ?? []));
  const placeholder = loading ? "…" : "Нет данных";

  function openLogs(from = range.from, to = range.to, selectedLevel?: LogLevel, selectedService = service, action?: string) {
    const filters = [
      ...(selectedService === null ? [] : [`service:${JSON.stringify(selectedService)}`]),
      ...(action === undefined ? [] : [`action:${JSON.stringify(action)}`]),
      ...(selectedLevel ? [`level:${selectedLevel.toUpperCase()}`] : []),
    ];
    navigate(`/logs?${new URLSearchParams({ view: "events", from, to, q: filters.join(" AND ") })}`);
  }

  return (
    <section className="analytics-page page-stack">
      <div className="page-heading"><h1>Аналитика</h1><p className="muted">Объём событий, ошибки и длительности</p></div>
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
        </div>
        <div className="metric"><span className="metric-label">В среднем событий в минуту</span>
          <span className="metric-value">{eventCount === null ? placeholder : decimal.format(eventCount / (duration / 60000))}</span>
          <span className="metric-note">Среднее за весь выбранный период</span>
        </div>
        <div className="metric"><span className="metric-label">Доля ERROR</span>
          <button type="button" className="metric-value link-button" disabled={!data.summary || share === null} onClick={() => openLogs(range.from, range.to, "error")}>
            {data.summary ? share === null ? "Нет событий" : `${decimal.format(share)}%` : placeholder}</button>
          <span className="metric-note">{compare && data.previous
            ? previousShare === null ? "В прошлом окне событий не было" : share === null ? "В этом окне событий нет"
              : `${share > previousShare ? "+" : share < previousShare ? "−" : ""}${decimal.format(Math.abs(share - previousShare))} п.п. к прошлому окну`
            : "Только ERROR; FATAL учитывается отдельно"}</span>
        </div>
        <div className="metric"><span className="metric-label">p95 duration_ms</span>
          <span className="metric-value">{data.summary ? data.summary.duration.p95_ms === null ? "Нет измерений" : `${decimal.format(data.summary.duration.p95_ms)} мс` : placeholder}</span>
          <DurationNote value={data.summary?.duration} /></div>
      </section>

      <Snapshot value={data.summary?.as_of} />
      <section className="panel" aria-labelledby="analytics-volume">
        <div className="panel-head"><h2 id="analytics-volume">Объём событий</h2><span className="muted">Шаг {decimal.format(intervalSeconds / 60)} мин</span></div>
        {data.previous && <div className="legend"><span><i className="legend-previous" />Предыдущее окно</span></div>}
        <Histogram label="Объём событий за выбранный период" buckets={data.summary?.buckets ?? []}
          loading={loading} intervalSeconds={intervalSeconds}
          comparisonBuckets={data.previous?.buckets}
          caption="Нажмите на интервал, чтобы открыть его логи. Время указано в вашем часовом поясе; крайние интервалы учитывают только события внутри выбранного диапазона."
          onBucket={(timestamp) => {
            const start = Math.max(Date.parse(timestamp), Date.parse(range.from));
            const end = Math.min(Date.parse(timestamp) + intervalSeconds * 1000, Date.parse(range.to));
            openLogs(new Date(start).toISOString(), new Date(end).toISOString());
          }} />
        {compare && <p className="muted comparison-note">Прошлое окно: {date.format(new Date(previousRange.from))} → {date.format(new Date(previousRange.to))}.
          {" "}<Snapshot value={data.previous?.as_of} />
        </p>}
      </section>

      <div className="analytics-grid">
        <section className="panel" aria-labelledby="analytics-levels">
          <div className="panel-head"><h2 id="analytics-levels">Уровни</h2><span className="muted">Доля всех событий</span></div>
          <div className="levels-stack" aria-hidden="true">{levels.map((level) => {
            const count = data.summary?.levels[level] ?? 0;
            return count > 0 && <i key={level} className={`level-dot level-${level}`} style={{ width: `${count / levelCount * 100}%` }} />;
          })}</div>
          <div className="levels">{levels.map((level) => {
            const count = data.summary?.levels[level] ?? 0;
            return <button key={level} type="button" className="level-row" disabled={!data.summary || count === 0}
              onClick={() => openLogs(range.from, range.to, level)}>
              <i className={`level-dot level-${level}`} /><span>{level.toUpperCase()}</span>
              <span className="muted">{data.summary ? integer.format(count) : "…"}</span>
              <b>{data.summary ? `${decimal.format(levelCount === 0 ? 0 : count / levelCount * 100)}%` : "…"}</b>
            </button>;
          })}</div>
          {data.summary && levelCount === 0 && <p className="muted">В этом окне событий нет.</p>}
        </section>

        <section className="panel" aria-labelledby="analytics-services">
          <div className="panel-head"><h2 id="analytics-services">Сервисы по ERROR</h2><span className="muted">До 5 групп</span></div>
          <div className="top-services">{data.summary?.error_services.map((group) => {
            const name = group.service;
            return <button key={name === null ? "null" : `service:${name}`} type="button" className="service-row"
              disabled={name === null} title={name === null ? "События без имени сервиса пока нельзя выбрать отдельно" : `Открыть ERROR сервиса ${name}`}
              onClick={() => openLogs(range.from, range.to, "error", name)}>
              <span className="service-name">{name === null ? "Не указан" : name || "Пустое имя"}</span>
              <span className="service-track" aria-hidden="true"><i style={{ width: `${group.count / maxServiceCount * 100}%` }} /></span>
              <b>{integer.format(group.count)}</b>
            </button>;
          })}</div>
          {!data.summary && <p className="muted">{loading ? "Загрузка сервисов…" : "Данные сервисов не загружены."}</p>}
          {data.summary?.error_services.length === 0 && <p className="muted">В этом окне событий ERROR нет.</p>}
          <p className="muted">Рейтинг по числу событий ERROR. FATAL учитывается в распределении уровней отдельно.</p>
        </section>
      </div>
      <section className="panel" aria-labelledby="analytics-actions">
        <div className="panel-head"><h2 id="analytics-actions">Действия</h2><span className="muted">До 20 групп по числу событий</span></div>
        <div className="table-scroll"><table className="actions-table">
          <thead><tr><th scope="col">action</th><th scope="col">Событий</th><th scope="col">p95 duration_ms</th><th scope="col">Измерений / покрытие</th></tr></thead>
          <tbody>{data.summary?.actions.map((group) => <tr key={group.action === null ? "null" : `action:${group.action}`}>
            <td>{group.action === null ? "Не указано" : <button type="button" className="link-button" onClick={() => openLogs(range.from, range.to, undefined, service, group.action!)}>{group.action || "Пустое значение"}</button>}</td>
            <td>{integer.format(group.count)}</td>
            <td>{group.duration.p95_ms === null ? "Нет измерений" : `${decimal.format(group.duration.p95_ms)} мс`}</td>
            <td>{integer.format(group.duration.measured)} / {decimal.format(group.duration.coverage * 100)}%</td>
          </tr>)}</tbody>
        </table></div>
        {data.summary?.actions.length === 0 && <p className="muted">В этом окне событий нет.</p>}
        <p className="muted">Число событий с каждым action, включая повторы; это не число вызовов. p95 приблизительный, метод t-digest; покрытие по числовому атрибуту duration_ms.</p>
      </section>
      <p className="muted snapshot-note">Показатели, график и таблицы используют один снимок сводки. Прошлое окно и каталог сервисов запрашиваются отдельно. Паттерны появятся следующим этапом.</p>
    </section>
  );
}
