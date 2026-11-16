import { expect, test, type Page } from "@playwright/test";

const createdAt = "2026-10-19T10:00:00Z";
const token = `lna_${"a".repeat(64)}`;
const relayToken = `lnr_${"b".repeat(64)}`;
const hostToken = `lnh_${"c".repeat(64)}`;

async function fixture(page: Page, empty = false, renameUnsupported = false) {
  const received = new Date().toISOString();
  const state = {
    relays: empty ? [] : [
      { id: 1, name: "relay-eu", kind: "relay", created_at: createdAt, heartbeat: { received_at: received } },
      { id: 2, name: "relay-empty", kind: "relay", created_at: createdAt },
      { id: 3, name: "mail-01", kind: "host", created_at: createdAt, heartbeat: { received_at: received } },
    ],
    agents: empty ? [] : [
      { id: 1, name: "api", relay_id: 1, created_at: createdAt, heartbeat: { received_at: received } },
      { id: 2, name: "worker", relay_id: 1, created_at: createdAt },
      { id: 3, name: "batch", relay_id: 2, created_at: createdAt },
      { id: 4, name: "mail-01", relay_id: 3, created_at: createdAt, heartbeat: { received_at: received } },
    ],
    writes: [] as { method: string; path: string; body: unknown }[],
  };
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (request.method() === "GET") {
      if (path === "/api/relays") return route.fulfill({ json: { relays: state.relays } });
      if (path === "/api/agents") return route.fulfill({ json: { agents: state.agents } });
    }
    const body = request.postDataJSON?.() ?? null;
    state.writes.push({ method: request.method(), path, body });
    const pathParts = path.split("/");
    const id = Number(pathParts[3]);
    if (request.method() === "POST" && path === "/api/relays") {
      const relay = { id: 4, name: body.name, kind: "relay", created_at: createdAt };
      state.relays.push(relay);
      return route.fulfill({ status: 201, json: { relay, token: relayToken } });
    }
    if (request.method() === "POST" && path === "/api/hosts") {
      const relay = { id: 4, name: body.name, kind: "host", created_at: createdAt };
      const agent = { id: 5, name: body.name, relay_id: 4, created_at: createdAt };
      state.relays.push(relay);
      state.agents.push(agent);
      return route.fulfill({ status: 201, json: { relay, agent, token: hostToken } });
    }
    if (request.method() === "POST" && path === "/api/agents") {
      const agent = { id: 5, ...body, created_at: createdAt };
      state.agents.push(agent);
      return route.fulfill({ status: 201, json: { agent, token } });
    }
    if (request.method() === "POST" && path.endsWith("/rotate-token")) {
      const host = pathParts[2] === "hosts";
      return route.fulfill({ json: host
        ? { relay: state.relays.find((relay) => relay.id === id), agent: state.agents.find((agent) => agent.relay_id === id), token: hostToken }
        : { agent: state.agents.find((agent) => agent.id === id), token } });
    }
    if (request.method() === "PATCH") {
      if (renameUnsupported) return route.fulfill({ status: 501, json: { error: "core_upgrade_required" } });
      const kind = pathParts[2];
      if (kind === "agents") {
        const agent = state.agents.find((item) => item.id === id);
        if (!agent) return route.fulfill({ status: 404, json: { error: "not_found" } });
        agent.name = body.name;
        return route.fulfill({ json: { agent } });
      }
      const relay = state.relays.find((item) => item.id === id);
      if (!relay) return route.fulfill({ status: 404, json: { error: "not_found" } });
      relay.name = body.name;
      if (kind === "hosts") {
        const agent = state.agents.find((item) => item.relay_id === id);
        if (agent) agent.name = body.name;
        return route.fulfill({ json: { relay, agent } });
      }
      return route.fulfill({ json: { relay } });
    }
    if (request.method() === "DELETE") {
      if (pathParts[2] === "agents") state.agents = state.agents.filter((agent) => agent.id !== id);
      else {
        state.relays = state.relays.filter((relay) => relay.id !== id);
        if (pathParts[2] === "hosts") state.agents = state.agents.filter((agent) => agent.relay_id !== id);
      }
      return route.fulfill({ status: 204 });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/access");
  await expect(page.getByRole("heading", { name: "Подключения" })).toBeVisible();
  return state;
}

test("shows Dock → relay → agents and Dock → host branches with heartbeat status", async ({ page }) => {
  await fixture(page);
  const tree = page.getByRole("region", { name: "Дерево подключений" });
  await expect(tree).toContainText("Dock");
  const relay = tree.getByRole("region", { name: "Relay relay-eu" });
  await expect(relay.getByRole("article", { name: "Агент api" })).toContainText("Онлайн");
  await expect(relay.getByRole("article", { name: "Агент worker" })).toContainText("Нет heartbeat");
  await expect(page.getByRole("article", { name: "Хост mail-01" })).toContainText("hwagent");
  await expect(page.getByRole("article", { name: "Хост mail-01" })).toContainText("Онлайн");
  await expect(tree.getByRole("button", { name: "Добавить relay" })).toBeVisible();
  await expect(tree.getByRole("button", { name: "Добавить хост" })).toBeVisible();
  await expect(relay.getByRole("button", { name: "Добавить агента" })).toBeVisible();
});

test("creates a relay and agent inline and shows each issued token once", async ({ page }) => {
  const state = await fixture(page, true);
  await page.getByRole("button", { name: "Добавить relay" }).click();
  await page.getByLabel("Имя relay").fill("new-relay");
  await page.getByRole("form", { name: "Создать relay" }).getByRole("button", { name: "Создать" }).click();
  await expect(page.getByLabel("Строка для .env")).toHaveValue(`LOGNARA_RELAY_CORE_TOKEN=${relayToken}`);
  await expect(page.getByRole("region", { name: "Relay new-relay" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Добавить relay" })).toBeDisabled();
  await page.getByRole("button", { name: "Я сохранил токен" }).click();
  const relay = page.getByRole("region", { name: "Relay new-relay" });
  await relay.getByRole("button", { name: "Добавить агента" }).click();
  await page.getByLabel("Имя агента").fill("new-agent");
  await relay.getByRole("form", { name: "Создать агента" }).getByRole("button", { name: "Создать" }).click();
  await expect(page.getByLabel("Строка для .env")).toHaveValue(`LOGNARA_AGENT_TOKEN=${token}`);
  await expect(relay.getByRole("article", { name: "Агент new-agent" })).toBeVisible();
  expect(state.writes.map(({ method, path, body }) => [method, path, body])).toEqual([
    ["POST", "/api/relays", { name: "new-relay" }],
    ["POST", "/api/agents", { name: "new-agent", relay_id: 4 }],
  ]);
});

test("renames relay, agent and host through their inline editors", async ({ page }) => {
  const state = await fixture(page);
  const relay = page.getByRole("region", { name: "Relay relay-eu" });
  await relay.getByRole("button", { name: "Переименовать" }).first().click();
  await relay.getByLabel("Новое имя").fill("relay-europe");
  await relay.getByRole("button", { name: "Сохранить" }).click();
  const renamedRelay = page.getByRole("region", { name: "Relay relay-europe" });
  const agent = renamedRelay.getByRole("article", { name: "Агент api" });
  await agent.getByRole("button", { name: "Переименовать" }).click();
  await agent.getByLabel("Новое имя").fill("api-v2");
  await agent.getByRole("button", { name: "Сохранить" }).click();
  const host = page.getByRole("article", { name: "Хост mail-01" });
  await host.getByRole("button", { name: "Переименовать" }).click();
  await host.getByLabel("Новое имя").fill("mail-edge");
  await host.getByRole("button", { name: "Сохранить" }).click();
  await expect(page.getByRole("article", { name: "Хост mail-edge" })).toBeVisible();
  expect(state.writes.map(({ method, path, body }) => [method, path, body])).toEqual([
    ["PATCH", "/api/relays/1", { name: "relay-europe" }],
    ["PATCH", "/api/agents/1", { name: "api-v2" }],
    ["PATCH", "/api/hosts/3", { name: "mail-edge" }],
  ]);
  expect(state.agents.find((item) => item.id === 4)?.name).toBe("mail-edge");
});

test("explains that Core must be upgraded when it does not support renaming", async ({ page }) => {
  await fixture(page, false, true);
  const relay = page.getByRole("region", { name: "Relay relay-eu" });
  await relay.getByRole("button", { name: "Переименовать" }).first().click();
  await page.getByLabel("Новое имя").fill("relay-europe");
  await page.getByRole("button", { name: "Сохранить" }).click();
  await expect(page.getByRole("alert")).toHaveText("Для переименования обновите Core");
});

test("confirms deletion and token rotation while retaining the one-time secret panel", async ({ page }) => {
  const state = await fixture(page);
  page.on("dialog", (dialog) => dialog.accept());
  const host = page.getByRole("article", { name: "Хост mail-01" });
  await host.getByRole("button", { name: "Сменить токен" }).click();
  await expect(page.getByRole("region", { name: "Выданный токен" })).toContainText("Токен хоста «mail-01» перевыпущен");
  await expect(page.getByLabel("Строка для .env")).toHaveValue(`LOGNARA_HOST_TOKEN=${hostToken}`);
  await page.getByRole("button", { name: "Я сохранил токен" }).click();
  const agent = page.getByRole("region", { name: "Relay relay-eu" }).getByRole("article", { name: "Агент worker" });
  await agent.getByRole("button", { name: "Удалить" }).click();
  await expect(agent).toHaveCount(0);
  await expect(state.writes.map(({ method, path }) => `${method} ${path}`)).toEqual([
    "POST /api/hosts/3/rotate-token", "DELETE /api/agents/2",
  ]);
});

test("host creation uses the Dock branch and returns its token", async ({ page }) => {
  await fixture(page, true);
  await page.getByRole("button", { name: "Добавить хост" }).click();
  await page.getByLabel("Имя хост").fill("mail-new");
  await page.getByRole("form", { name: "Создать хост" }).getByRole("button", { name: "Создать" }).click();
  await expect(page.getByLabel("Строка для .env")).toHaveValue(`LOGNARA_HOST_TOKEN=${hostToken}`);
  await expect(page.getByRole("article", { name: "Хост mail-new" })).toContainText("hwagent");
});
