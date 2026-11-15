import { expect, test, type Page } from "@playwright/test";

async function fixture(page: Page) {
  const state = {
    alerts: [] as Record<string, unknown>[],
    writes: [] as { method: string; path: string; body: unknown }[],
  };
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/alerts") return route.fulfill({ json: {
      enabled: true, token_configured: true, send_paused: false, send_pause_reason: null, core_status: "available",
      core_checked_at: null, core_error: null, recipients: [], alerts: [], pending_deliveries: 0, permanent_errors: 0,
      groups: [{ id: "g1", kind: "personal", name: "Дежурные", recipient_ids: ["r1"], alerts: [], log_alerts: [] }],
    } });
    if (path === "/api/log-alerts" && method === "GET") {
      return route.fulfill({ json: { alerts: state.alerts, core_status: "available" } });
    }
    if (path === "/api/stats/summary") {
      state.writes.push({ method, path, body: request.postDataJSON() });
      return route.fulfill({ json: {
        as_of: "2026-11-15T10:00:00Z", total: 2, buckets: [], levels: {}, error_services: [],
        duration: { count: 0, p95_ms: null }, actions: [],
        page: { events: [{ id: "e1", timestamp: "2026-11-15T09:59:00Z", message: "sudo: eve COMMAND=/tmp/x" }],
          next_cursor: null, as_of: "2026-11-15T10:00:00Z" },
      } });
    }
    if (path === "/api/log-alerts" && method === "POST") {
      const body = request.postDataJSON();
      state.writes.push({ method, path, body });
      state.alerts.push({ id: "a1", revision: 1, no_recipients: false, last_count: 0, last_match_at: null,
        last_checked_at: null, last_sent_at: null, ...body });
      return route.fulfill({ status: 201, json: { id: "a1" } });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  return state;
}

test("Logs query becomes a log alert with preview and groups", async ({ page }) => {
  const state = await fixture(page);
  await page.goto(`/log-alerts?${new URLSearchParams({ q: 'service:host "COMMAND=/tmp"' })}`);
  await expect(page.getByLabel("Запрос")).toHaveValue('service:host "COMMAND=/tmp"');
  await page.getByLabel("Имя").fill("sudo из /tmp");
  await page.getByRole("button", { name: "Проверить за час" }).click();
  await expect(page.getByLabel("Предпросмотр")).toContainText("Совпадений за последний час: 2");
  await expect(page.getByLabel("Предпросмотр")).toContainText("sudo: eve COMMAND=/tmp/x");
  await page.getByLabel("Дежурные").check();
  await page.getByRole("button", { name: "Создать алерт" }).click();
  await expect(page.getByLabel("Алерт sudo из /tmp")).toBeVisible();
  const created = state.writes.find((write) => write.method === "POST" && write.path === "/api/log-alerts");
  expect(created?.body).toEqual({
    name: "sudo из /tmp", enabled: true, query: 'service:host "COMMAND=/tmp"',
    filters: { service: ["host"] }, text: { query: "COMMAND=/tmp", mode: "phrase" },
    threshold: 1, cooldown_seconds: 300, group_ids: ["g1"],
  });
});

test("single-event fields are rejected before saving", async ({ page }) => {
  const state = await fixture(page);
  await page.goto("/log-alerts");
  await page.getByLabel("Имя").fill("trace");
  await page.getByLabel("Запрос").fill("trace_id:0123456789abcdef0123456789abcdef");
  await page.getByRole("button", { name: "Создать алерт" }).click();
  await expect(page.getByText("Поле trace_id нельзя использовать в алерте")).toBeVisible();
  expect(state.writes.some((write) => write.path === "/api/log-alerts")).toBe(false);
});
