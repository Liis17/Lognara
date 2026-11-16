import { useMemo, type ReactNode } from "react";
import type { SeriesLine } from "../api";
import type { HistogramBucket, LogLevel } from "../types";
import LineChart from "./LineChart";

const LEVEL_SERIES: LogLevel[] = ["info", "warn", "error", "fatal", "debug", "trace", "unknown"];

interface Props {
  visibleLevels: ReadonlySet<LogLevel>;
  onToggleLevel: (level: LogLevel) => void;
  buckets: HistogramBucket[];
  intervalSeconds: number;
  loading: boolean;
  showLevels: boolean;
  /** Правая часть заголовка, если нет разбивки по уровням. */
  aside?: ReactNode;
  caption?: string;
  selection?: string;
  onSelect: (from: number, to: number) => void;
  onReset: () => void;
}

/** Линейный график логов с общей временной сеткой и выбором интервала. */
export default function EventsChart({ visibleLevels, onToggleLevel, buckets, intervalSeconds, loading, showLevels,
  aside, caption, selection, onSelect, onReset }: Props) {
  const levels = showLevels;
  const series = useMemo<SeriesLine[]>(() => levels
    ? LEVEL_SERIES.map((level) => ({
      label: level.toUpperCase(),
      values: buckets.map((bucket) => bucket.levels?.[level] ?? 0),
    }))
    : [{ label: "События", values: buckets.map((bucket) => bucket.count) }],
  [buckets, levels]);
  const visibleSeries = useMemo(() => levels
    ? new Set(LEVEL_SERIES.flatMap((level, index) => visibleLevels.has(level) ? [index] : []))
    : new Set([0]), [levels, visibleLevels]);
  const intervalMillis = intervalSeconds * 1000;

  const selectBucket = (index: number) => {
    const from = Date.parse(buckets[index].timestamp);
    onSelect(from, from + intervalMillis);
  };
  const selectRange = (from: number, to: number) => {
    const start = Date.parse(buckets[0].timestamp);
    const last = buckets.length - 1;
    const bucket = (value: number) => Math.min(last, Math.max(0, Math.floor((value - start) / intervalMillis)));
    const low = bucket(Math.min(from, to));
    const high = bucket(Math.max(from, to));
    onSelect(start + low * intervalMillis, start + (high + 1) * intervalMillis);
  };

  return <section className="panel events-chart" aria-labelledby="events-chart-title" aria-busy={loading}>
    <div className="events-chart-head">
      <h2 id="events-chart-title">События по времени</h2>
      {levels ? <div className="level-toggles" role="group" aria-label="Уровни графика и ленты">
        {LEVEL_SERIES.map((level) => <button key={level} type="button" className={`level-${level}`}
          aria-pressed={visibleLevels.has(level)} onClick={() => onToggleLevel(level)}>
          <i aria-hidden="true" />{level.toUpperCase()}
        </button>)}
      </div> : aside}
    </div>
    {!buckets.length ? <p className="events-chart-empty" role="status">{loading ? "Загружаем диаграмму…" : "Нет данных для диаграммы."}</p> : <div role="group"
      aria-label="График событий: линия с точками, выделите диапазон мышью, колесо меняет масштаб">
      <LineChart buckets={buckets.map((bucket) => bucket.timestamp)} series={series} visibleSeries={visibleSeries}
        onToggleSeries={(index) => onToggleLevel(LEVEL_SERIES[index])} onSelectRange={selectRange} onZoom={() => {}}
        onSelectBucket={selectBucket} showLegend={false} />
    </div>}
    <div className="events-chart-foot">
      <span>{selection ?? "Наведите на точку для значений. Выделите диапазон мышью; колесо меняет масштаб."}</span>
      <button type="button" onClick={onReset}>Сбросить выбор и масштаб</button>
    </div>
    {caption && <p className="events-chart-caption">{caption}</p>}
  </section>;
}
