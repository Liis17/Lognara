import { useId, useState } from "react";
import {
  dateKeyInZone, daysInCalendarMonth, formatDateRange, presetDateRange, rangeForZonedDays,
  shiftMonth, timeInZone, type DateRangePreset,
} from "./dateRange";

export interface DateRangePickerProps {
  from: string;
  to: string;
  onChange: (from: string, to: string) => void;
  timeZone?: string;
}

const presets: { value: DateRangePreset; label: string }[] = [
  { value: "15m", label: "15 мин" }, { value: "1h", label: "1 ч" }, { value: "6h", label: "6 ч" },
  { value: "24h", label: "24 ч" }, { value: "7d", label: "7 дн" },
];
const monthName = new Intl.DateTimeFormat("ru-RU", { month: "long", year: "numeric", timeZone: "UTC" });
const weekDays = ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"];

export default function DateRangePicker({ from, to, onChange, timeZone = Intl.DateTimeFormat().resolvedOptions().timeZone }: DateRangePickerProps) {
  const id = useId();
  const startKey = dateKeyInZone(from, timeZone);
  const endKey = dateKeyInZone(to, timeZone);
  const initialMonth = startKey.slice(0, 7).split("-").map(Number);
  const [open, setOpen] = useState(false);
  const [month, setMonth] = useState({ year: initialMonth[0], month: initialMonth[1] });
  const [draftStart, setDraftStart] = useState<string | null>(null);
  const [draftEnd, setDraftEnd] = useState<string | null>(null);
  const [startTime, setStartTime] = useState(() => timeInZone(from, timeZone));
  const [endTime, setEndTime] = useState(() => timeInZone(to, timeZone));
  const days = daysInCalendarMonth(month.year, month.month);
  const selectedStart = draftStart ?? startKey;
  const selectedEnd = draftStart && !draftEnd ? draftStart : draftEnd ?? endKey;
  const todayKey = dateKeyInZone(Date.now(), timeZone);

  function show() {
    const parts = initialMonth;
    setMonth({ year: parts[0], month: parts[1] });
    setDraftStart(null);
    setDraftEnd(null);
    setStartTime(timeInZone(from, timeZone));
    setEndTime(timeInZone(to, timeZone));
    setOpen((value) => !value);
  }

  function applyDays(start: string, end: string) {
    const range = rangeForZonedDays(start, end, startTime, endTime, timeZone);
    if (range) onChange(range.from, range.to);
  }

  function selectDay(day: string) {
    if (!draftStart || draftEnd) {
      setDraftStart(day);
      setDraftEnd(null);
      return;
    }
    if (day < draftStart) {
      setDraftStart(day);
      return;
    }
    setDraftEnd(day);
    applyDays(draftStart, day);
  }

  function updateTime(which: "start" | "end", value: string) {
    if (which === "start") setStartTime(value); else setEndTime(value);
    if (!/^\d{2}:\d{2}:\d{2}$/u.test(value)) return;
    const start = selectedStart;
    const end = selectedEnd;
    const range = rangeForZonedDays(start, end, which === "start" ? value : startTime, which === "end" ? value : endTime, timeZone);
    if (range) onChange(range.from, range.to);
  }

  const label = formatDateRange({ from, to }, timeZone);
  const calendarLabel = monthName.format(new Date(Date.UTC(month.year, month.month - 1, 1)));

  return (
    <div className="date-range-picker">
      <button type="button" className="date-range-trigger" aria-haspopup="dialog" aria-expanded={open} onClick={show}>{label}</button>
      {open && <section className="date-range-popover" role="dialog" aria-label="Выбор диапазона времени">
        <div className="date-range-month">
          <button type="button" aria-label="Предыдущий месяц" onClick={() => setMonth((current) => shiftMonth(current.year, current.month, -1))}>‹</button>
          <strong>{calendarLabel}</strong>
          <button type="button" aria-label="Следующий месяц" onClick={() => setMonth((current) => shiftMonth(current.year, current.month, 1))}>›</button>
        </div>
        <div className="date-range-calendar" role="group" aria-label={`${calendarLabel}: календарь`}>
          {weekDays.map((name) => <span key={name} aria-hidden="true">{name}</span>)}
          {days.map((day) => {
            const inRange = day >= selectedStart && day <= selectedEnd;
            const selected = day === selectedStart || day === selectedEnd;
            return <button
              key={day}
              type="button"
              aria-label={day}
              aria-pressed={selected}
              aria-current={day === todayKey ? "date" : undefined}
              className={`${day.slice(0, 7) === `${month.year}-${String(month.month).padStart(2, "0")}` ? "" : "outside-month "}${inRange ? "in-range " : ""}${selected ? "selected" : ""}`.trim()}
              onClick={() => selectDay(day)}
            >{Number(day.slice(-2))}</button>;
          })}
        </div>
        <div className="date-range-times">
          <label htmlFor={`${id}-start`}>Начало <input id={`${id}-start`} aria-label="Время начала" inputMode="numeric" maxLength={8} placeholder="ЧЧ:ММ:СС" value={startTime}
            onChange={(event) => updateTime("start", event.target.value)} /></label>
          <label htmlFor={`${id}-end`}>Конец <input id={`${id}-end`} aria-label="Время окончания" inputMode="numeric" maxLength={8} placeholder="ЧЧ:ММ:СС" value={endTime}
            onChange={(event) => updateTime("end", event.target.value)} /></label>
        </div>
        <div className="date-range-presets" role="group" aria-label="Быстрые периоды">
          {presets.map((preset) => <button type="button" key={preset.value} onClick={() => {
            const range = presetDateRange(preset.value);
            setOpen(false);
            onChange(range.from, range.to);
          }}>{preset.label}</button>)}
        </div>
        <span className="date-range-zone">{timeZone}</span>
      </section>}
    </div>
  );
}
