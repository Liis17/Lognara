import { useEffect, useRef } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import type { SeriesLine } from "../api";

const HEIGHT = 320;

/**
 * Восемь категориальных цветов по порядку: проверены валидатором на светлом фоне.
 * Серии сверх них получают нейтральный цвет, новые оттенки без проверки не добавляем.
 */
const palette = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#4a3aa7", "#e34948"];
const overflow = "#898781";

interface Props {
  /** Моменты точек в формате RFC3339. */
  buckets: string[];
  series: SeriesLine[];
  unit: string;
}

export default function Chart({ buckets, series, unit }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const container = ref.current!;
    const data: uPlot.AlignedData = [
      buckets.map((bucket) => Date.parse(bucket) / 1000),
      ...series.map((line) => line.values),
    ];
    const plot = new uPlot(
      {
        width: container.clientWidth,
        height: HEIGHT,
        series: [
          {},
          ...series.map((line, i) => ({
            label: line.label || "всего",
            stroke: palette[i] ?? overflow,
            width: 2,
          })),
        ],
        axes: [{}, { values: (_, splits) => splits.map((value) => `${value} ${unit}`.trim()) }],
      },
      data,
      container,
    );
    const observer = new ResizeObserver(() => plot.setSize({ width: container.clientWidth, height: HEIGHT }));
    observer.observe(container);
    return () => {
      observer.disconnect();
      plot.destroy();
    };
  }, [buckets, series, unit]);

  return <div ref={ref} />;
}
