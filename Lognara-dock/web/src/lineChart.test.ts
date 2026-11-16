import assert from "node:assert/strict";
import test from "node:test";
import { chartColorToken, chartData, formatTimeAxisTicks, selectedChartRange, zoomChartRange } from "./components/lineChartUtils.ts";

test("chart data shares timestamp positions and keeps gaps as null", () => {
  assert.deepEqual(chartData(["2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z"], [
    { label: "INFO", values: [3, null] }, { label: "ERROR", values: [0, 2] },
  ]), [[1767225600, 1767225660], [3, null], [0, 2]]);
});

test("selection callback uses ordered millisecond boundaries", () => {
  assert.deepEqual(selectedChartRange(10, 5), { from: 5000, to: 10000 });
  assert.equal(selectedChartRange(2, 2), null);
});

test("time axis uses compact local labels and marks a date change", () => {
  const values = [
    "2026-11-16T23:55:00Z", "2026-11-17T00:00:00Z", "2026-11-17T00:05:00Z",
  ].map((value) => Date.parse(value) / 1000);
  assert.deepEqual(formatTimeAxisTicks(values, 500, "UTC"), ["16.11 23:55", "17.11 00:00", "00:05"]);
});

test("long time ranges thin labels to fit the chart width", () => {
  const values = Array.from({ length: 15 }, (_, index) => Date.parse("2026-11-01T00:00:00Z") / 1000 + index * 12 * 60 * 60);
  const labels = formatTimeAxisTicks(values, 336, "UTC");
  assert.equal(labels.filter(Boolean).length, 3);
  assert.match(labels.filter(Boolean)[0], /^01\.11 /);
  assert.match(labels.filter(Boolean).at(-1)!, /^08\.11 /);
});

test("series colors use the level palette and a neutral overflow token", () => {
  assert.equal(chartColorToken(0), "--lvl-info");
  assert.equal(chartColorToken(7), "--ok");
  assert.equal(chartColorToken(8), "--ink3");
});

test("wheel zoom keeps the pointer anchor and clamps to data bounds", () => {
  assert.deepEqual(zoomChartRange({ from: 20, to: 60 }, { from: 0, to: 100 }, 0.25, "in"), { from: 25, to: 45 });
  assert.deepEqual(zoomChartRange({ from: 80, to: 100 }, { from: 0, to: 100 }, 0.75, "out"), { from: 60, to: 100 });
  assert.equal(zoomChartRange({ from: 0, to: 100 }, { from: 0, to: 100 }, 0.5, "out"), null);
});
