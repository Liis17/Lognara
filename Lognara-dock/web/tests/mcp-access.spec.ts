import { expect, test, type Page } from "@playwright/test";

const secret = `lnm_${"s".repeat(43)}`;
const nodes = {
  relays: [{ id: 4, name: "Edge", created_at: "2026-01-01T00:00:00Z" }, { id: 5, name: "Core", created_at: "2026-01-01T00:00:00Z" }],
  agents: [
    { id: 9, name: "api-1", relay_id: 4, created_at: "2026-01-01T00:00:00Z" },
    { id: 12, name: "db-1", relay_id: 5, created_at: "2026-01-01T00:00:00Z" },
  ],
};

async function accessFixture(page: Page) {
  const everywhere = { logs: true, metrics: true, alerts_write: false, node_scope: { kind: "all" }, node_name: null };
  const state = {
    tokens: [{ id: "token-1", name: "Old laptop", created_at: 1_798_761_600_000, expires_at: 1_801_353_600_000, last_used_at: null, ...everywhere }],
    grants: [{ id: "grant-1", client_name: "ChatGPT", scope: "lognara.read", created_at: 1_798_761_600_000, last_used_at: 1_798_800_000_000, ...everywhere }],
    issued: 0,
    bodies: [] as unknown[],
  };
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/mcp/access") return route.fulfill({ json: { enabled: true, url: "https://dock.example.com/mcp" } });
    if (path === "/api/relays") return route.fulfill({ json: { relays: nodes.relays } });
    if (path === "/api/agents") return route.fulfill({ json: { agents: nodes.agents } });
    if (path === "/api/mcp/personal-tokens" && request.method() === "GET") return route.fulfill({ json: { tokens: state.tokens } });
    if (path === "/api/mcp/grants" && request.method() === "GET") return route.fulfill({ json: { grants: state.grants } });
    if (path === "/api/mcp/personal-tokens" && request.method() === "POST") {
      state.issued += 1;
      const body = request.postDataJSON();
      state.bodies.push(body);
      const personal_token = {
        id: "token-new",
        name: body.name,
        created_at: 1_798_761_600_000,
        expires_at: body.never_expires ? null : 1_801_353_600_000,
        last_used_at: null,
        logs: body.logs,
        metrics: body.metrics,
        alerts_write: body.alerts_write,
        node_scope: body.node_scope,
        node_name: body.node_scope.kind === "relay" ? "Edge" : body.node_scope.kind === "agent" ? "api-1" : null,
      };
      state.tokens.unshift(personal_token);
      return route.fulfill({ status: 201, json: { token: secret, personal_token } });
    }
    if (request.method() === "DELETE" && path.startsWith("/api/mcp/personal-tokens/")) {
      state.tokens = state.tokens.filter((token) => path !== `/api/mcp/personal-tokens/${token.id}`);
      return route.fulfill({ status: 204 });
    }
    if (request.method() === "DELETE" && path.startsWith("/api/mcp/grants/")) {
      state.grants = state.grants.filter((grant) => path !== `/api/mcp/grants/${grant.id}`);
      return route.fulfill({ status: 204 });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/mcp-access?source=test#tokens");
  await expect(page).toHaveURL(/\/settings\/mcp\?source=test#tokens$/u);
  await expect(page.getByRole("heading", { name: "MCP-доступ", exact: true })).toBeVisible();
  return state;
}

async function chooseCombobox(page: Page, label: string, option: string) {
  const input = page.getByRole("combobox", { name: label, exact: true });
  await input.fill(option);
  await page.getByRole("option", { name: new RegExp(option, "u") }).click();
}

test("settings use URL tabs and a horizontal sidebar on narrow screens", async ({ page }) => {
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/account/passkeys") return route.fulfill({ json: { passkeys: [] } });
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/settings");
  await expect(page).toHaveURL(/\/settings\/account$/u);

  const sidebar = page.getByRole("navigation", { name: "Настройки", exact: true });
  await expect(sidebar.getByRole("link", { name: "Аккаунт", exact: true })).toHaveAttribute("aria-current", "page");
  expect(await sidebar.evaluate((element) => getComputedStyle(element).display)).toBe("flex");
  await sidebar.getByRole("link", { name: "Вход и passkey", exact: true }).click();
  await expect(page).toHaveURL(/\/settings\/passkeys$/u);
  await expect(page.getByRole("heading", { name: "Вход и passkey", exact: true })).toBeVisible();
});

test("issues one-time PAT, copies it explicitly, lists expiry and revokes credentials", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const state = await accessFixture(page);
  await expect(page.getByText("https://dock.example.com/mcp")).toBeVisible();
  const meta = page.locator(".mcp-record").filter({ hasText: "Old laptop" }).locator(".mcp-record-meta");
  await expect(meta).toHaveText(/^Истекает через \d+ (день|дня|дней) · не использовался$/u);
  await expect(meta).toHaveAttribute("title", /Последнее использование: не использовался/u);

  await page.getByLabel("Название", { exact: true }).fill("Codex workstation");
  await page.getByRole("button", { name: "Выпустить токен", exact: true }).click();
  const issued = page.getByRole("region", { name: "Выданный токен" });
  await expect(issued.getByLabel("Скопируйте секрет сейчас")).toHaveValue(secret);
  expect(state.issued).toBe(1);
  expect(state.bodies).toEqual([{ name: "Codex workstation", logs: true, metrics: true, alerts_write: false, node_scope: { kind: "all" }, expires_in_days: 30 }]);
  expect(await page.evaluate(() => localStorage.length)).toBe(0);
  await issued.getByRole("button", { name: "Скопировать токен", exact: true }).click();
  await expect(issued.getByRole("button", { name: "Скопировано", exact: true })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(secret);

  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Отозвать", exact: true }).first().click();
  await expect(page.getByText("Codex workstation", { exact: true })).toHaveCount(0);
  expect(state.tokens).toHaveLength(1);

  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Отозвать", exact: true }).last().click();
  await expect(page.getByText("ChatGPT", { exact: true })).toHaveCount(0);
  expect(state.grants).toHaveLength(0);
});

test("OAuth consent names the read scope and returns only to the validated ChatGPT callback", async ({ page }) => {
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/oauth/consent/pending-1" && request.method() === "GET") {
      return route.fulfill({ json: { client_name: "ChatGPT", scope: "lognara.read", resource: "https://dock.example.com/mcp" } });
    }
    if (path === "/api/oauth/consent/pending-1" && request.method() === "POST") {
      expect(request.postDataJSON()).toEqual({ allow: true, logs: true, metrics: true, alerts_write: false, node_scope: { kind: "all" } });
      return route.fulfill({ json: { redirect_to: "https://chatgpt.com/connector_platform_oauth_redirect?code=code-1&state=state-1&iss=https%3A%2F%2Fdock.example.com" } });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.route("https://chatgpt.com/connector_platform_oauth_redirect?**", (route) => route.fulfill({ contentType: "text/html", body: "ChatGPT callback" }));
  await page.goto("/oauth/consent?request=pending-1");
  await expect(page.getByRole("heading", { name: "Разрешить подключение?", exact: true })).toBeVisible();
  await expect(page.getByText("lognara.read", { exact: true })).toBeVisible();
  await expect(page.getByText(/Текст логов не маскируется/u)).toBeVisible();
  await page.getByRole("button", { name: "Разрешить", exact: true }).click();
  await expect(page).toHaveURL(/chatgpt\.com\/connector_platform_oauth_redirect\?code=code-1/u);
});

test("token with alerts write scoped to a relay is listed with its rights and node", async ({ page }) => {
  const state = await accessFixture(page);
  await page.getByLabel("Название", { exact: true }).fill("Alert bot");
  const alerts = page.getByRole("checkbox", { name: "Управление алертами логов и метрик", exact: true });
  await alerts.check();
  await page.getByRole("radio", { name: "Relay и его агенты", exact: true }).check();
  await chooseCombobox(page, "Relay", "Edge");
  await page.getByRole("combobox", { name: "Срок действия", exact: true }).selectOption("90");
  await page.getByRole("button", { name: "Выпустить токен", exact: true }).click();
  await expect(page.getByRole("region", { name: "Выданный токен" })).toBeVisible();
  expect(state.bodies).toEqual([{ name: "Alert bot", logs: true, metrics: true, alerts_write: true, node_scope: { kind: "relay", id: 4 }, expires_in_days: 90 }]);
  const relayToken = page.locator(".mcp-record").filter({ hasText: "Alert bot" });
  await expect(relayToken.getByText("Relay и агенты · Edge", { exact: true })).toBeVisible();
  await expect(relayToken.getByText("Управление алертами", { exact: true })).toBeVisible();
});

test("alert rights require data access and an agent is chosen only inside its relay", async ({ page }) => {
  const state = await accessFixture(page);
  const metrics = page.getByRole("checkbox", { name: "Метрики", exact: true });
  const logs = page.getByRole("checkbox", { name: "Логи", exact: true });
  const alerts = page.getByRole("checkbox", { name: "Управление алертами логов и метрик", exact: true });
  await alerts.check();
  await metrics.uncheck();
  await expect(alerts).toBeEnabled();
  await expect(alerts).toBeChecked();
  await logs.uncheck();
  await expect(alerts).toBeDisabled();
  await expect(alerts).not.toBeChecked();
  await metrics.check();
  await expect(alerts).toBeEnabled();
  await expect(alerts).not.toBeChecked();
  await logs.check();

  await page.getByLabel("Название", { exact: true }).fill("Agent token");
  await page.getByRole("radio", { name: "Конкретный агент", exact: true }).check();
  const agent = page.getByLabel("Агент", { exact: true });
  await expect(agent).toBeDisabled();
  await chooseCombobox(page, "Relay", "Edge");
  await agent.fill("");
  await expect(page.getByRole("option", { name: /api-1/u })).toBeVisible();
  await chooseCombobox(page, "Агент", "api-1");
  await page.getByRole("button", { name: "Выпустить токен", exact: true }).click();
  await expect(page.getByRole("region", { name: "Выданный токен" })).toBeVisible();
  expect(state.bodies).toEqual([{ name: "Agent token", logs: true, metrics: true, alerts_write: false, node_scope: { kind: "agent", id: 9 }, expires_in_days: 30 }]);
  const agentToken = page.locator(".mcp-record").filter({ hasText: "Agent token" });
  await expect(agentToken.getByText("Агент · api-1", { exact: true })).toBeVisible();
});

test("issues a log-only writer without granting metric alert tools", async ({ page }) => {
  const state = await accessFixture(page);
  await expect(page.getByText("Логовые алерты доступны при чтении логов, алерты метрик — при чтении метрик.", { exact: true })).toBeVisible();
  await page.getByLabel("Название", { exact: true }).fill("Log alert bot");
  await page.getByRole("checkbox", { name: "Метрики", exact: true }).uncheck();
  await page.getByRole("checkbox", { name: "Управление алертами логов и метрик", exact: true }).check();
  await page.getByRole("button", { name: "Выпустить токен", exact: true }).click();
  await expect(page.getByRole("region", { name: "Выданный токен" })).toBeVisible();
  expect(state.bodies).toEqual([{ name: "Log alert bot", logs: true, metrics: false, alerts_write: true, node_scope: { kind: "all" }, expires_in_days: 30 }]);
  const logToken = page.locator(".mcp-record").filter({ hasText: "Log alert bot" });
  await expect(logToken.getByText("Логи", { exact: true })).toBeVisible();
  await expect(logToken.getByText("Все узлы", { exact: true })).toBeVisible();
});

test("changing the relay drops an agent chosen under the previous relay", async ({ page }) => {
  const state = await accessFixture(page);
  await page.getByLabel("Название", { exact: true }).fill("Stale agent");
  await page.getByRole("radio", { name: "Конкретный агент", exact: true }).check();
  await chooseCombobox(page, "Relay", "Edge");
  await chooseCombobox(page, "Агент", "api-1");
  await page.getByRole("radio", { name: "Relay и его агенты", exact: true }).check();
  await chooseCombobox(page, "Relay", "Core");
  await page.getByRole("radio", { name: "Конкретный агент", exact: true }).check();
  await expect(page.getByRole("combobox", { name: "Агент", exact: true })).toHaveValue("Выберите агента");
  await expect(page.getByRole("button", { name: "Выпустить токен", exact: true })).toBeDisabled();
  expect(state.bodies).toEqual([]);
});

test("consent offers write only when the client asked for it and posts the chosen rights", async ({ page }) => {
  let posted: unknown = null;
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/relays") return route.fulfill({ json: { relays: nodes.relays } });
    if (path === "/api/agents") return route.fulfill({ json: { agents: nodes.agents } });
    if (path === "/api/oauth/consent/pending-2" && request.method() === "GET") {
      return route.fulfill({ json: { client_name: "Codex", scope: "lognara.read lognara.write", resource: "https://dock.example.com/mcp" } });
    }
    if (path === "/api/oauth/consent/pending-2" && request.method() === "POST") {
      posted = request.postDataJSON();
      return route.fulfill({ json: { redirect_to: "https://chatgpt.com/connector_platform_oauth_redirect?code=code-2&state=state-2&iss=https%3A%2F%2Fdock.example.com" } });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.route("https://chatgpt.com/connector_platform_oauth_redirect?**", (route) => route.fulfill({ contentType: "text/html", body: "ChatGPT callback" }));
  await page.goto("/oauth/consent?request=pending-2");
  await expect(page.getByText("Клиент просит доступ на запись", { exact: false })).toBeVisible();
  await expect(page.getByText(/логовые алерты при чтении логов и алерты метрик при чтении метрик/u)).toBeVisible();
  const alerts = page.getByRole("checkbox", { name: "Управление алертами логов и метрик", exact: true });
  await expect(alerts).not.toBeChecked();
  await alerts.check();
  await page.getByRole("radio", { name: "Конкретный агент", exact: true }).check();
  await page.getByLabel("Relay", { exact: true }).selectOption({ label: "Edge" });
  await page.getByLabel("Агент", { exact: true }).selectOption({ label: "api-1" });
  await page.getByRole("button", { name: "Разрешить", exact: true }).click();
  await expect(page).toHaveURL(/code=code-2/u);
  expect(posted).toEqual({ allow: true, logs: true, metrics: true, alerts_write: true, node_scope: { kind: "agent", id: 9 } });
});

test("an active session resumes OAuth from the login return path", async ({ page }) => {
  await page.route("**/api/**", (route) => route.fulfill({ json: { username: "liis", passkeys: 0 } }));
  await page.route("**/oauth/authorize?**", (route) => route.fulfill({ contentType: "text/html", body: "Authorization endpoint" }));
  await page.goto("/login?return_to=%2Foauth%2Fauthorize%3Fresponse_type%3Dcode%26state%3Dstate-1");
  await expect(page.getByText("Authorization endpoint", { exact: true })).toBeVisible();
  await expect(page).toHaveURL(/\/oauth\/authorize\?response_type=code&state=state-1/u);
});

for (const returnTo of ["https://evil.example/oauth/authorize?state=1", "//evil.example/oauth/authorize?state=1", "/logs?state=1", ""]) {
  test(`an active login rejects a non-OAuth return path: ${returnTo}`, async ({ page }) => {
    await page.route("**/api/**", (route) => route.fulfill({ json: { username: "liis", passkeys: 0 } }));
    await page.goto(`/login?return_to=${encodeURIComponent(returnTo)}`);
    await expect(page).toHaveURL((url) =>
      url.origin === new URL(test.info().project.use.baseURL!).origin && url.pathname === "/logs",
    );
  });
}

test("login preserves a local OAuth authorization return path", async ({ page }) => {
  let authenticated = false;
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/me") return route.fulfill(authenticated ? { json: { username: "liis", passkeys: 0 } } : { status: 401, json: { error: "unauthorized" } });
    if (path === "/api/auth/login") { authenticated = true; return route.fulfill({ json: {} }); }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.route("**/oauth/authorize?**", (route) => route.fulfill({ contentType: "text/html", body: "Authorization endpoint" }));
  await page.goto("/login?return_to=%2Foauth%2Fauthorize%3Fresponse_type%3Dcode%26state%3Dstate-1");
  await page.getByLabel("Логин", { exact: true }).fill("liis");
  await page.getByLabel("Пароль", { exact: true }).fill("test-password");
  await page.getByRole("button", { name: "Войти", exact: true }).click();
  await expect(page).toHaveURL(/\/oauth\/authorize\?response_type=code&state=state-1/u);
  await expect(page.getByText("Authorization endpoint", { exact: true })).toBeVisible();
});

test("issues a never-expiring token and lists it as unlimited", async ({ page }) => {
  const state = await accessFixture(page);
  await page.getByLabel("Название", { exact: true }).fill("CI bot");
  await page.getByRole("combobox", { name: "Срок действия", exact: true }).selectOption({ label: "Без ограничения" });
  await expect(page.getByText("Бессрочный токен действует, пока его не отзовут.")).toBeVisible();
  await page.getByRole("button", { name: "Выпустить токен", exact: true }).click();
  await expect(page.getByRole("region", { name: "Выданный токен" })).toBeVisible();
  expect(state.bodies).toEqual([{ name: "CI bot", logs: true, metrics: true, alerts_write: false, node_scope: { kind: "all" }, never_expires: true }]);
  const meta = page.locator(".mcp-record").filter({ hasText: "CI bot" }).locator(".mcp-record-meta");
  await expect(meta).toHaveText("Бессрочный · не использовался");
  await expect(meta).toHaveAttribute("title", /Срок действия не ограничен/u);
});
