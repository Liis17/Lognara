import type { SeriesLine } from "../api";

export interface ChartRange { from: number; to: number }

const colors = ["--lvl-info", "--lvl-warn", "--lvl-error", "--lvl-fatal", "--lvl-debug", "--lvl-trace", "--lvl-unknown", "--ok"];

export function chartColorToken(index: number): string {
  return colors[index] ?? "--ink3";
}

export function chartData(buckets: string[], series: SeriesLine[]): [number[], ...(number | null)[][]] {
  return [buckets.map((bucket) => Date.parse(bucket) / 1000), ...series.map((line) => line.values)];
}

export function selectedChartRange(from: number, to: number): ChartRange | null {
  if (!Number.isFinite(from) || !Number.isFinite(to) || from === to) return null;
  return { from: Math.round(Math.min(from, to) * 1000), to: Math.round(Math.max(from, to) * 1000) };
}

export function zoomChartRange(
  current: ChartRange,
  bounds: ChartRange,
  fraction: number,
  direction: "in" | "out",
): ChartRange | null {
  const span = current.to - current.from;
  const fullSpan = bounds.to - bounds.from;
  if (!(span > 0) || !(fullSpan > 0)) return null;
  const nextSpan = Math.min(fullSpan, Math.max(fullSpan / 8, span * (direction === "in" ? 0.5 : 2)));
  if (Math.abs(nextSpan - span) < Number.EPSILON) return null;
  const at = current.from + Math.min(1, Math.max(0, fraction)) * span;
  let from = at - Math.min(1, Math.max(0, fraction)) * nextSpan;
  let to = from + nextSpan;
  if (from < bounds.from) { to += bounds.from - from; from = bounds.from; }
  if (to > bounds.to) { from -= to - bounds.to; to = bounds.to; }
  return { from: Math.max(bounds.from, from), to: Math.min(bounds.to, to) };
}
