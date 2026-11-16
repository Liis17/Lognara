export type DateRangePreset = "15m" | "1h" | "6h" | "24h" | "7d";
export interface DateRange { from: string; to: string }
export interface ZonedParts { year: number; month: number; day: number; hour: number; minute: number; second: number }

const dateTimeParts = (timeZone: string) => new Intl.DateTimeFormat("en-GB", {
  timeZone, year: "numeric", month: "2-digit", day: "2-digit",
  hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23",
});

export function zonedParts(value: number | string | Date, timeZone: string): ZonedParts {
  const date = value instanceof Date ? value : new Date(value);
  const parts = Object.fromEntries(dateTimeParts(timeZone).formatToParts(date).map((part) => [part.type, Number(part.value)]));
  return { year: parts.year, month: parts.month, day: parts.day, hour: parts.hour, minute: parts.minute, second: parts.second };
}

export function dateKey(year: number, month: number, day: number): string {
  return `${year.toString().padStart(4, "0")}-${month.toString().padStart(2, "0")}-${day.toString().padStart(2, "0")}`;
}

export function dateKeyInZone(value: number | string | Date, timeZone: string): string {
  const parts = zonedParts(value, timeZone);
  return dateKey(parts.year, parts.month, parts.day);
}

export function timeInZone(value: number | string | Date, timeZone: string): string {
  const { hour, minute, second } = zonedParts(value, timeZone);
  return `${hour.toString().padStart(2, "0")}:${minute.toString().padStart(2, "0")}:${second.toString().padStart(2, "0")}`;
}

export function daysInCalendarMonth(year: number, month: number): string[] {
  const first = new Date(Date.UTC(year, month - 1, 1));
  const mondayOffset = (first.getUTCDay() + 6) % 7;
  const start = Date.UTC(year, month - 1, 1 - mondayOffset);
  return Array.from({ length: 42 }, (_, index) => {
    const day = new Date(start + index * 86_400_000);
    return dateKey(day.getUTCFullYear(), day.getUTCMonth() + 1, day.getUTCDate());
  });
}

export function shiftMonth(year: number, month: number, amount: number): { year: number; month: number } {
  const shifted = new Date(Date.UTC(year, month - 1 + amount, 1));
  return { year: shifted.getUTCFullYear(), month: shifted.getUTCMonth() + 1 };
}

export function zonedTimeToEpoch(day: string, time: string, timeZone: string): number | null {
  const dateMatch = /^(\d{4})-(\d{2})-(\d{2})$/u.exec(day);
  const timeMatch = /^(\d{2}):(\d{2}):(\d{2})$/u.exec(time);
  if (!dateMatch || !timeMatch) return null;
  const [year, month, date] = dateMatch.slice(1).map(Number);
  const [hour, minute, second] = timeMatch.slice(1).map(Number);
  if (month < 1 || month > 12 || date < 1 || date > 31 || hour > 23 || minute > 59 || second > 59) return null;
  const wallTime = Date.UTC(year, month - 1, date, hour, minute, second);
  const normalized = new Date(wallTime);
  if (normalized.getUTCFullYear() !== year || normalized.getUTCMonth() + 1 !== month || normalized.getUTCDate() !== date) return null;
  let epoch = wallTime;
  for (let attempt = 0; attempt < 3; attempt++) {
    const parts = zonedParts(epoch, timeZone);
    const shownAsUtc = Date.UTC(parts.year, parts.month - 1, parts.day, parts.hour, parts.minute, parts.second);
    epoch += wallTime - shownAsUtc;
  }
  const resolved = zonedParts(epoch, timeZone);
  if (resolved.year !== year || resolved.month !== month || resolved.day !== date ||
      resolved.hour !== hour || resolved.minute !== minute || resolved.second !== second) return null;
  return epoch;
}

export function rangeForZonedDays(start: string, end: string, startTime: string, endTime: string, timeZone: string): DateRange | null {
  const from = zonedTimeToEpoch(start, startTime, timeZone);
  const to = zonedTimeToEpoch(end, endTime, timeZone);
  if (from === null || to === null || to <= from) return null;
  return { from: new Date(from).toISOString(), to: new Date(to).toISOString() };
}

export function presetDateRange(preset: DateRangePreset, now = Date.now()): DateRange {
  const durations: Record<DateRangePreset, number> = {
    "15m": 15 * 60_000, "1h": 60 * 60_000, "6h": 6 * 60 * 60_000,
    "24h": 24 * 60 * 60_000, "7d": 7 * 24 * 60 * 60_000,
  };
  return { from: new Date(now - durations[preset]).toISOString(), to: new Date(now).toISOString() };
}

export function formatDateRange(range: DateRange, timeZone: string): string {
  const format = new Intl.DateTimeFormat("ru-RU", {
    timeZone, day: "2-digit", month: "2-digit", year: "numeric",
    hour: "2-digit", minute: "2-digit", second: "2-digit", hourCycle: "h23",
  });
  return `${format.format(new Date(range.from))} → ${format.format(new Date(range.to))}`;
}
