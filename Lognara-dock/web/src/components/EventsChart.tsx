import { type PointerEvent, type ReactNode, useEffect, useRef, useState } from "react";
import { LOG_LEVELS } from "../logs";
import type { HistogramBucket, LogLevel } from "../types";

// Снизу вверх: фон неизвестных уровней, затем по возрастанию важности.
const STACK: LogLevel[] = ["unknown", "trace", "debug", "info", "warn", "error", "fatal"];
const MAX_ZOOM = 8;
const number = new Intl.NumberFormat("ru-RU");
const clock = new Intl.DateTimeFormat("ru-RU", { hour: "2-digit", minute: "2-digit", hour12: false });
const stamp = new Intl.DateTimeFormat("ru-RU", { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit", hour12: false });

interface Props {
  buckets: HistogramBucket[];
  intervalSeconds: number;
  loading: boolean;
  /** Правая часть заголовка, если у корзин нет уровней. */
  aside?: ReactNode;
  caption?: string;
  /** Текст о текущем ограничении диапазона; без него показывается подсказка. */
  selection?: string;
  onSelect: (from: number, to: number) => void;
  onReset: () => void;
}

interface Drag { x0: number; y0: number; x1: number; y1: number; width: number; height: number; start: number; visible: number }

/** Гистограмма событий: выделение интервалов мышью или пальцем, масштаб колесом, фильтр уровней только для графика. */
export default function EventsChart({ buckets, intervalSeconds, loading, aside, caption, selection, onSelect, onReset }: Props) {
  const plot = useRef<HTMLDivElement>(null);
  const [hidden, setHidden] = useState<ReadonlySet<LogLevel>>(new Set());
  const [zoom, setZoom] = useState(1);
  const [start, setStart] = useState(0);
  const [drag, setDrag] = useState<Drag | null>(null);
  const total = buckets.length;
  const visible = Math.max(1, Math.ceil(total / zoom));
  const first = Math.min(Math.max(start, 0), Math.max(0, total - visible));
  const shown = buckets.slice(first, first + visible);
  const stacked = buckets.some((bucket) => bucket.levels !== undefined);
  const view = useRef({ total, zoom, first });
  view.current = { total, zoom, first };
  const intervalMillis = intervalSeconds * 1000;
  const wide = total > 0 && Date.parse(buckets[total - 1].timestamp) - Date.parse(buckets[0].timestamp) >= 86_400_000;
  const label = (time: number) => (wide ? stamp : clock).format(new Date(time));

  useEffect(() => { setZoom(1); setStart(0); setDrag(null); }, [buckets]);

  // React подписывает wheel пассивно, поэтому preventDefault нужен через нативный обработчик.
  useEffect(() => {
    const element = plot.current;
    if (!element) return;
    const wheel = (event: WheelEvent) => {
      const { total, zoom, first } = view.current;
      if (!event.deltaY || total < 2) return;
      event.preventDefault();
      const next = event.deltaY < 0 ? Math.min(MAX_ZOOM, zoom * 2) : Math.max(1, zoom / 2);
      if (next === zoom) return;
      const rect = element.getBoundingClientRect();
      const fraction = Math.min(Math.max((event.clientX - rect.left) / rect.width, 0), 1);
      const anchor = first + fraction * Math.ceil(total / zoom);
      const nextVisible = Math.ceil(total / next);
      setZoom(next);
      setStart(Math.min(Math.max(Math.round(anchor - fraction * nextVisible), 0), total - nextVisible));
    };
    element.addEventListener("wheel", wheel, { passive: false });
    return () => element.removeEventListener("wheel", wheel);
  }, [total > 0]);

  const amount = (bucket: HistogramBucket, level?: LogLevel) =>
    bucket.levels ? (level ? (hidden.has(level) ? 0 : bucket.levels[level]) :
      STACK.reduce((sum, item) => sum + (hidden.has(item) ? 0 : bucket.levels![item]), 0)) : bucket.count;
  const peak = Math.max(1, ...shown.map((bucket) => amount(bucket)));
  const indexAt = (x: number, state: Drag) =>
    state.start + Math.min(Math.max(Math.floor(x / (state.width / state.visible)), 0), state.visible - 1);
  const dragRange = drag ? [indexAt(Math.min(drag.x0, drag.x1), drag), indexAt(Math.max(drag.x0, drag.x1), drag)] : null;
  const select = (low: number, high: number) =>
    onSelect(Date.parse(buckets[low].timestamp), Date.parse(buckets[high].timestamp) + intervalMillis);

  const point = (event: PointerEvent<HTMLDivElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    return {
      x: Math.min(Math.max(event.clientX - rect.left, 0), rect.width),
      y: Math.min(Math.max(event.clientY - rect.top, 0), rect.height), rect,
    };
  };
  const down = (event: PointerEvent<HTMLDivElement>) => {
    if (!total || event.button !== 0) return;
    event.currentTarget.setPointerCapture(event.pointerId);
    const { x, y, rect } = point(event);
    setDrag({ x0: x, y0: y, x1: x, y1: y, width: rect.width, height: rect.height, start: first, visible });
  };
  const move = (event: PointerEvent<HTMLDivElement>) => {
    if (!drag) return;
    const { x, y } = point(event);
    setDrag({ ...drag, x1: x, y1: y });
  };
  const up = () => {
    if (!drag || !dragRange) return;
    setDrag(null);
    select(dragRange[0], Math.abs(drag.x1 - drag.x0) < 4 ? dragRange[0] : dragRange[1]);
  };
  const toggle = (level: LogLevel) => setHidden((current) => {
    const next = new Set(current);
    if (!next.delete(level)) next.add(level);
    return next;
  });
  const bucketText = (bucket: HistogramBucket) => {
    const time = Date.parse(bucket.timestamp);
    return `${stamp.format(new Date(time))} – ${stamp.format(new Date(time + intervalMillis))}: ${number.format(bucket.count)} событий` +
      (bucket.levels ? `; ${LOG_LEVELS.map((level) => `${level.toUpperCase()} ${number.format(bucket.levels![level])}`).join(", ")}` : "");
  };

  return <section className="panel events-chart histogram" aria-labelledby="events-chart-title" aria-busy={loading}>
    <div className="events-chart-head">
      <h2 id="events-chart-title">События по времени</h2>
      {stacked ? <div className="level-toggles" role="group" aria-label="Уровни на графике">{LOG_LEVELS.map((level) =>
        <button key={level} type="button" className={`level-${level}`} aria-pressed={!hidden.has(level)} onClick={() => toggle(level)}>
          <i aria-hidden="true" />{level.toUpperCase()}
        </button>)}</div> : aside}
    </div>
    {!total ? <p className="events-chart-empty" role="status">{loading ? "Загрузка диаграммы…" : "Нет данных для диаграммы."}</p> : <>
      <div ref={plot} className="events-chart-plot" role="group"
        aria-label="График событий: выделите интервалы мышью, колесо меняет масштаб"
        onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={() => setDrag(null)}>
        {drag && <div className="events-chart-box" aria-hidden="true" style={{
          left: Math.min(drag.x0, drag.x1), top: Math.min(drag.y0, drag.y1),
          width: Math.abs(drag.x1 - drag.x0), height: Math.abs(drag.y1 - drag.y0),
        }} />}
        {shown.map((bucket, offset) => {
          const index = first + offset;
          const dimmed = dragRange && (index < dragRange[0] || index > dragRange[1]);
          return <div key={bucket.timestamp} className="events-chart-column" title={bucketText(bucket)}
            style={{ height: `${amount(bucket) / peak * 100}%`, opacity: dimmed ? .35 : 1 }}>
            {bucket.levels ? STACK.map((level) => {
              const count = amount(bucket, level);
              return count > 0 && <i key={level} className={`level-${level}`} data-level={level} style={{ flexGrow: count }} />;
            }) : <i className="level-info" style={{ flexGrow: 1 }} />}
          </div>;
        })}
      </div>
      <div className="events-chart-axis" aria-hidden="true">
        <span>{label(Date.parse(shown[0].timestamp))}</span>
        <span>{label(Date.parse(shown[Math.floor((shown.length - 1) / 2)].timestamp))}</span>
        <span>{label(Date.parse(shown[shown.length - 1].timestamp) + intervalMillis)}</span>
      </div>
    </>}
    <div className="events-chart-foot">
      <span>{selection ?? "Выделите столбцы мышью, чтобы ограничить логи диапазоном. Колесо над графиком меняет масштаб."}</span>
      <button type="button" onClick={() => { setZoom(1); setStart(0); onReset(); }}>Сбросить выбор и масштаб</button>
    </div>
    {caption && <p className="events-chart-caption">{caption}</p>}
    {total > 0 && <details className="histogram-data">
      <summary>Числа диаграммы</summary>
      <div className="table-scroll"><table>
        <thead><tr><th scope="col">Интервал</th><th scope="col">Событий</th></tr></thead>
        <tbody>{buckets.map((bucket, index) => <tr key={bucket.timestamp}>
          <td><time dateTime={bucket.timestamp}>{stamp.format(new Date(bucket.timestamp))}</time></td>
          <td><button type="button" className="link-button" aria-label={`${bucketText(bucket)}. Открыть логи`}
            onClick={() => select(index, index)}>{number.format(bucket.count)}</button></td>
        </tr>)}</tbody>
      </table></div>
    </details>}
  </section>;
}
