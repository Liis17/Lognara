import { expect, test, type Page } from "@playwright/test";
const metric = {
  server: "server-a",
  service: "billing",
  name: "requests",
  origin: "counter",
  first_seen: "2026-01-01T00:00:00Z",
  last_seen: "2026-10-15T12:00:00Z",
  definition: null,
  environments: ["prod", "stage"],
};
const bundle = {
  format: "lognara-metrics",
  version: 1,
  metrics: [
    {
      name: "requests",
      title: "Запросы",
      kind: "delta",
      rule: { contains: "done", pattern: "done" },
    },
  ],
};
async function fixture(page: Page, conflict = false) {
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/me")
      return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/series")
      return route.fulfill({
        json: { metrics: [metric, { ...metric, server: "server-b" }] },
      });
    if (path === "/api/series/sources")
      return route.fulfill({
        json: {
          sources: [
            { server: "server-a", service: "billing" },
            { server: "server-b", service: "nginx" },
          ],
        },
      });
    if (path === "/api/series/query")
      return route.fulfill({
        json: {
          kind: "delta",
          aggregation: "sum",
          unit: "requests",
          buckets: ["2026-10-15T11:00:00Z", "2026-10-15T12:00:00Z"],
          series: [{ label: "", values: [10, 20] }],
        },
      });
    if (path === "/api/series/preview")
      return route.fulfill({ json: { scanned: 2, matched: 0, samples: [] } });
    if (path === "/api/series/definitions")
      return route.fulfill({ status: 201, json: { id: "new-definition" } });
    if (path === "/api/series/import/check")
      return route.fulfill({
        json: {
          revision: "revision-1",
          errors: [],
          conflicts: conflict
            ? [
                {
                  index: 0,
                  existing: { ...metric, id: "existing", title: "Запросы" },
                },
              ]
            : [],
          metrics: route.request().postDataJSON().bundle.metrics,
        },
      });
    if (path === "/api/series/import/apply")
      return route.fulfill({ json: { created: 1, replaced: 0, skipped: 0 } });
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
}
async function create(page: Page, server = "server-a", service = "billing") {
  await page
    .getByRole("button", { name: "Новая метрика", exact: true })
    .click();
  await page.getByLabel("Выберите сервер").selectOption(server);
  await page.getByLabel("Выберите сервис").selectOption(service);
}
test("server tiles link to reloadable detail pages and preserve query scope", async ({
  page,
}, testInfo) => {
  await fixture(page);
  await page.goto("/metrics");
  await expect(
    page.getByRole("heading", { name: "server-a", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "server-b", exact: true }),
  ).toBeVisible();
  await expect(page.locator(".metric-tile canvas")).toHaveCount(2);
  await page.screenshot({
    path: testInfo.outputPath("metrics-overview.png"),
    fullPage: true,
  });
  await page.locator(".metric-tile").first().click();
  await expect(page).toHaveURL(
    /\/metrics\/detail\?server=server-a&service=billing&name=requests&origin=counter/,
  );
  await page.reload();
  await expect(page.locator(".chart-panel canvas")).toBeVisible();
  const filtered = page.waitForRequest(
    (r) =>
      r.url().endsWith("/api/series/query") &&
      r.postDataJSON().environment === "prod",
  );
  await page.getByLabel("Окружение", { exact: true }).selectOption("prod");
  expect((await filtered).postDataJSON()).toMatchObject({
    server: "server-a",
    service: "billing",
    name: "requests",
    origin: "counter",
    environment: "prod",
  });
  const split = page.waitForRequest(
    (r) =>
      r.url().endsWith("/api/series/query") &&
      r.postDataJSON().split === "instance",
  );
  await page.getByLabel("Разбиение").selectOption("instance");
  await split;
  await page.getByRole("button", { name: "Настроить" }).click();
  const dialog = page.getByRole("dialog", { name: "Настройка метрики" });
  await expect(dialog.getByLabel("Сервер", { exact: true })).toHaveValue(
    "server-a",
  );
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Настроить" })).toBeFocused();
});
test("manual creation uses dependent selects and scoped editor on mobile", async ({
  page,
}, testInfo) => {
  await fixture(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/metrics");
  await page
    .getByRole("button", { name: "Новая метрика", exact: true })
    .click();
  await expect(page.getByLabel("Выберите сервис")).toBeDisabled();
  await page.getByLabel("Выберите сервер").selectOption("server-b");
  await expect(
    page.getByLabel("Выберите сервис").getByRole("option", { name: "billing" }),
  ).toHaveCount(0);
  await page.getByLabel("Выберите сервис").selectOption("nginx");
  await page.getByLabel("Выберите сервер").selectOption("server-a");
  await expect(page.getByLabel("Выберите сервис")).toHaveValue("");
  await page.getByLabel("Выберите сервис").selectOption("billing");
  await page.getByLabel("Имя", { exact: true }).fill("latency");
  await page.getByLabel("Подстрока для отбора строк").fill("duration=");
  await page.getByLabel("Регулярное выражение").fill("duration=(?<ms>\\d+)");
  await page.getByLabel(/^Группа значения/).fill("ms");
  const preview = page.waitForRequest((r) =>
    r.url().endsWith("/api/series/preview"),
  );
  await page.getByRole("button", { name: "Проверить", exact: true }).click();
  expect((await preview).postDataJSON()).toMatchObject({
    server: "server-a",
    service: "billing",
    minutes: 60,
  });
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth),
  ).toBeLessThanOrEqual(390);
  await page.screenshot({
    path: testInfo.outputPath("metrics-create-mobile.png"),
    fullPage: true,
  });
  const save = page.waitForRequest((r) =>
    r.url().endsWith("/api/series/definitions"),
  );
  await page.getByRole("button", { name: "Сохранить", exact: true }).click();
  expect((await save).postDataJSON()).toMatchObject({
    server: "server-a",
    service: "billing",
    name: "latency",
    rule: { value_group: "ms" },
  });
  await expect(page.getByRole("dialog")).toHaveCount(0);
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth),
  ).toBeLessThanOrEqual(390);
});
test("JSON import resolves conflicts and sends checked revision", async ({
  page,
}) => {
  await fixture(page, true);
  await page.goto("/metrics");
  await create(page);
  await page.getByRole("button", { name: "Импорт JSON", exact: true }).click();
  await page.getByLabel("JSON набора").fill("{bad");
  await page.getByRole("button", { name: "Проверить набор" }).click();
  await expect(page.getByRole("alert")).toBeVisible();
  await page.getByLabel("JSON набора").fill(JSON.stringify(bundle));
  await page.getByRole("button", { name: "Проверить набор" }).click();
  const dialog = page.getByRole("dialog", { name: "Метрики уже существуют" });
  await expect(dialog).toBeVisible();
  await dialog.getByLabel("Действие для requests").selectOption("rename");
  await dialog.getByLabel("Новое имя").fill("requests.v2");
  await dialog.getByRole("button", { name: "Готово" }).click();
  const apply = page.waitForRequest((r) =>
    r.url().endsWith("/api/series/import/apply"),
  );
  await page.getByRole("button", { name: "Применить набор" }).click();
  expect((await apply).postDataJSON()).toMatchObject({
    server: "server-a",
    service: "billing",
    revision: "revision-1",
    actions: [{ action: "rename", name: "requests.v2" }],
  });
  await expect(page.getByRole("status")).toContainText("Создано: 1");
});
test("nginx templates allow preview without matches", async ({ page }) => {
  await fixture(page);
  await page.goto("/metrics");
  await create(page, "server-b", "nginx");
  await page.getByRole("button", { name: "Шаблоны", exact: true }).click();
  const check = page.waitForRequest((r) =>
    r.url().endsWith("/api/series/import/check"),
  );
  await page.getByRole("button", { name: "Проверить набор" }).click();
  expect((await check).postDataJSON().bundle.metrics).toHaveLength(6);
  await page
    .getByRole("button", { name: "Проверить логи", exact: true })
    .first()
    .click();
  await expect(page.getByText(/Подходящих строк пока нет/)).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Применить набор" }),
  ).toBeEnabled();
});

test("mini charts limit concurrent requests and load more as slots free", async ({
  page,
}) => {
  await fixture(page);
  await page.route("**/api/series?group_by=server", (route) =>
    route.fulfill({
      json: {
        metrics: Array.from({ length: 6 }, (_, i) => ({
          ...metric,
          name: "requests_" + i,
        })),
      },
    }),
  );
  const pending: (() => void)[] = [];
  let started = 0,
    active = 0,
    peak = 0;
  await page.route("**/api/series/query", async (route) => {
    started++;
    active++;
    peak = Math.max(peak, active);
    await new Promise<void>((resolve) => pending.push(resolve));
    active--;
    await route.fulfill({
      json: {
        kind: "delta",
        aggregation: "sum",
        unit: "",
        buckets: [],
        series: [],
      },
    });
  });
  await page.goto("/metrics");
  await expect.poll(() => started).toBe(2);
  await page.evaluate(
    () =>
      new Promise<void>((resolve) =>
        requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
      ),
  );
  expect(started).toBe(2);
  pending.shift()!();
  await expect.poll(() => started).toBe(3);
  while (started < 6) {
    pending.shift()!();
    await expect.poll(() => pending.length).toBeGreaterThan(0);
  }
  pending.splice(0).forEach((resolve) => resolve());
  expect(peak).toBeLessThanOrEqual(2);
  await expect(
    page.getByText("За 24 часа данных нет", { exact: true }),
  ).toHaveCount(6);
});
