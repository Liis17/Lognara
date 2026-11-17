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
    if (path === "/api/log-alerts" && method === "GET")
      return route.fulfill({ json: { alerts: [], core_status: "available" } });
    if (path === "/api/series") return route.fulfill({ json: { metrics: [] } });
    if (path === "/api/alerts" && method === "GET")
      return route.fulfill({ json: structuredClone(state) });
    if (path === "/api/alerts/definition" && method === "GET")
      return route.fulfill({ json: { alert: null } });
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
    if (/^\/api\/alerts\/rules\/[^/]+$/.test(path) && method === "DELETE") {
      writes.push({ method, path, body });
      const id = decodeURIComponent(path.split("/").at(-1)!);
      state.alerts = state.alerts.filter((alert) => alert.id !== id);
      return route.fulfill({ status: 204 });
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

test("alerts tabs validate Telegram first and provide recipient and group CRUD", async ({ page }) => {
  const { state, writes } = await settingsFixture(page);
  await page.goto("/alerts?tab=telegram");
  const telegram = page.getByRole("region", { name: "Telegram" });
  await expect(telegram.getByText("Core: unknown")).toBeVisible();

  await telegram.getByLabel("Токен бота").fill("123456:abcdefghijklmnopqrstuvwxyz0123456789");
  await telegram.getByRole("button", { name: "Проверить и сохранить токен" }).click();
  await expect(telegram.getByText("Токен проверен и сохранён на сервере.")).toBeVisible();
  expect(state.token_configured).toBe(true);
  expect(JSON.stringify(state)).not.toContain("123456:abcdefghijklmnopqrstuvwxyz0123456789");
  await expect(telegram.getByLabel("Включить отправку алертов")).toBeEnabled();
  await telegram.getByLabel("Включить отправку алертов").click();
  await expect(telegram.getByLabel("Включить отправку алертов")).toBeChecked();

  await page.getByRole("button", { name: "Получатели и группы" }).click();
  const recipients = page.getByRole("region", { name: "Получатели", exact: true });
  await recipients.getByRole("button", { name: "Добавить получателя" }).click();
  const recipientDialog = page.getByRole("dialog", { name: "Добавить получателя" });
  await recipientDialog.getByLabel("Название", { exact: true }).fill("On-call");
  await recipientDialog.getByLabel("Numeric Telegram chat ID").fill("-1001234567890");
  await recipientDialog.getByRole("button", { name: "Добавить получателя" }).click();
  await expect(recipients.getByText("On-call", { exact: true })).toBeVisible();
  await recipients.getByRole("button", { name: "Изменить" }).click();
  const editRecipientDialog = page.getByRole("dialog", { name: "Изменить получателя" });
  await editRecipientDialog.getByLabel("Название", { exact: true }).fill("Primary on-call");
  await editRecipientDialog.getByRole("button", { name: "Сохранить получателя" }).click();
  await expect(recipients.getByText("Primary on-call", { exact: true })).toBeVisible();

  const groupPanel = page.getByRole("region", { name: "Группы получателей" });
  await groupPanel.getByRole("button", { name: "Добавить группу" }).click();
  let groupDialog = page.getByRole("dialog", { name: "Добавить группу" });
  await groupDialog.getByLabel("Название", { exact: true }).fill("Primary");
  await groupDialog.getByLabel(/Primary on-call/).check();
  await groupDialog.getByRole("button", { name: "Добавить группу" }).click();
  await expect(groupPanel.getByText("Primary", { exact: true })).toBeVisible();
  await groupPanel.getByRole("button", { name: "Изменить" }).click();
  groupDialog = page.getByRole("dialog", { name: "Изменить группу" });
  await groupDialog.getByLabel("Название", { exact: true }).fill("Primary team");
  await groupDialog.getByRole("button", { name: "Сохранить группу" }).click();
  await expect(groupPanel.getByText("Primary team", { exact: true })).toBeVisible();
  await groupPanel.getByRole("button", { name: "Удалить" }).click();
  await expect(groupPanel.getByText(/Сначала отсоедините группу от алертов: server-a \/ billing \/ requests/)).toBeVisible();

  await recipients.getByRole("button", { name: "Удалить" }).click();
  await expect(recipients.getByText("Получателей пока нет")).toBeVisible();
  await recipients.getByRole("button", { name: "Добавить получателя" }).click();
  const channelDialog = page.getByRole("dialog", { name: "Добавить получателя" });
  await channelDialog.getByLabel("Тип получателя").selectOption("channel");
  await channelDialog.getByLabel("Название", { exact: true }).fill("Announcements");
  await channelDialog.getByLabel("Numeric Telegram chat ID").fill("-1009876543210");
  await channelDialog.getByRole("button", { name: "Добавить получателя" }).click();
  await expect(recipients.getByText("Announcements", { exact: true })).toBeVisible();
  await groupPanel.getByRole("button", { name: "Добавить группу" }).click();
  groupDialog = page.getByRole("dialog", { name: "Добавить группу" });
  await groupDialog.getByLabel("Тип группы").selectOption("channel");
  await groupDialog.getByLabel("Название", { exact: true }).fill("Announcements group");
  await groupDialog.getByLabel(/Announcements/).check();
  await groupDialog.getByRole("button", { name: "Добавить группу" }).click();
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
    if (path === "/api/series") return route.fulfill({ json: { metrics: [] } });
    if (path === "/api/log-alerts") return route.fulfill({ json: { alerts: [], core_status: "available" } });
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
  await page.goto("/alerts?tab=rules&type=metric&server=server-a&service=billing&name=requests&origin=counter");
  const editor = page.getByRole("region", { name: "Редактор алерта метрики" });
  await expect(editor.getByRole("heading", { name: "Редактор алерта метрики" })).toBeVisible();
  await expect(editor.getByRole("status")).toContainText("Глобальная отправка выключена");
  await expect(editor.getByLabel("Включить это правило")).toBeDisabled();
  await expect(editor.getByLabel("Обработка отсутствия данных")).toHaveValue("loss");
  await editor.getByRole("group", { name: "Группы получателей" }).getByLabel(/Empty/).check();
  await editor.getByRole("group", { name: "Группы получателей" }).getByLabel(/Announcements/).check();
  await editor.getByRole("button", { name: "Сохранить алерт" }).click();
  await expect(page.getByRole("button", { name: "Новый алерт" })).toBeVisible();
  expect(saved).toBe(true);
  expect(savedGroupIds).toEqual(["empty", "announcements"]);

  await page.goto("/alerts?tab=rules&type=metric&server=server-a&service=billing&name=custom&origin=rule");
  const ruleEditor = page.getByRole("region", { name: "Редактор алерта метрики" });
  await expect(ruleEditor.getByLabel("Обработка отсутствия данных")).toHaveValue("normal");
});

test("MCP-scoped rule is shown beside the global rule and can only be deleted from UI", async ({ page }) => {
  const { state, writes } = await settingsFixture(page);
  const base = {
    key,
    enabled: true,
    environment: null,
    no_data: "loss" as const,
    combine: "and" as const,
    group_ids: [],
    triggers: [{ window_seconds: 300, aggregate: "avg" as const, comparator: ">" as const, threshold: 1 }],
    revision: 1,
    enabled_at: 1,
    state: "ok" as const,
    loss_registered: false,
    loss_at: null,
    last_point_at: null,
    last_as_of: null,
    no_recipients: false,
  };
  state.alerts = [
    { ...base, id: "alert-global", node_scope: { kind: "all" } },
    { ...base, id: "alert-mcp", enabled: false, node_scope: { kind: "relay", id: 4 } },
  ];
  await page.goto("/alerts");
  const rules = page.locator(".alert-rule-list");
  await expect(rules.locator(".alert-rule-row")).toHaveCount(2);
  await expect(rules.getByText("Создано через MCP · relay 4 · изменяется через MCP", { exact: true })).toBeVisible();
  await expect(rules.getByRole("button", { name: "Изменить", exact: true })).toHaveCount(1);
  await expect(rules.getByRole("button", { name: "Удалить", exact: true })).toHaveCount(1);

  page.once("dialog", (dialog) => dialog.dismiss());
  await rules.getByRole("button", { name: "Удалить", exact: true }).click();
  await expect(rules.locator(".alert-rule-row")).toHaveCount(2);
  page.once("dialog", (dialog) => dialog.accept());
  await rules.getByRole("button", { name: "Удалить", exact: true }).click();
  await expect(rules.locator(".alert-rule-row")).toHaveCount(1);
  expect(writes).toContainEqual({ method: "DELETE", path: "/api/alerts/rules/alert-mcp", body: null });
  state.alerts = [
    { ...base, id: "alert-global", node_scope: { kind: "all" } },
    { ...base, id: "alert-mcp", enabled: false, node_scope: { kind: "relay", id: 4 } },
  ];

  await page.goto("/alerts?tab=rules&type=metric&server=server-a&service=billing&name=requests&origin=counter");
  const editor = page.getByRole("region", { name: "Редактор алерта метрики" });
  await expect(editor.getByText("Создано через MCP · relay 4", { exact: true })).toBeVisible();
});

test("metric picker exposes technical key, server, service, and description", async ({ page }) => {
  const metric = {
    server: "relay-eu-1", service: "network", name: "http_5xx", origin: "counter",
    first_seen: "2026-11-16T09:00:00Z", last_seen: "2026-11-16T09:29:00Z", environments: ["prod"],
    definition: {
      server: "relay-eu-1", id: "http_5xx", service: "network", name: "http_5xx", title: "Ошибки HTTP",
      description: "Ответы сервера с кодом 5xx", unit: "requests", kind: "delta", aggregation: "sum",
      enabled: true, rule: null, updated_at: "2026-11-16T09:00:00Z",
    },
  };
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/alerts") return route.fulfill({ json: initialSettings() });
    if (path === "/api/log-alerts") return route.fulfill({ json: { alerts: [], core_status: "available" } });
    if (path === "/api/series") return route.fulfill({ json: { metrics: [metric] } });
    if (path === "/api/alerts/definition") return route.fulfill({ json: { alert: null } });
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/alerts");
  await page.getByRole("button", { name: "Новый алерт" }).click();
  await page.getByRole("button", { name: "Алерт метрики" }).click();
  const search = page.getByRole("combobox", { name: "Метрика" });
  await search.fill("Ошибки HTTP");
  const option = page.getByRole("listbox").getByRole("option");
  await expect(option).toContainText("http_5xx");
  await expect(option).toContainText("relay-eu-1 · network");
  await expect(option).toContainText("Ответы сервера с кодом 5xx");
  await option.click();
  await expect(page.getByRole("region", { name: "Редактор алерта метрики" })).toBeVisible();
});

test("metric detail opens the selected rule in the unified alerts editor", async ({ page }) => {
  const metric = {
    server: "server-a", service: "billing", name: "requests", origin: "counter",
    first_seen: null, last_seen: null, environments: [], definition: null,
  };
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/series/lookup") return route.fulfill({ json: { metrics: [metric] } });
    if (path === "/api/series/query") return route.fulfill({ json: { kind: "delta", aggregation: "sum", unit: "requests", buckets: [], series: [] } });
    if (path === "/api/series") return route.fulfill({ json: { metrics: [metric] } });
    if (path === "/api/alerts") return route.fulfill({ json: initialSettings() });
    if (path === "/api/log-alerts") return route.fulfill({ json: { alerts: [], core_status: "available" } });
    if (path === "/api/alerts/definition") return route.fulfill({ json: { alert: null } });
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/metrics/detail?server=server-a&service=billing&name=requests&origin=counter");
  await page.getByRole("button", { name: "Алерт" }).click();
  await expect(page).toHaveURL(/\/alerts\?tab=rules&type=metric/);
  await expect(page.getByRole("region", { name: "Редактор алерта метрики" })).toBeVisible();
});

test("token validation error is visible and does not enable global alerts", async ({ page }) => {
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/account/passkeys") return route.fulfill({ json: { passkeys: [] } });
    if (path === "/api/alerts" && request.method() === "GET")
      return route.fulfill({ json: initialSettings() });
    if (path === "/api/log-alerts") return route.fulfill({ json: { alerts: [], core_status: "available" } });
    if (path === "/api/series") return route.fulfill({ json: { metrics: [] } });
    if (path === "/api/alerts/token")
      return route.fulfill({ status: 400, json: { error: "invalid_bot_token" } });
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/alerts?tab=telegram");
  const telegram = page.getByRole("region", { name: "Telegram" });
  await telegram.getByLabel("Токен бота").fill("bad-token");
  await telegram.getByRole("button", { name: "Проверить и сохранить токен" }).click();
  await expect(telegram.getByRole("alert")).toHaveText("Telegram отклонил токен. Проверьте его и повторите");
  await expect(telegram.getByLabel("Включить отправку алертов")).toBeDisabled();
});

test("renames a metric alert from its tile without opening the editor", async ({ page }) => {
  const { state } = await settingsFixture(page);
  const puts: unknown[] = [];
  await page.route("**/api/alerts/definition", async (route) => {
    if (route.request().method() !== "PUT") return route.fallback();
    const body = route.request().postDataJSON() as { title: string };
    puts.push(body);
    state.alerts[0] = { ...state.alerts[0], title: body.title };
    return route.fulfill({ json: { id: state.alerts[0].id, alert: state.alerts[0] } });
  });
  state.alerts = [{
    id: "alert-global", key, enabled: true, environment: null, no_data: "loss", combine: "and", group_ids: [],
    triggers: [{ window_seconds: 300, aggregate: "avg", comparator: ">", threshold: 1 }], revision: 1, enabled_at: 1,
    state: "ok", loss_registered: false, loss_at: null, last_point_at: null, last_as_of: null, no_recipients: false,
    node_scope: { kind: "all" },
  }];
  await page.goto("/alerts");
  const tile = page.locator(".alert-rule-row");
  await tile.hover();
  await tile.getByRole("button", { name: "Переименовать" }).click();
  await tile.getByLabel("Публичное имя алерта").fill("Ошибки оплаты");
  await tile.getByRole("button", { name: "Сохранить" }).click();
  await expect(tile.getByText("Ошибки оплаты", { exact: true })).toBeVisible();
  expect(puts).toEqual([expect.objectContaining({ key, title: "Ошибки оплаты", enabled: true })]);
});
