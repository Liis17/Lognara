import assert from "node:assert/strict";
import test from "node:test";
import { appendHistory, contextBounds, eventBounds, eventsToCsv, matchesQuery, parseQuery, prependLive, sourceQuery,
  withFilter, withPatternFilter } from "./logs.ts";
import type { LogEvent } from "./types.ts";

const event = (overrides: Partial<LogEvent> = {}): LogEvent => ({
  id: "019a8100-0000-7000-8000-000000000001", timestamp: "2026-10-14T12:00:00.123456789Z",
  ingested_at: "2026-10-14T12:00:01Z", level: "error", message: "Database connection refused",
  environment: "production", server: "eu-01", backend: "payments", service: "billing",
  service_instance: "billing-7f9c", action: null, trace_id: null, span_id: null,
  parent_span_id: null, request_id: null, attributes: {}, ...overrides,
});

test("template query maps AND fields and OR levels to core's exact filters", () => {
  assert.deepEqual(parseQuery("service:billing AND level:(ERROR OR WARN)"), {
    filters: { service: ["billing"], level: ["error", "warn"] },
  });
  assert.deepEqual(parseQuery('server:"prod eu" level:FATAL'), {
    filters: { server: ["prod eu"], level: ["fatal"] },
  });
});

test("message all/phrase retain their different token semantics", () => {
  const all = parseQuery("service:billing refused database");
  assert.deepEqual(all.text, { query: "refused database", mode: "all" });
  assert.equal(matchesQuery(event(), all), true);
  assert.equal(matchesQuery(event(), parseQuery('message:"connection refused"')), true);
  assert.equal(matchesQuery(event(), parseQuery('"refused database"')), false);
  assert.equal(matchesQuery(event({ level: "info" }), parseQuery("level:(ERROR OR WARN)")), false);
  assert.equal(matchesQuery(event({ message: "réseau: отказ, 5012 ms" }), parseQuery("RÉSEAU отказ")), true);
});

test("unsupported boolean operations and malformed queries fail explicitly", () => {
  for (const query of ["service:billing OR level:ERROR", "NOT service:billing", "level:(ERROR AND WARN)",
    "(service:billing)", "level:(ERROR OR level:WARN)", "service:billing AND", "AND level:WARN",
    "level:(ERROR OR)", "service:", 'message:"unfinished', "attributes.db:postgres", "level:WARNING",
    "service:billing AND service:checkout", '"connection refused" timeout', 'message:"one" message:"two"']) {
    assert.throws(() => parseQuery(query), Error, query);
  }
});

test("quoted exact values, case-sensitive fields and trace normalization are preserved", () => {
  const service = 'billing "east" \\ shard';
  const query = parseQuery(`service:${JSON.stringify(service)} AND trace_id:ABCDEF1234567890ABCDEF1234567890`);
  assert.equal(query.filters.service[0], service);
  assert.equal(query.filters.trace_id[0], "abcdef1234567890abcdef1234567890");
  assert.equal(matchesQuery(event({ service }), query), false);
  assert.throws(() => parseQuery("trace_id:abc"), /trace_id/);
  assert.throws(() => parseQuery("SERVICE:billing"), /не поддерживается/);
  const multiline = "host\nname\t\\backslash";
  assert.equal(parseQuery(`server:${JSON.stringify(multiline)}`).filters.server[0], multiline);
  assert.equal(matchesQuery(event(), parseQuery("id:019A8100000070008000000000000001")), true);
  assert.throws(() => parseQuery("id:broken"), /UUID/);
});

test("history preserves server order, nanos and repeated ids across cursor pages", () => {
  const first = event({ timestamp: "2026-10-14T12:00:00.123000099Z" });
  const second = event({ timestamp: "2026-10-14T12:00:00.123000001Z" });
  assert.equal(Date.parse(first.timestamp), Date.parse(second.timestamp));
  assert.deepEqual(appendHistory([first], [second]), [first, second]);
  assert.deepEqual(appendHistory([second], [first]), [second, first]);
});

test("live snapshots deduplicate separately and the pause buffer retains the newest bounded rows", () => {
  const older = event({ id: "older" });
  const newer = event({ id: "newer" });
  assert.deepEqual(prependLive([newer, older], [older, newer]).rows, [newer, older]);
  const paused = prependLive([], [older, newer], 1);
  assert.deepEqual(paused, { rows: [newer], trimmed: true });
  assert.deepEqual(prependLive([older], [...paused.rows].reverse()).rows, [newer, older]);
  assert.deepEqual(prependLive([], [older, older]).rows, [older]);
});

test("context pins ±5 minutes and exact source fields without importing event severity", () => {
  const source = event({ server: 'host "one"', service_instance: null });
  assert.deepEqual(parseQuery(sourceQuery(source)), { filters: {
    environment: ["production"], server: ['host "one"'], backend: ["payments"], service: ["billing"],
  } });
  assert.deepEqual(contextBounds(source.timestamp), {
    from: "2026-10-14T11:55:00.123Z", to: "2026-10-14T12:05:00.123Z",
  });
});

// Независимое чтение RFC4180, чтобы проверить фактические значения, а не реализацию writer.
function readCsv(text: string): string[][] {
  const rows: string[][] = [];
  let row: string[] = [], cell = "", quoted = false;
  text = text.replace(/^\uFEFF/u, "");
  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (char === '"') {
      if (quoted && text[i + 1] === '"') { cell += '"'; i++; } else quoted = !quoted;
    } else if (!quoted && char === ",") { row.push(cell); cell = ""; }
    else if (!quoted && char === "\r" && text[i + 1] === "\n") {
      row.push(cell); rows.push(row); row = []; cell = ""; i++;
    } else cell += char;
  }
  return rows;
}

test("CSV exports exactly loaded rows, preserving multiline messages and JSON attributes", () => {
  const input = [event({ message: 'Первое, "цитата"\r\nвторое', attributes: { retry: 3, nullable: null, ok: true } })];
  const [header, ...rows] = readCsv(eventsToCsv(input));
  assert.equal(rows.length, 1);
  assert.equal(rows[0][header.indexOf("timestamp")], input[0].timestamp);
  assert.equal(rows[0][header.indexOf("message")], input[0].message);
  assert.equal(rows[0][header.indexOf("action")], "");
  assert.deepEqual(JSON.parse(rows[0][header.indexOf("attributes")]), input[0].attributes);
  assert.equal(readCsv(eventsToCsv([])).length, 1);
});

test("CSV prevents spreadsheet formulas while leaving the source event unchanged", () => {
  for (const formula of ['=HYPERLINK("https://example.invalid")', "+SUM(1,2)", "-2+3", "@SUM(1)", "  =1+1", "\t=1+1"]) {
    const input = event({ message: formula });
    const [header, row] = readCsv(eventsToCsv([input]));
    assert.equal(row[header.indexOf("message")], `'${formula}`);
    assert.equal(input.message, formula);
  }
});

test('Live tokenization follows core Unicode tokenization then per-character lowercase', () => {
  assert.equal(matchesQuery(event({ message: 'İstanbul' }), parseQuery('i')), false);
  assert.equal(matchesQuery(event({ message: 'ΟΣ' }), parseQuery('οσ')), true);
  assert.equal(matchesQuery(event({ message: 'ΟΣ' }), parseQuery('ος')), false);
});

test('event lookup bounds preserve nanoseconds, carry seconds and handle pre-epoch timestamps', () => {
  assert.deepEqual(eventBounds('2026-10-14T12:00:00.123456789Z'), { from: '2026-10-14T12:00:00.123456789Z', to: '2026-10-14T12:00:00.123456790Z' });
  assert.equal(eventBounds('2026-10-14T12:00:00.999999999Z').to, '2026-10-14T12:00:01.000000000Z');
  assert.equal(eventBounds('1969-12-31T23:59:59.123456789Z').to, '1969-12-31T23:59:59.123456790Z');
  assert.throws(() => eventBounds('bad-time'));
});

test("pattern_id is validated, lowercased and replaces only the pattern filter in event links", () => {
  const id = `p1_${"ab".repeat(32)}`;
  assert.deepEqual(parseQuery(`pattern_id:${id.toUpperCase().replace("P1_", "p1_")}`), { filters: { pattern_id: [id] } });
  assert.throws(() => parseQuery("pattern_id:p1_123"), /pattern_id/);
  const link = withPatternFilter(`service:"prod api" AND pattern_id:(p1_${"0".repeat(64)} OR p1_${"1".repeat(64)}) AND "connection refused"`, id);
  assert.deepEqual(parseQuery(link), {
    filters: { service: ["prod api"], pattern_id: [id] }, text: { query: "connection refused", mode: "phrase" },
  });
  assert.deepEqual(parseQuery(withPatternFilter("payment failed", id)), {
    filters: { pattern_id: [id] }, text: { query: "payment failed", mode: "all" },
  });
});

test("fingerprint is validated, lowercased and replaces only its own filter", () => {
  const id = `f1_${"ab".repeat(32)}`;
  assert.deepEqual(parseQuery(`fingerprint:${id.toUpperCase().replace("F1_", "f1_")}`), { filters: { fingerprint: [id] } });
  assert.throws(() => parseQuery("fingerprint:p1_123"), /fingerprint/);
  assert.deepEqual(parseQuery(withFilter(`service:api AND fingerprint:f1_${"0".repeat(64)}`, "fingerprint", id)), {
    filters: { service: ["api"], fingerprint: [id] },
  });
});
