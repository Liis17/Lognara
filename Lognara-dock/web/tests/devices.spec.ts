import { expect, test, type BrowserContext } from "@playwright/test";

type DeviceInfo = { id: string; name: string; created_at: number; last_used_at: number | null };
type PairingStatus = "pending" | "used" | "expired" | "replaced";
type Pairing = { pairing_id: string; payload: string; expires_at: number; status: PairingStatus; listed: boolean };

async function deviceFixture(context: BrowserContext, options: { expiresInMs?: number; devices?: DeviceInfo[] } = {}) {
  const state = {
    devices: [...(options.devices ?? [])],
    pairings: new Map<string, Pairing>(),
    created: 0,
    statusReads: 0,
    revoked: [] as string[],
  };

  await context.route("**/api/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/devices" && request.method() === "GET") {
      return route.fulfill({ json: { devices: state.devices } });
    }
    if (path === "/api/devices/pairing" && request.method() === "POST") {
      for (const pairing of state.pairings.values()) {
        if (pairing.status === "pending") pairing.status = "replaced";
      }
      state.created += 1;
      const pairingId = `pairing-${state.created}`;
      const payload = Buffer.from(JSON.stringify({ v: 1, url: "https://dock.example.com", code: `manual-${state.created}` })).toString("base64url");
      const pairing: Pairing = {
        pairing_id: pairingId,
        payload,
        expires_at: Date.now() + (options.expiresInMs ?? 120_000),
        status: "pending",
        listed: false,
      };
      state.pairings.set(pairingId, pairing);
      return route.fulfill({ status: 201, json: { pairing_id: pairing.pairing_id, payload: pairing.payload, expires_at: pairing.expires_at } });
    }
    const pairingMatch = path.match(/^\/api\/devices\/pairing\/([^/]+)$/u);
    if (pairingMatch && request.method() === "GET") {
      state.statusReads += 1;
      const pairing = state.pairings.get(decodeURIComponent(pairingMatch[1]));
      if (!pairing) return route.fulfill({ status: 404, json: { error: "not_found" } });
      if (pairing.status === "used" && !pairing.listed) {
        pairing.listed = true;
        state.devices.unshift({ id: "device-1", name: "Pixel 10", created_at: Date.now(), last_used_at: null });
      }
      return route.fulfill({ json: { status: pairing.status } });
    }
    const deviceMatch = path.match(/^\/api\/devices\/([^/]+)$/u);
    if (deviceMatch && request.method() === "DELETE") {
      const deviceId = decodeURIComponent(deviceMatch[1]);
      state.revoked.push(deviceId);
      state.devices = state.devices.filter((device) => device.id !== deviceId);
      return route.fulfill({ status: 204 });
    }
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });

  return state;
}

test("opens devices settings, shows an SVG QR and countdown on mobile without storing pairing data", async ({ page, context }) => {
  const messages: string[] = [];
  page.on("console", (message) => messages.push(message.text()));
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const state = await deviceFixture(context);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/settings/devices");

  await expect(page).toHaveURL(/\/settings\/devices$/u);
  await expect(page.getByRole("navigation", { name: "Настройки" }).getByRole("link", { name: "Устройства", exact: true })).toHaveAttribute("aria-current", "page");
  await expect(page.getByRole("heading", { name: "Устройства", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Подключить устройство", exact: true }).click();

  const qr = page.locator("svg.device-qr");
  await expect(qr).toBeVisible();
  await expect(qr).toHaveAttribute("aria-label", "QR-код для подключения устройства");
  await expect(qr).toHaveAttribute("viewBox", /^0 0 \d+ \d+$/u);
  await expect(page.locator(".device-countdown")).toHaveText(/^Код действует ещё \d{2}:\d{2}$/u);
  await expect(page.getByText("Откройте приложение Lognara и отсканируйте код.", { exact: true })).toBeVisible();

  const pairing = state.pairings.get("pairing-1");
  expect(pairing).toBeDefined();
  expect(await page.evaluate(() => [localStorage.length, sessionStorage.length])).toEqual([0, 0]);
  expect(page.url()).not.toContain(pairing!.payload);
  expect(page.url()).not.toContain(pairing!.pairing_id);
  expect(messages.join("\n")).not.toContain(pairing!.payload);
  expect(messages.join("\n")).not.toContain(pairing!.pairing_id);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);

  const manualCode = page.getByRole("textbox", { name: "Код для ручного ввода", exact: true });
  await expect(manualCode).toHaveCount(0);
  await page.getByRole("button", { name: "Показать код для ручного ввода", exact: true }).click();
  await expect(manualCode).toHaveValue(pairing!.payload);
  await page.getByRole("button", { name: "Скопировать код", exact: true }).click();
  await expect(page.getByRole("button", { name: "Скопировано", exact: true })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(pairing!.payload);
});

test("shows success only after this pairing is used and refreshes the device list", async ({ page, context }) => {
  const state = await deviceFixture(context);
  await page.goto("/settings/devices");
  await page.getByRole("button", { name: "Подключить устройство", exact: true }).click();
  await expect(page.locator("svg.device-qr")).toBeVisible();
  state.pairings.get("pairing-1")!.status = "used";

  await expect(page.getByText("Устройство подключено", { exact: true })).toBeVisible({ timeout: 7_000 });
  await expect(page.locator("svg.device-qr")).toHaveCount(0);
  await expect(page.getByText("Pixel 10", { exact: true })).toBeVisible();
  await expect(page.getByText("Последнее использование не использовалось", { exact: true })).toBeVisible();
  expect(state.statusReads).toBeGreaterThan(0);
});

test("shows backend expired status and allows creating another code", async ({ page, context }) => {
  const state = await deviceFixture(context);
  await page.goto("/settings/devices");
  await page.getByRole("button", { name: "Подключить устройство", exact: true }).click();
  await expect(page.locator("svg.device-qr")).toBeVisible();
  state.pairings.get("pairing-1")!.status = "expired";

  await expect(page.getByText("Код истёк", { exact: true })).toBeVisible({ timeout: 7_000 });
  await expect(page.locator("svg.device-qr")).toHaveCount(0);
  await page.getByRole("button", { name: "Создать новый код", exact: true }).click();
  await expect(page.locator("svg.device-qr")).toBeVisible();
  expect(state.created).toBe(2);
});

test("expires locally when the countdown ends", async ({ page, context }) => {
  await deviceFixture(context, { expiresInMs: 500 });
  await page.goto("/settings/devices");
  await page.getByRole("button", { name: "Подключить устройство", exact: true }).click();
  await expect(page.locator("svg.device-qr")).toBeVisible();

  await expect(page.getByText("Код истёк", { exact: true })).toBeVisible({ timeout: 4_000 });
  await expect(page.locator("svg.device-qr")).toHaveCount(0);
});

test("reports a replaced code from another tab without showing success", async ({ context }) => {
  const state = await deviceFixture(context);
  const firstPage = await context.newPage();
  await firstPage.goto("/settings/devices");
  await firstPage.getByRole("button", { name: "Подключить устройство", exact: true }).click();
  await expect(firstPage.locator("svg.device-qr")).toBeVisible();

  const secondPage = await context.newPage();
  await secondPage.goto("/settings/devices");
  await secondPage.getByRole("button", { name: "Подключить устройство", exact: true }).click();
  await expect(secondPage.locator("svg.device-qr")).toBeVisible();

  await expect(firstPage.getByText("Код заменён в другой вкладке или окне", { exact: true })).toBeVisible({ timeout: 7_000 });
  await expect(firstPage.getByText("Устройство подключено", { exact: true })).toHaveCount(0);
  expect(state.pairings.get("pairing-1")?.status).toBe("replaced");
});

test("revokes a device after confirmation", async ({ page, context }) => {
  const state = await deviceFixture(context, {
    devices: [{ id: "device-7", name: "ThinkPad", created_at: 1_800_000_000_000, last_used_at: null }],
  });
  await page.goto("/settings/devices");
  await expect(page.getByText("ThinkPad", { exact: true })).toBeVisible();
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Отозвать", exact: true }).click();

  await expect(page.getByText("ThinkPad", { exact: true })).toHaveCount(0);
  await expect(page.getByText("Устройств нет.", { exact: true })).toBeVisible();
  expect(state.revoked).toEqual(["device-7"]);
});
