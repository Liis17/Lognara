import { test } from "node:test";
import assert from "node:assert/strict";
import { refreshedText } from "./infraPresentation.ts";

test("refresh age is shown in seconds and minutes under ten minutes, then as clock time", () => {
  const loadedAt = new Date("2026-10-15T12:00:00Z");
  const at = (elapsedMs: number) => refreshedText(loadedAt, loadedAt.getTime() + elapsedMs);
  assert.equal(at(0), "0 сек назад");
  assert.equal(at(-5_000), "0 сек назад");
  assert.equal(at(59_999), "59 сек назад");
  assert.equal(at(60_000), "1 мин назад");
  assert.equal(at(599_999), "9 мин назад");
  assert.match(at(600_000), /^\d{2}:\d{2}$/);
});
