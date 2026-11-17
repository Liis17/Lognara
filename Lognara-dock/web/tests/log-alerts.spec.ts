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
    if (path === "/api/series") return route.fulfill({ json: { metrics: [] } });
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
    if (path === "/api/alerts/preview") return route.fulfill({ json: {
      html: "<b>🔴 Алерт сработал</b>\n<b>Логи</b>\nЗапрос: service:host\nСовпадений: 1",
      button: true,
    } });
    if (path === "/api/log-alerts" && method === "POST") {
      const body = request.postDataJSON();
      state.writes.push({ method, path, body });
      state.alerts.push({ id: "a1", revision: 1, no_recipients: false, last_count: 0, last_match_at: null,
        last_checked_at: null, last_sent_at: null, ...body });
      return route.fulfill({ status: 201, json: { id: "a1" } });
    }
    if (path === "/api/alerts/test") {
      state.writes.push({ method, path, body: request.postDataJSON() });
      return route.fulfill({ json: { sent: 1, failed: [{ recipient: "bob", error: "recipient_unavailable" }] } });
    }
    if (path.startsWith("/api/log-alerts/") && method === "PUT") {
      const body = request.postDataJSON();
      state.writes.push({ method, path, body });
      const index = state.alerts.findIndex((alert) => `/api/log-alerts/${alert.id}` === path);
      state.alerts[index] = { ...state.alerts[index], ...body };
      return route.fulfill({ status: 204 });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  return state;
}

test("Logs query becomes a log alert with preview and groups", async ({ page }) => {
  const state = await fixture(page);
  await page.goto(`/log-alerts?${new URLSearchParams({ q: 'service:host "COMMAND=/tmp"' })}`);
  await expect(page).toHaveURL(/\/alerts\?/);
  await expect(page.getByRole("textbox", { name: "Запрос" })).toHaveValue('service:host "COMMAND=/tmp"');
  await page.getByRole("textbox", { name: "Имя" }).fill("sudo из /tmp");
  await page.getByRole("button", { name: "Проверить за час" }).click();
  await expect(page.getByLabel("Предпросмотр")).toContainText("Совпадений за последний час: 2");
  await expect(page.getByLabel("Предпросмотр")).toContainText("sudo: eve COMMAND=/tmp/x");
  await page.getByRole("button", { name: "Предпросмотр Telegram" }).click();
  await expect(page.getByLabel("Предпросмотр сообщения Telegram")).toContainText("Алерт сработал");
  await page.getByLabel("Дежурные").check();
  await page.getByRole("button", { name: "Создать алерт" }).click();
  await expect(page.locator(".alert-rule-row").getByText("sudo из /tmp", { exact: true })).toBeVisible();
  const created = state.writes.find((write) => write.method === "POST" && write.path === "/api/log-alerts");
  expect(created?.body).toEqual({
    name: "sudo из /tmp", enabled: true, query: 'service:host "COMMAND=/tmp"',
    filters: { service: ["host"] }, text: { query: "COMMAND=/tmp", mode: "phrase" },
    threshold: 1, cooldown_seconds: 300, group_ids: ["g1"], message_template: "",
    message_fields: ["name", "count", "query", "samples", "time", "status"],
  });
});

test("notification link node filters remain in preview and saved alert", async ({ page }) => {
  const state = await fixture(page);
  const params = new URLSearchParams({ q: "service:host", relay_id: "4", agent_id: "9" });
  await page.goto(`/log-alerts?${params}`);
  await page.getByRole("textbox", { name: "Имя" }).fill("Scoped alert");
  await page.getByRole("button", { name: "Проверить за час" }).click();
  await expect.poll(() => state.writes.find((write) => write.path === "/api/stats/summary")?.body).toMatchObject({
    filters: { service: ["host"], relay_id: ["4"], agent_id: ["9"] },
  });
  await page.getByRole("button", { name: "Создать алерт" }).click();
  const created = state.writes.find((write) => write.method === "POST" && write.path === "/api/log-alerts");
  expect(created?.body).toMatchObject({
    query: "service:host",
    filters: { service: ["host"], relay_id: ["4"], agent_id: ["9"] },
  });
});

test("single-event fields are rejected before saving", async ({ page }) => {
  const state = await fixture(page);
  await page.goto("/alerts?tab=rules&type=logs&q=");
  await page.getByRole("textbox", { name: "Имя" }).fill("trace");
  await page.getByRole("textbox", { name: "Запрос" }).fill("trace_id:0123456789abcdef0123456789abcdef");
  await page.getByRole("button", { name: "Создать алерт" }).click();
  await expect(page.getByText("Поле trace_id нельзя использовать в алерте")).toBeVisible();
  expect(state.writes.some((write) => write.path === "/api/log-alerts")).toBe(false);
});

test("renames a log alert from its tile and keeps the rest of the rule", async ({ page }) => {
  const state = await fixture(page);
  state.alerts.push({ id: "a1", revision: 1, no_recipients: false, last_count: 0, last_match_at: null,
    last_checked_at: null, last_sent_at: null, name: "sudo", enabled: true, query: "service:host",
    filters: { service: ["host"] }, threshold: 1, cooldown_seconds: 300, group_ids: ["g1"],
    message_template: "", message_fields: ["name"] });
  await page.goto("/alerts");
  const tile = page.locator(".alert-rule-row");
  await tile.hover();
  await tile.getByRole("button", { name: "Переименовать" }).click();
  await tile.getByLabel("Публичное имя алерта").fill("sudo на хосте");
  await tile.getByRole("button", { name: "Сохранить" }).click();
  await expect(tile.getByText("sudo на хосте", { exact: true })).toBeVisible();
  expect(state.writes).toEqual([{ method: "PUT", path: "/api/log-alerts/a1", body: {
    name: "sudo на хосте", enabled: true, query: "service:host", filters: { service: ["host"] }, threshold: 1,
    cooldown_seconds: 300, group_ids: ["g1"], message_template: "", message_fields: ["name"],
  } }]);
});

test("sends a test log alert to the selected groups", async ({ page }) => {
  const state = await fixture(page);
  await page.goto(`/log-alerts?${new URLSearchParams({ q: "service:host" })}`);
  await page.getByRole("textbox", { name: "Имя" }).fill("sudo");
  const send = page.getByRole("button", { name: "Отправить тест в Telegram" });
  await expect(send).toBeDisabled();
  await page.getByLabel("Дежурные").check();
  await send.click();
  await expect(page.getByRole("status").filter({ hasText: "Тестовое сообщение" }))
    .toHaveText("Тестовое сообщение отправлено в чатов: 1. Не доставлено: bob (чат недоступен или бот заблокирован).");
  const write = state.writes.find((item) => item.path === "/api/alerts/test");
  expect(write?.body).toMatchObject({ group_ids: ["g1"], message: { kind: "logs", name: "sudo", query: "service:host" } });
  expect(state.alerts).toHaveLength(0);
});
