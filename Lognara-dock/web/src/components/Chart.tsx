import { useEffect, useRef } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import type { SeriesLine } from "../api";

const HEIGHT = 320;

/**
 * Восемь категориальных цветов серий по порядку.
 * Серии сверх них получают нейтральный цвет, новые оттенки без проверки не добавляем.
 */
const palette = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#4a3aa7", "#e34948"];
const overflow = "#898781";

interface Props {
  compact?: boolean;
  /** Моменты точек в формате RFC3339. */
  buckets: string[];
  series: SeriesLine[];
  unit: string;
}

export default function Chart({ buckets, series, unit, compact = false }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const container = ref.current!;
    const height = compact ? 76 : HEIGHT;
    const theme = getComputedStyle(container);
    const ink = theme.getPropertyValue("--ink3").trim();
    const line = theme.getPropertyValue("--line").trim();
    const axis = {
      font: "12px Arial",
      stroke: ink,
      grid: { stroke: line },
      ticks: { stroke: line },
    };
    const data: uPlot.AlignedData = [
      buckets.map((bucket) => Date.parse(bucket) / 1000),
      ...series.map((line) => line.values),
    ];
    const plot = new uPlot(
      {
        width: container.clientWidth,
        height,
        series: [
          {},
          ...series.map((line, i) => ({
            label: line.label || "всего",
            stroke: palette[i] ?? overflow,
            width: 2,
          })),
        ],
        cursor: { show: !compact },
        select: { show: !compact, left: 0, top: 0, width: 0, height: 0 },
        legend: { show: !compact },
        axes: compact
          ? [{ show: false }, { show: false }]
          : [
              axis,
              {
                ...axis,
                size: (self, values) => {
                  self.ctx.save();
                  self.ctx.font = "12px Arial";
                  const width = Math.max(
                    0,
                    ...(values ?? []).map(
                      (value) => self.ctx.measureText(String(value)).width,
                    ),
                  );
                  self.ctx.restore();
                  return Math.ceil(width + 18);
                },
              },
            ],
      },
      data,
      container,
    );
    const observer = new ResizeObserver(() => plot.setSize({ width: container.clientWidth, height }));
    observer.observe(container);
    return () => {
      observer.disconnect();
      plot.destroy();
    };
  }, [buckets, series, unit, compact]);

  return (
    <div className="metric-chart">
      {!compact && unit && <div className="metric-chart-unit">{unit}</div>}
      <div ref={ref} />
    </div>
  );
}
