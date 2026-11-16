import { expect, test } from "@playwright/test";

test("host is one Dock child, can be renamed and has a rotatable token", async ({ page }) => {
  const createdAt = "2026-11-15T10:00:00Z";
  const relays = [{ id: 2, name: "mail-01", kind: "host", created_at: createdAt }];
  const agents = [{ id: 8, name: "mail-01", relay_id: 2, created_at: createdAt }];
  const writes: string[] = [];
  page.on("dialog", (dialog) => dialog.accept());
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/relays") return route.fulfill({ json: { relays } });
    if (path === "/api/agents") return route.fulfill({ json: { agents } });
    writes.push(`${request.method()} ${path}`);
    if (path === "/api/hosts/2" && request.method() === "PATCH") {
      const name = request.postDataJSON().name;
      relays[0].name = name;
      agents[0].name = name;
      return route.fulfill({ json: { relay: relays[0], agent: agents[0] } });
    }
    if (path === "/api/hosts/2/rotate-token") {
      return route.fulfill({ json: { relay: relays[0], agent: agents[0], token: `lnh_${"c".repeat(64)}` } });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
  await page.goto("/access");
  await expect(page.getByRole("article", { name: "Хост mail-01" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Relay mail-01" })).toHaveCount(0);
  await page.getByRole("article", { name: "Хост mail-01" }).getByRole("button", { name: "Переименовать" }).click();
  await page.getByLabel("Новое имя").fill("web-02");
  await page.getByRole("button", { name: "Сохранить" }).click();
  await expect(page.getByRole("article", { name: "Хост web-02" })).toBeVisible();
  await page.getByRole("article", { name: "Хост web-02" }).getByRole("button", { name: "Сменить токен" }).click();
  await expect(page.getByLabel("Строка для .env")).toHaveValue(`LOGNARA_HOST_TOKEN=lnh_${"c".repeat(64)}`);
  expect(writes).toEqual(["PATCH /api/hosts/2", "POST /api/hosts/2/rotate-token"]);
});
