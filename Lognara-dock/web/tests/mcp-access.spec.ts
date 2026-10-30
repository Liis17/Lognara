import { expect, test, type Page } from "@playwright/test";

const secret = `lnm_${"s".repeat(43)}`;

async function accessFixture(page: Page) {
  const state = {
    tokens: [{ id: "token-1", name: "Old laptop", created_at: 1_798_761_600_000, expires_at: 1_801_353_600_000, last_used_at: null }],
    grants: [{ id: "grant-1", client_name: "ChatGPT", scope: "lognara.read", created_at: 1_798_761_600_000, last_used_at: 1_798_800_000_000 }],
    issued: 0,
  };
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/mcp/access") return route.fulfill({ json: { enabled: true, url: "https://dock.example.com/mcp" } });
    if (path === "/api/mcp/personal-tokens" && request.method() === "GET") return route.fulfill({ json: { tokens: state.tokens } });
    if (path === "/api/mcp/grants" && request.method() === "GET") return route.fulfill({ json: { grants: state.grants } });
    if (path === "/api/mcp/personal-tokens" && request.method() === "POST") {
      state.issued += 1;
      const personal_token = { id: "token-new", name: request.postDataJSON().name, created_at: 1_798_761_600_000, expires_at: 1_801_353_600_000, last_used_at: null };
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
  await page.goto("/mcp-access");
  await expect(page.getByRole("heading", { name: "MCP-доступ", exact: true })).toBeVisible();
  return state;
}

test("issues one-time PAT, copies it explicitly, lists expiry and revokes credentials", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const state = await accessFixture(page);
  await expect(page.getByText("https://dock.example.com/mcp")).toBeVisible();
  await expect(page.getByText("Истекает", { exact: false })).toBeVisible();
  await expect(page.getByText("Последнее использование: не использовался", { exact: true })).toBeVisible();

  await page.getByLabel("Название", { exact: true }).fill("Codex workstation");
  await page.getByRole("button", { name: "Выпустить токен", exact: true }).click();
  const issued = page.getByRole("region", { name: "Выданный токен" });
  await expect(issued.getByLabel("Скопируйте секрет сейчас")).toHaveValue(secret);
  expect(state.issued).toBe(1);
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
      expect(request.postDataJSON()).toEqual({ allow: true });
      return route.fulfill({ json: { redirect_to: "https://chatgpt.com/connector_platform_oauth_redirect?code=code-1&state=state-1&iss=https%3A%2F%2Fdock.example.com" } });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.route("https://chatgpt.com/connector_platform_oauth_redirect?**", (route) => route.fulfill({ contentType: "text/html", body: "ChatGPT callback" }));
  await page.goto("/oauth/consent?request=pending-1");
  await expect(page.getByRole("heading", { name: "Разрешить подключение?", exact: true })).toBeVisible();
  await expect(page.getByText("lognara.read", { exact: true })).toBeVisible();
  await expect(page.getByText(/текст логов не маскируется/u)).toBeVisible();
  await page.getByRole("button", { name: "Разрешить", exact: true }).click();
  await expect(page).toHaveURL(/chatgpt\.com\/connector_platform_oauth_redirect\?code=code-1/u);
});

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
