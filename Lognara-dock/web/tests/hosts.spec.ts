import { expect, test } from "@playwright/test";

const createdAt = "2026-11-15T10:00:00Z";
const hostToken = `lnh_${"c".repeat(64)}`;

test("hosts are managed separately from relays and rotate their token", async ({ page }) => {
  const relays = [
    { id: 1, name: "relay-eu", kind: "relay", created_at: createdAt },
    { id: 2, name: "mail-01", kind: "host", created_at: createdAt },
  ];
  const agents = [{ id: 1, name: "api", relay_id: 1, created_at: createdAt }, { id: 2, name: "mail-01", relay_id: 2, created_at: createdAt }];
  const writes: string[] = [];
  page.on("dialog", (dialog) => dialog.accept());
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/relays") return route.fulfill({ json: { relays } });
    if (path === "/api/agents") return route.fulfill({ json: { agents } });
    writes.push(`${request.method()} ${path}`);
    if (path === "/api/hosts") {
      const relay = { id: 3, name: request.postDataJSON().name, kind: "host", created_at: createdAt };
      relays.push(relay);
      return route.fulfill({ status: 201, json: { relay, agent: { id: 3, name: relay.name, relay_id: 3, created_at: createdAt }, token: hostToken } });
    }
    if (path === "/api/hosts/2/rotate-token") {
      return route.fulfill({ json: { relay: relays[1], agent: agents[1], token: hostToken } });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/access");
  await expect(page.getByLabel("Хост mail-01")).toBeVisible();
  await expect(page.getByLabel("Relay mail-01")).toHaveCount(0);
  await expect(page.getByLabel("Relay агента").locator("option")).toHaveText(["relay-eu"]);
  await expect(page.getByText("Relay: 1 · Агенты: 1")).toBeVisible();

  await page.getByLabel("Имя хоста").fill("web-02");
  await page.getByRole("button", { name: "Добавить хост" }).click();
  await expect(page.getByLabel("Строка для .env")).toHaveValue(`LOGNARA_HOST_TOKEN=${hostToken}`);
  await page.getByRole("button", { name: "Я сохранил токен" }).click();

  await page.getByLabel("Хост mail-01").getByRole("button", { name: "Сменить токен" }).click();
  await expect(page.getByText("Токен хоста «mail-01» перевыпущен.")).toBeVisible();
  expect(writes).toEqual(["POST /api/hosts", "POST /api/hosts/2/rotate-token"]);
});
