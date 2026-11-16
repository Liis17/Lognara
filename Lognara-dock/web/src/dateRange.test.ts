import assert from "node:assert/strict";
import test from "node:test";
import { dateKeyInZone, daysInCalendarMonth, presetDateRange, rangeForZonedDays, shiftMonth, timeInZone } from "./components/dateRange.ts";

test("calendar renders a Monday-first six-week month grid", () => {
  const days = daysInCalendarMonth(2026, 11);
  assert.equal(days.length, 42);
  assert.equal(days[0], "2026-10-26");
  assert.equal(days[5], "2026-10-31");
  assert.equal(days[6], "2026-11-01");
});

test("zoned calendar input preserves wall time when converted to UTC", () => {
  const range = rangeForZonedDays("2026-11-16", "2026-11-16", "12:30:00", "13:30:00", "Asia/Yerevan");
  assert.deepEqual(range, { from: "2026-11-16T08:30:00.000Z", to: "2026-11-16T09:30:00.000Z" });
  assert.equal(dateKeyInZone(range!.from, "Asia/Yerevan"), "2026-11-16");
  assert.equal(timeInZone(range!.to, "Asia/Yerevan"), "13:30:00");
});

test("zoned range rejects invalid times and an end before its start", () => {
  assert.equal(rangeForZonedDays("2026-11-16", "2026-11-16", "25:00:00", "26:00:00", "UTC"), null);
  assert.equal(rangeForZonedDays("2026-11-16", "2026-11-16", "12:00:00", "11:00:00", "UTC"), null);
});

test("presets use exact durations and month navigation crosses year boundaries", () => {
  const now = Date.parse("2026-01-01T00:00:00.000Z");
  assert.deepEqual(presetDateRange("15m", now), { from: "2025-12-31T23:45:00.000Z", to: "2026-01-01T00:00:00.000Z" });
  assert.deepEqual(shiftMonth(2026, 1, -1), { year: 2025, month: 12 });
  assert.deepEqual(shiftMonth(2026, 12, 1), { year: 2027, month: 1 });
});
