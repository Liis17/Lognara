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
async function fixture(
  page: Page,
  conflict = false,
  alertSummary: {
    enabled: boolean;
    alerts: {
      key: Pick<typeof metric, "server" | "service" | "name" | "origin">;
      enabled: boolean;
      state: "unknown" | "ok" | "firing";
      no_recipients: boolean;
    }[];
  } = { enabled: false, alerts: [] },
) {
  let favorites = [metric, { ...metric, server: "server-b" }].map(
    ({ server, service, name, origin }) => ({ server, service, name, origin }),
  );
  await page.route("**/api/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/me")
      return route.fulfill({ json: { username: "liis", passkeys: 0 } });
    if (path === "/api/alerts/summary")
      return route.fulfill({ json: alertSummary });
    if (path === "/api/alerts")
      return route.fulfill({
        json: {
          enabled: alertSummary.enabled,
          token_configured: true,
          send_paused: false,
          send_pause_reason: null,
          core_status: "available",
          core_checked_at: null,
          core_error: null,
          recipients: [],
          groups: [],
          alerts: [],
          pending_deliveries: 0,
          permanent_errors: 0,
        },
      });
    if (path === "/api/alerts/definition")
      return route.fulfill({ json: { alert: null } });
    if (path === "/api/series/favorites") {
      if (route.request().method() === "GET")
        return route.fulfill({ json: { keys: favorites } });
      const key = route.request().postDataJSON();
      favorites = favorites.filter(
        (k) => JSON.stringify(k) !== JSON.stringify(key),
      );
      if (route.request().method() === "PUT") favorites.push(key);
      return route.fulfill({ status: 204 });
    }
    if (path === "/api/series" || path === "/api/series/lookup") {
      const url = new URL(route.request().url());
      let metrics = [metric, { ...metric, server: "server-b" }];
      if (path.endsWith("lookup")) {
        const keys = route.request().postDataJSON().keys;
        metrics = metrics.filter((m) =>
          keys.some(
            (k: typeof metric) =>
              k.server === m.server &&
              k.service === m.service &&
              k.name === m.name &&
              k.origin === m.origin,
          ),
        );
      } else {
        metrics = metrics.filter(
          (m) =>
            (!url.searchParams.has("server") ||
              m.server === url.searchParams.get("server")) &&
            (!url.searchParams.has("service") ||
              m.service === url.searchParams.get("service")),
        );
      }
      return route.fulfill({ json: { metrics } });
    }
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
  await page.locator(".metric-tile-link").first().click();
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

test("enabled alert eye appears for any metric and is muted while global alerts are paused", async ({
  page,
}) => {
  let summaryRequests = 0;
  await fixture(page);
  await page.route("**/api/alerts/summary", async (route) => {
    summaryRequests++;
    await route.fulfill({
      json: {
        enabled: false,
        alerts: [
          {
            key: {
              server: "server-a",
              service: "billing",
              name: "requests",
              origin: "counter",
            },
            enabled: true,
            state: "firing",
            no_recipients: true,
          },
        ],
      },
    });
  });
  await page.goto("/metrics");
  const eye = page.getByRole("link", {
    name: "Алерт включён в настройке, но глобально приостановлен",
  });
  await expect(eye).toBeVisible();
  await expect(eye).toHaveAttribute(
    "title",
    "Алерт приостановлен: глобальная отправка выключена",
  );
  await expect(page.locator(".metric-alert-indicator.paused")).toHaveCount(1);
  expect(summaryRequests).toBe(1);
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
  await page.route("**/api/series/favorites", (route) =>
    route.fulfill({
      json: {
        keys: Array.from({ length: 6 }, (_, i) => ({
          ...metric,
          name: "requests_" + i,
        })),
      },
    }),
  );
  await page.route("**/api/series/lookup", (route) =>
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

async function openService(
  page: Page,
  server = "server-a",
  service = "billing",
) {
  const others = page.getByRole("region", {
    name: "Остальные метрики",
    exact: true,
  });
  await others.getByRole("button", { name: "Остальные метрики" }).click();
  await others.getByRole("button", { name: server, exact: false }).click();
  await others.getByRole("button", { name: service, exact: false }).click();
  return others;
}

test("empty favorites do not fetch catalogs and stars persist without navigation", async ({
  page,
}) => {
  await fixture(page);
  const keys: (typeof metric)[] = [];
  let failSave = false;
  await page.route("**/api/series/favorites", async (route) => {
    const method = route.request().method();
    if (method === "GET") return route.fulfill({ json: { keys } });
    if (failSave)
      return route.fulfill({
        status: 503,
        json: { error: "core_unavailable" },
      });
    const key = route.request().postDataJSON();
    if (method === "PUT") keys.push(key);
    else
      keys.splice(
        keys.findIndex((k) => k.server === key.server && k.name === key.name),
        1,
      );
    return route.fulfill({ status: 204 });
  });
  const calls: string[] = [];
  page.on("request", (r) => {
    if (r.url().includes("/api/series")) calls.push(r.url());
  });
  await page.goto("/metrics");
  await expect(page.getByText(/Избранных метрик пока нет/)).toBeVisible();
  expect([...new Set(calls.map((url) => new URL(url).pathname))]).toEqual([
    "/api/series/favorites",
  ]);
  const others = await openService(page);
  await expect(others.locator(".metric-tile")).toHaveCount(1);
  const catalogs = calls.filter(
    (url) => new URL(url).pathname === "/api/series",
  );
  expect(catalogs.length).toBeGreaterThan(0);
  expect(new URL(catalogs[0]).searchParams.get("server")).toBe("server-a");
  expect(new URL(catalogs[0]).searchParams.get("service")).toBe("billing");
  const tile = others.locator(".metric-tile");
  await tile.hover();
  const star = tile.getByRole("button", { name: "Добавить в избранное" });
  await expect(star).toHaveCSS("opacity", "1");
  await star.focus();
  await page.keyboard.press("Enter");
  const favorites = page.getByRole("region", {
    name: "Избранные метрики",
    exact: true,
  });
  await expect(favorites.locator(".metric-tile")).toHaveCount(1);
  await expect(others.locator(".metric-tile")).toHaveCount(0);
  await expect(page).toHaveURL(/\/metrics$/);
  await page.reload();
  await expect(favorites.locator(".metric-tile")).toHaveCount(1);
  await expect(
    page.getByRole("button", { name: "Остальные метрики" }),
  ).toHaveAttribute("aria-expanded", "false");
  failSave = true;
  await favorites.getByRole("button", { name: "Убрать из избранного" }).click();
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(favorites.locator(".metric-tile")).toHaveCount(1);
  failSave = false;
  await favorites.getByRole("button", { name: "Убрать из избранного" }).click();
  await expect(favorites.locator(".metric-tile")).toHaveCount(0);
});

test("missing favorites stay removable without chart requests", async ({
  page,
}) => {
  await fixture(page);
  await page.route("**/api/series/lookup", (route) =>
    route.fulfill({ json: { metrics: [] } }),
  );
  let queries = 0;
  page.on("request", (r) => {
    if (r.url().endsWith("/api/series/query")) queries++;
  });
  await page.goto("/metrics");
  await expect(
    page.getByText("Метрика недоступна", { exact: true }),
  ).toHaveCount(2);
  expect(queries).toBe(0);
  await page
    .getByRole("button", { name: "Убрать из избранного" })
    .first()
    .click();
  await expect(
    page.getByText("Метрика недоступна", { exact: true }),
  ).toHaveCount(1);
});

test("large catalogs remain lazy and refresh only expanded services", async ({
  page,
}) => {
  await fixture(page);
  await page.route("**/api/series/favorites", (route) =>
    route.fulfill({ json: { keys: [] } }),
  );
  let catalogs = 0,
    queries = 0;
  await page.route("**/api/series?**", (route) => {
    catalogs++;
    const params = new URL(route.request().url()).searchParams;
    expect(params.get("server")).toBe("server-a");
    expect(params.get("service")).toBe("billing");
    return route.fulfill({
      json: {
        metrics: Array.from({ length: 1100 }, (_, i) => ({
          ...metric,
          name: "requests_" + i,
        })),
      },
    });
  });
  page.on("request", (r) => {
    if (r.url().endsWith("/api/series/query")) queries++;
  });
  await page.goto("/metrics");
  await expect(page.getByText(/Избранных метрик пока нет/)).toBeVisible();
  expect(catalogs).toBe(0);
  const others = await openService(page);
  await expect(others.locator(".metric-tile")).toHaveCount(1100);
  await expect.poll(() => queries).toBeGreaterThan(0);
  expect(queries).toBeLessThan(30);
  const beforeRefresh = catalogs;
  await page.getByRole("button", { name: "Обновить", exact: true }).click();
  await expect.poll(() => catalogs).toBe(beforeRefresh + 1);
  await expect(others.locator(".metric-tile")).toHaveCount(1100);
  await others.getByRole("button", { name: "billing", exact: false }).click();
  await expect(others.locator(".metric-tile")).toHaveCount(0);
  await page.getByRole("button", { name: "Обновить", exact: true }).click();
  await expect(page.getByText(/Избранных метрик пока нет/)).toBeVisible();
  expect(catalogs).toBe(beforeRefresh + 1);
});

test("collapsing a service aborts active charts and removes queued charts", async ({
  page,
}) => {
  await fixture(page);
  await page.route("**/api/series/favorites", (route) =>
    route.fulfill({ json: { keys: [] } }),
  );
  await page.route("**/api/series?**", (route) =>
    route.fulfill({
      json: {
        metrics: Array.from({ length: 6 }, (_, i) => ({
          ...metric,
          name: "requests_" + i,
        })),
      },
    }),
  );
  const releases: (() => void)[] = [];
  let started = 0,
    failed = 0;
  page.on("requestfailed", (r) => {
    if (r.url().endsWith("/api/series/query")) failed++;
  });
  await page.route("**/api/series/query", async (route) => {
    started++;
    await new Promise<void>((resolve) => releases.push(resolve));
    await route
      .fulfill({
        json: {
          kind: "delta",
          aggregation: "sum",
          unit: "",
          buckets: [],
          series: [],
        },
      })
      .catch(() => {});
  });
  await page.goto("/metrics");
  const others = await openService(page);
  await expect.poll(() => started).toBe(2);
  await others.getByRole("button", { name: "billing", exact: false }).click();
  await expect(others.locator(".metric-tile")).toHaveCount(0);
  await expect.poll(() => failed).toBe(2);
  releases.splice(0).forEach((resolve) => resolve());
  await page.getByRole("button", { name: "Обновить", exact: true }).click();
  await expect(page.getByText(/Избранных метрик пока нет/)).toBeVisible();
  expect(started).toBe(2);
});

for (const unit of [
  "микросекунд на обработку запроса",
  "оченьдлиннаяединицаизмерениябезпробелов".repeat(4),
  "",
]) {
  test(`chart unit wraps without overflow: ${unit.slice(0, 20) || "empty"}`, async ({
    page,
  }, testInfo) => {
    await fixture(page);
    await page.route("**/api/series/query", (route) =>
      route.fulfill({
        json: {
          kind: "gauge",
          aggregation: "avg",
          unit,
          buckets: ["2026-10-15T11:00:00Z", "2026-10-15T12:00:00Z"],
          series: [{ label: "", values: [-123456789012345, 987654321098765] }],
        },
      }),
    );
    await page.goto(
      "/metrics/detail?server=server-a&service=billing&name=requests&origin=counter",
    );
    for (const width of [390, 1280]) {
      await page.setViewportSize({ width, height: 900 });
      await expect(page.locator(".chart-panel canvas")).toBeVisible();
      const label = page.locator(".metric-chart-unit");
      if (unit) await expect(label).toHaveText(unit);
      else await expect(label).toHaveCount(0);
      await expect
        .poll(() =>
          page.evaluate(
            () => document.documentElement.scrollWidth <= window.innerWidth,
          ),
        )
        .toBe(true);
      const sizes = await page
        .locator(".metric-chart")
        .evaluate((el) => ({ width: el.clientWidth, scroll: el.scrollWidth }));
      expect(sizes.scroll).toBeLessThanOrEqual(sizes.width);
      if (unit)
        await page.screenshot({
          path: testInfo.outputPath(`unit-${width}.png`),
          fullPage: true,
        });
    }
  });
}

test("favorite stars are visible on touch devices", async ({ browser }) => {
  const context = await browser.newContext({
    hasTouch: true,
    viewport: { width: 390, height: 844 },
  });
  const page = await context.newPage();
  await fixture(page);
  await page.route("**/api/series/favorites", (route) =>
    route.fulfill({ json: { keys: [] } }),
  );
  await page.route("**/api/series?**", route => route.fulfill({ json: { metrics: [{ ...metric, definition: { enabled: true, unit: "длиннаяединицаизмерениябезпробелов".repeat(5) } }] } }));
  await page.goto("/metrics");
  const others = await openService(page);
  const star = others.getByRole("button", { name: "Добавить в избранное" });
  await expect(star).toHaveCSS("opacity", "1");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  const sizes = await others.locator(".metric-tile").evaluate(el => ({ width: el.clientWidth, scroll: el.scrollWidth }));
  expect(sizes.scroll).toBeLessThanOrEqual(sizes.width);
  await context.close();
});
