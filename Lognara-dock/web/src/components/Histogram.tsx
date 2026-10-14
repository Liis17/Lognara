import { useId } from "react";
import { LOG_LEVELS } from "../logs";
import type { HistogramBucket } from "../types";

export interface HistogramProps {
  buckets: HistogramBucket[];
  label: string;
  caption?: string;
  loading?: boolean;
  intervalSeconds?: number;
  onBucket?: (timestamp: string) => void;
  comparisonBuckets?: HistogramBucket[];
  comparisonLabel?: string;
}

const number = new Intl.NumberFormat("ru-RU");
const time = new Intl.DateTimeFormat("ru-RU", {
  day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit",
});

function bucketLabel(bucket: HistogramBucket, intervalSeconds?: number): string {
  const start = time.format(new Date(bucket.timestamp));
  const end = intervalSeconds
    ? ` – ${time.format(new Date(Date.parse(bucket.timestamp) + intervalSeconds * 1000))}`
    : "";
  return `${start}${end}: ${number.format(bucket.count)} событий${bucket.levels ? `; ${LOG_LEVELS.map((level) => `${level.toUpperCase()} ${number.format(bucket.levels![level])}`).join(", ")}` : ""}`;
}

/** Общая count-диаграмма. Сравнение выравнивается по позиции в окне. */
export default function Histogram({
  buckets, label, caption, loading = false, intervalSeconds, onBucket,
  comparisonBuckets, comparisonLabel = "Предыдущее окно",
}: HistogramProps) {
  const titleId = useId();
  if (loading && buckets.length === 0) {
    return <div className="histogram histogram-empty" role="status">Загрузка диаграммы…</div>;
  }
  if (buckets.length === 0) {
    return <div className="histogram histogram-empty">Нет данных для диаграммы.</div>;
  }

  const width = 900;
  const height = 180;
  const maximum = Math.max(0, ...buckets.map((bucket) => bucket.count),
    ...(comparisonBuckets ?? []).map((bucket) => bucket.count));
  const peak = Math.max(1, maximum);
  const step = width / buckets.length;
  const gap = Math.min(4, step * 0.2);
  const comparison = comparisonBuckets?.length === buckets.length ? comparisonBuckets : undefined;
  const points = comparison?.map((bucket, index) =>
    `${(index + 0.5) * step},${height - bucket.count / peak * (height - 8)}`,
  ).join(" ");

  const stacked = buckets.some((bucket) => bucket.levels !== undefined);
  return (
    <div className="histogram" aria-busy={loading}>
      {stacked && <div className="legend histogram-legend" aria-label="Уровни диаграммы">{LOG_LEVELS.map((level) =>
        <span key={level}><i className={`level-dot level-${level}`} />{level.toUpperCase()}</span>)}</div>}
      <svg viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" aria-labelledby={titleId} role="group">
        <title id={titleId}>{label}. Максимум: {number.format(maximum)} событий за интервал.</title>
        {[0, 0.25, 0.5, 0.75, 1].map((fraction) => (
          <line key={fraction} className="histogram-grid" x1="0" x2={width}
            y1={height * fraction} y2={height * fraction} />
        ))}
        {buckets.map((bucket, index) => {
          const barHeight = bucket.count / peak * (height - 8);
          const description = bucketLabel(bucket, intervalSeconds);
          const previous = comparison?.[index];
          return (
            <g key={bucket.timestamp} className={onBucket ? "histogram-bucket interactive" : "histogram-bucket"}
              role={onBucket ? "button" : "img"} tabIndex={onBucket ? 0 : undefined}
              aria-label={`${description}${onBucket ? ". Открыть логи этого интервала" : ""}`}
              onClick={onBucket ? () => onBucket(bucket.timestamp) : undefined}
              onKeyDown={onBucket ? (event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  onBucket(bucket.timestamp);
                }
              } : undefined}>
              <title>{description}{previous ? `\n${comparisonLabel}: ${bucketLabel(previous, intervalSeconds)}` : ""}</title>
              <rect className="histogram-hit" x={index * step} y="0" width={step} height={height} fill="transparent" />
              {bucket.levels ? LOG_LEVELS.map((level, levelIndex) => {
                const count = bucket.levels![level];
                const before = LOG_LEVELS.slice(0, levelIndex).reduce((sum, item) => sum + bucket.levels![item], 0);
                const segmentHeight = count / peak * (height - 8);
                return <rect key={level} className={`histogram-bar level-${level}`} data-level={level}
                  x={index * step + gap / 2} y={height - (before + count) / peak * (height - 8)}
                  width={step - gap} height={segmentHeight} />;
              }) : <rect className="histogram-bar" x={index * step + gap / 2} y={height - barHeight}
                width={step - gap} height={barHeight} rx={Math.min(2, step / 4)} />}

            </g>
          );
        })}
        {points && <polyline className="histogram-comparison" points={points} fill="none"
          strokeWidth="2" strokeDasharray="6 5" vectorEffect="non-scaling-stroke" pointerEvents="none" aria-hidden="true" />}
      </svg>
      <div className="histogram-scale" aria-hidden="true"><span>0</span><span>Пик: {number.format(maximum)}</span></div>
      <div className="histogram-axis" aria-hidden="true">
        <span>{time.format(new Date(buckets[0].timestamp))}</span>
        {buckets.length > 2 && <span>{time.format(new Date(buckets[Math.floor(buckets.length / 2)].timestamp))}</span>}
        <span>{time.format(new Date(buckets[buckets.length - 1].timestamp))}</span>
      </div>
      {caption && <p className="muted histogram-caption">{caption}</p>}
      <details className="histogram-data">
        <summary>Числа диаграммы</summary>
        <div className="table-scroll">
          <table>
            <caption>{label}</caption>
            <thead><tr><th scope="col">Начало интервала</th><th scope="col">Событий</th>
              {stacked && LOG_LEVELS.map((level) => <th key={level} scope="col">{level.toUpperCase()}</th>)}
              {comparison && <th scope="col">{comparisonLabel}</th>}
            </tr></thead>
            <tbody>{buckets.map((bucket, index) => (
              <tr key={bucket.timestamp}>
                <td><time dateTime={bucket.timestamp} title={bucket.timestamp}>{time.format(new Date(bucket.timestamp))}</time></td>
                <td>{onBucket
                  ? <button type="button" className="link-button" onClick={() => onBucket(bucket.timestamp)}
                    aria-label={`${bucketLabel(bucket, intervalSeconds)}. Открыть логи`}>{number.format(bucket.count)}</button>
                  : number.format(bucket.count)}</td>
                {stacked && LOG_LEVELS.map((level) => <td key={level}>{number.format(bucket.levels?.[level] ?? 0)}</td>)}
                {comparison && <td><time dateTime={comparison[index].timestamp} title={comparison[index].timestamp}>{time.format(new Date(comparison[index].timestamp))}</time>{" · "}{number.format(comparison[index].count)}</td>}
              </tr>
            ))}</tbody>
          </table>
        </div>
      </details>
    </div>
  );
}
