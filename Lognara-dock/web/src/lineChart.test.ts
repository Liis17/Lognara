import assert from "node:assert/strict";
import test from "node:test";
import { chartColorToken, chartData, selectedChartRange, zoomChartRange } from "./components/lineChartUtils.ts";

test("chart data shares timestamp positions and keeps gaps as null", () => {
  assert.deepEqual(chartData(["2026-01-01T00:00:00Z", "2026-01-01T00:01:00Z"], [
    { label: "INFO", values: [3, null] }, { label: "ERROR", values: [0, 2] },
  ]), [[1767225600, 1767225660], [3, null], [0, 2]]);
});

test("selection callback uses ordered millisecond boundaries", () => {
  assert.deepEqual(selectedChartRange(10, 5), { from: 5000, to: 10000 });
  assert.equal(selectedChartRange(2, 2), null);
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
