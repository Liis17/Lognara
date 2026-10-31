import { expect, test, type Page } from "@playwright/test";
import type { MetricAlertSettings } from "../src/api";

const key = {
  server: "server-a",
  service: "billing",
  name: "requests",
  origin: "counter" as const,
};

function initialSettings(): MetricAlertSettings {
  return {
    enabled: false,
    token_configured: false,
    send_paused: false,
    send_pause_reason: null,
    core_status: "unknown",
    core_checked_at: null,
    core_error: null,
    recipients: [],
    groups: [],
    alerts: [],
    pending_deliveries: 0,
    permanent_errors: 0,
  };
}

async function settingsFixture(page: Page) {
  const state: MetricAlertSettings = initialSettings();
  const writes: { method: string; path: string; body: unknown }[] = [];
  let nextRecipientId = 1;
  let nextGroupId = 1;
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
    const body = method === "GET" || method === "DELETE" ? null : request.postDataJSON();
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/account/passkeys") return route.fulfill({ json: { passkeys: [] } });
    if (path === "/api/alerts" && method === "GET")
      return route.fulfill({ json: structuredClone(state) });
    if (path === "/api/alerts/token" && method === "PUT") {
      writes.push({ method, path, body });
      state.token_configured = true;
      return route.fulfill({ status: 204 });
    }
    if (path === "/api/alerts/global" && method === "PUT") {
      writes.push({ method, path, body });
      state.enabled = (body as { enabled: boolean }).enabled;
      return route.fulfill({ status: 204 });
    }
    if (path === "/api/alerts/recipients" && method === "POST") {
      writes.push({ method, path, body });
      const recipient = {
        id: `recipient-${nextRecipientId++}`,
        ...(body as object),
        last_error: null,
      } as (typeof state.recipients)[number];
      state.recipients.push(recipient);
      return route.fulfill({ status: 201, json: recipient });
    }
    if (/^\/api\/alerts\/recipients\/recipient-\d+$/.test(path) && method === "PUT") {
      writes.push({ method, path, body });
      const recipient = state.recipients.find((item) => item.id === path.split("/").at(-1))!;
      Object.assign(recipient, body);
      return route.fulfill({ json: recipient });
    }
    if (/^\/api\/alerts\/recipients\/recipient-\d+$/.test(path) && method === "DELETE") {
      writes.push({ method, path, body });
      const id = path.split("/").at(-1);
      state.recipients = state.recipients.filter((item) => item.id !== id);
      for (const group of state.groups)
        group.recipient_ids = group.recipient_ids.filter((recipientId) => recipientId !== id);
      return route.fulfill({ status: 204 });
    }
    if (path === "/api/alerts/groups" && method === "POST") {
      writes.push({ method, path, body });
      const group = {
        id: `group-${nextGroupId++}`,
        ...(body as object),
        alerts: [key],
      } as (typeof state.groups)[number];
      state.groups.push(group);
      return route.fulfill({ status: 201, json: group });
    }
    if (/^\/api\/alerts\/groups\/group-\d+$/.test(path) && method === "PUT") {
      writes.push({ method, path, body });
      const group = state.groups.find((item) => item.id === path.split("/").at(-1))!;
      Object.assign(group, body);
      return route.fulfill({ json: group });
    }
    if (/^\/api\/alerts\/groups\/group-\d+$/.test(path) && method === "DELETE") {
      writes.push({ method, path, body });
      return route.fulfill({
        status: 409,
        json: { error: "group_in_use", alerts: [key] },
      });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  return { state, writes };
}

test("settings validate Telegram first and provide recipient and group CRUD", async ({ page }) => {
  const { state, writes } = await settingsFixture(page);
  await page.goto("/settings");
  const alerts = page.getByRole("region", { name: "Алерты метрик" });
  await expect(alerts.getByText("Core: unknown")).toBeVisible();

  await alerts.getByLabel("Токен бота").fill("123456:abcdefghijklmnopqrstuvwxyz0123456789");
  await alerts.getByRole("button", { name: "Проверить и сохранить токен" }).click();
  await expect(alerts.getByText("Токен проверен и сохранён на сервере.")).toBeVisible();
  expect(state.token_configured).toBe(true);
  expect(JSON.stringify(state)).not.toContain("123456:abcdefghijklmnopqrstuvwxyz0123456789");
  await expect(alerts.getByLabel("Включить алерты глобально")).toBeEnabled();
  await alerts.getByLabel("Включить алерты глобально").click();
  await expect(alerts.getByLabel("Включить алерты глобально")).toBeChecked();

  await alerts.getByLabel("Название", { exact: true }).first().fill("On-call");
  await alerts.getByLabel("Numeric Telegram chat ID").fill("-1001234567890");
  await alerts.getByRole("button", { name: "Добавить получателя" }).click();
  await expect(alerts.getByText("On-call", { exact: true })).toBeVisible();
  await alerts.getByRole("button", { name: "Изменить" }).first().click();
  await alerts.getByLabel("Название", { exact: true }).first().fill("Primary on-call");
  await alerts.getByRole("button", { name: "Сохранить получателя" }).click();
  await expect(alerts.getByText("Primary on-call", { exact: true })).toBeVisible();

  const groupPanel = alerts.getByRole("region", { name: "Группы получателей" });
  await groupPanel.getByLabel("Название", { exact: true }).fill("Primary");
  await groupPanel.getByLabel(/Primary on-call/).check();
  await groupPanel.getByRole("button", { name: "Добавить группу" }).click();
  await expect(groupPanel.getByText("Primary", { exact: true })).toBeVisible();
  await groupPanel.getByRole("button", { name: "Изменить" }).click();
  await groupPanel.getByLabel("Название", { exact: true }).fill("Primary team");
  await groupPanel.getByRole("button", { name: "Сохранить группу" }).click();
  await expect(groupPanel.getByText("Primary team", { exact: true })).toBeVisible();
  await groupPanel.getByRole("button", { name: "Удалить" }).click();
  await expect(groupPanel.getByText(/Сначала отсоедините группу от алертов: server-a \/ billing \/ requests/)).toBeVisible();

  await alerts.getByRole("button", { name: "Удалить" }).first().click();
  await expect(alerts.getByText("Получателей пока нет")).toBeVisible();
  await alerts.getByLabel("Тип получателя").selectOption("channel");
  await alerts.getByLabel("Название", { exact: true }).first().fill("Announcements");
  await alerts.getByLabel("Numeric Telegram chat ID").fill("-1009876543210");
  await alerts.getByRole("button", { name: "Добавить получателя" }).click();
  await expect(alerts.getByText("Announcements", { exact: true })).toBeVisible();
  await groupPanel.getByLabel("Тип группы").selectOption("channel");
  await groupPanel.getByLabel("Название", { exact: true }).fill("Announcements group");
  await groupPanel.getByLabel(/Announcements/).check();
  await groupPanel.getByRole("button", { name: "Добавить группу" }).click();
  await expect(groupPanel.getByText("Announcements group", { exact: true })).toBeVisible();
  expect(writes.map((write) => `${write.method} ${write.path}`)).toEqual([
    "PUT /api/alerts/token",
    "PUT /api/alerts/global",
    "POST /api/alerts/recipients",
    "PUT /api/alerts/recipients/recipient-1",
    "POST /api/alerts/groups",
    "PUT /api/alerts/groups/group-1",
    "DELETE /api/alerts/groups/group-1",
    "DELETE /api/alerts/recipients/recipient-1",
    "POST /api/alerts/recipients",
    "POST /api/alerts/groups",
  ]);
});

test("missing metrics still allow alert editing and show global pause and no recipients", async ({ page }) => {
  let saved = false;
  let savedGroupIds: string[] = [];
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const method = request.method();
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/series/lookup") return route.fulfill({ json: { metrics: [] } });
    if (path === "/api/alerts")
      return route.fulfill({
        json: {
          ...initialSettings(),
          token_configured: true,
          groups: [
            { id: "empty", kind: "personal", name: "Empty", recipient_ids: [], alerts: [] },
            {
              id: "announcements",
              kind: "channel",
              name: "Announcements",
              recipient_ids: ["channel-recipient"],
              alerts: [],
            },
          ],
        },
      });
    if (path === "/api/alerts/definition" && method === "GET")
      return route.fulfill({ json: { alert: null } });
    if (path === "/api/alerts/definition" && method === "PUT") {
      saved = true;
      const body = request.postDataJSON();
      savedGroupIds = body.group_ids;
      return route.fulfill({
        json: {
          id: "alert-1",
          alert: {
            id: "alert-1",
            ...body,
            revision: 1,
            enabled_at: 1,
            state: "unknown",
            loss_registered: false,
            loss_at: null,
            last_point_at: null,
            last_as_of: null,
            no_recipients: true,
          },
        },
      });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/metrics/detail?server=server-a&service=billing&name=requests&origin=counter");
  await expect(page.getByText("Метрика не найдена на выбранном сервере.")).toBeVisible();
  const editor = page.getByRole("region", { name: "Алерт метрики" });
  await expect(editor.getByRole("heading", { name: "Алерт метрики" })).toBeVisible();
  await expect(editor.getByRole("status")).toContainText("Глобальная отправка выключена");
  await expect(editor.getByLabel("Включить это правило")).toBeDisabled();
  await expect(editor.getByLabel("Обработка отсутствия данных")).toHaveValue("loss");
  await editor.getByLabel("Обработка отсутствия данных").selectOption("loss");
  await editor.getByRole("group", { name: "Группы получателей" }).getByLabel(/Empty/).check();
  await editor.getByRole("group", { name: "Группы получателей" }).getByLabel(/Announcements/).check();
  await editor.getByRole("button", { name: "Сохранить алерт" }).click();
  await expect(editor.getByText("Алерт сохранён.")).toBeVisible();
  await expect(editor.getByText(/нет групп с получателями/)).toBeVisible();
  expect(saved).toBe(true);
  expect(savedGroupIds).toEqual(["empty", "announcements"]);

  await page.goto("/metrics/detail?server=server-a&service=billing&name=custom&origin=rule");
  const ruleEditor = page.getByRole("region", { name: "Алерт метрики" });
  await expect(ruleEditor.getByLabel("Обработка отсутствия данных")).toHaveValue("normal");
});

test("token validation error is visible and does not enable global alerts", async ({ page }) => {
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/account/passkeys") return route.fulfill({ json: { passkeys: [] } });
    if (path === "/api/alerts" && request.method() === "GET")
      return route.fulfill({ json: initialSettings() });
    if (path === "/api/alerts/token")
      return route.fulfill({ status: 400, json: { error: "invalid_bot_token" } });
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/settings");
  const alerts = page.getByRole("region", { name: "Алерты метрик" });
  await alerts.getByLabel("Токен бота").fill("bad-token");
  await alerts.getByRole("button", { name: "Проверить и сохранить токен" }).click();
  await expect(alerts.getByRole("alert")).toHaveText("Telegram отклонил токен. Проверьте его и повторите");
  await expect(alerts.getByLabel("Включить алерты глобально")).toBeDisabled();
});
