import { useEffect, useMemo, useRef, useState } from "react";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import type { SeriesLine } from "../api";
import { chartColorToken, chartData, selectedChartRange, zoomChartRange } from "./lineChart";

export interface LineChartProps {
  buckets: string[];
  series: SeriesLine[];
  unit?: string;
  visibleSeries: ReadonlySet<number>;
  onToggleSeries: (index: number) => void;
  onSelectRange: (from: number, to: number) => void;
  onZoom: (from: number, to: number) => void;
}

interface Hover { index: number; left: number; tooltipLeft: number }
const HEIGHT = 320;
const number = new Intl.NumberFormat("ru-RU", { maximumFractionDigits: 4 });

export default function LineChart({ buckets, series, unit = "", visibleSeries, onToggleSeries, onSelectRange, onZoom }: LineChartProps) {
  const container = useRef<HTMLDivElement>(null);
  const plotRef = useRef<uPlot | null>(null);
  const callbacks = useRef({ onSelectRange, onZoom });
  const [hover, setHover] = useState<Hover | null>(null);
  callbacks.current = { onSelectRange, onZoom };
  const timestamps = useMemo(() => buckets.map((bucket) => Date.parse(bucket)), [buckets]);
  const timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const tooltipTime = useMemo(() => new Intl.DateTimeFormat("ru-RU", {
    timeZone, day: "2-digit", month: "2-digit", year: "numeric",
    hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23",
  }), [timeZone]);

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    const theme = getComputedStyle(element);
    const colorValues = series.map((_line, index) => theme.getPropertyValue(chartColorToken(index)).trim() || theme.getPropertyValue("--accent").trim());
    const ink = theme.getPropertyValue("--ink3").trim();
    const grid = theme.getPropertyValue("--line").trim();
    const axis = { font: "12px Arial", stroke: ink, grid: { stroke: grid }, ticks: { stroke: grid } };
    const data = chartData(buckets, series) as uPlot.AlignedData;
    const plot = new uPlot({
      width: element.clientWidth,
      height: HEIGHT,
      scales: { x: { time: true } },
      series: [
        {},
        ...series.map((line, index) => ({
          label: line.label || `Серия ${index + 1}`,
          stroke: colorValues[index],
          width: 2,
          points: { show: true, size: 5, width: 1, stroke: colorValues[index], fill: colorValues[index] },
        })),
      ],
      axes: [
        { ...axis, values: (_self, values) => (values ?? []).map((value) => tooltipTime.format(new Date(Number(value) * 1000))) },
        { ...axis },
      ],
      cursor: { show: true, x: true, y: false, drag: { x: true, y: false, setScale: false, dist: 4 } },
      select: { show: true, left: 0, top: 0, width: 0, height: 0 },
      legend: { show: false },
      hooks: {
        setCursor: [(self) => {
          const index = self.cursor.idx;
          if (index == null || index < 0 || index >= buckets.length) {
            setHover(null);
            return;
          }
          const left = self.valToPos(Number(self.data[0][index]), "x", true);
          setHover({ index, left, tooltipLeft: Math.min(left + 10, Math.max(8, element.clientWidth - 220)) });
        }],
        setSelect: [(self) => {
          if (!self.select.width) return;
          const range = selectedChartRange(
            self.posToVal(self.select.left, "x"),
            self.posToVal(self.select.left + self.select.width, "x"),
          );
          if (range) callbacks.current.onSelectRange(range.from, range.to);
        }],
      },
    }, data, element);
    plotRef.current = plot;
    const observer = new ResizeObserver(() => plot.setSize({ width: element.clientWidth, height: HEIGHT }));
    observer.observe(element);
    return () => {
      observer.disconnect();
      plot.destroy();
      plotRef.current = null;
    };
  }, [buckets, series, tooltipTime]);

  useEffect(() => {
    const plot = plotRef.current;
    if (!plot) return;
    series.forEach((_line, index) => plot.setSeries(index + 1, { show: visibleSeries.has(index) }));
  }, [series, visibleSeries]);

  useEffect(() => {
    const plot = plotRef.current;
    const over = plot?.over;
    if (!plot || !over || timestamps.length < 2) return;
    const full = { from: timestamps[0] / 1000, to: timestamps[timestamps.length - 1] / 1000 };
    const wheel = (event: WheelEvent) => {
      if (!event.deltaY) return;
      const scale = plot.scales.x;
      if (scale.min == null || scale.max == null) return;
      const rect = over.getBoundingClientRect();
      const fraction = Math.min(1, Math.max(0, (event.clientX - rect.left) / rect.width));
      const next = zoomChartRange(
        { from: scale.min, to: scale.max }, full, fraction, event.deltaY < 0 ? "in" : "out",
      );
      if (!next) return;
      event.preventDefault();
      plot.setScale("x", next);
      const range = selectedChartRange(next.from, next.to);
      if (range) callbacks.current.onZoom(range.from, range.to);
    };
    over.addEventListener("wheel", wheel, { passive: false });
    return () => over.removeEventListener("wheel", wheel);
  }, [timestamps]);

  const hoveredTimestamp = hover ? buckets[hover.index] : undefined;
  return <section className="line-chart" aria-label="Линейная диаграмма">
    {unit && <div className="line-chart-unit">{unit}</div>}
    {series.length > 0 && <div className="line-chart-legend" role="group" aria-label="Серии диаграммы">
      {series.map((line, index) => <button key={`${index}/${line.label}`} type="button" aria-pressed={visibleSeries.has(index)}
        onClick={() => onToggleSeries(index)}>
        <i aria-hidden="true" style={{ backgroundColor: `var(${chartColorToken(index)})` }} />{line.label || `Серия ${index + 1}`}
      </button>)}
    </div>}
    <div className="line-chart-frame" onMouseLeave={() => setHover(null)}>
      <div ref={container} className="line-chart-plot" role="group" aria-label="Диаграмма: наведите для значений, выделите диапазон мышью, колесо меняет масштаб" />
      {hover && <>
        <svg className="line-chart-points" aria-hidden="true">
          {series.map((line, index) => {
            const value = line.values[hover.index];
            if (!visibleSeries.has(index) || value == null || !Number.isFinite(value)) return null;
            const position = plotRef.current?.valToPos(value, "y", true);
            return position == null ? null : <circle key={index} cx={hover.left} cy={position} r="4" fill={`var(${chartColorToken(index)})`} />;
          })}
        </svg>
        <div className="line-chart-tooltip" style={{ left: `${hover.tooltipLeft}px` }} role="status">
          {hoveredTimestamp && <time dateTime={hoveredTimestamp}>{tooltipTime.format(new Date(hoveredTimestamp))}</time>}
          {series.map((line, index) => <div key={`${index}/${line.label}`}><i style={{ backgroundColor: `var(${chartColorToken(index)})` }} />
            {line.label || `Серия ${index + 1}`}: {line.values[hover.index] == null ? "—" : `${number.format(line.values[hover.index]!)}${unit ? ` ${unit}` : ""}`}
          </div>)}
        </div>
      </>}
    </div>
    {!!buckets.length && <details className="histogram-data">
      <summary>Числа диаграммы</summary>
      <div className="table-scroll"><table>
        <thead><tr><th scope="col">Время</th>{series.map((line, index) => <th scope="col" key={`${index}/${line.label}`}>{line.label || `Серия ${index + 1}`}</th>)}</tr></thead>
        <tbody>{buckets.map((bucket, row) => <tr key={`${bucket}/${row}`}>
          <td><time dateTime={bucket}>{tooltipTime.format(new Date(bucket))}</time></td>
          {series.map((line, index) => <td key={`${index}/${line.label}`}>{line.values[row] == null ? "—" : number.format(line.values[row]!)}</td>)}
        </tr>)}</tbody>
      </table></div>
    </details>}
  </section>;
}
