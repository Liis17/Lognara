import { expect, test, type Page } from "@playwright/test";

const hosts = [
  { id: 1, name: "mail-01", kind: "host", created_at: "2026-11-01T00:00:00Z" },
  { id: 2, name: "mail-02", kind: "host", created_at: "2026-11-02T00:00:00Z" },
  { id: 3, name: "relay-eu", kind: "relay", created_at: "2026-11-03T00:00:00Z" },
];
type TestMetric = {
  server: string;
  service: string;
  name: string;
  origin: "counter" | "gauge";
  first_seen: string | null;
  last_seen: string | null;
  definition: unknown;
  environments: string[];
};
const metrics: TestMetric[] = [
  { server: "mail-01", service: "host", name: "cpu.usage_percent", origin: "gauge", first_seen: "2026-11-16T08:00:00Z", last_seen: "2026-11-16T09:00:00Z", definition: null, environments: [] },
  { server: "mail-01", service: "host", name: "cpu.iowait_percent", origin: "gauge", first_seen: "2026-11-16T08:00:00Z", last_seen: "2026-11-16T09:00:00Z", definition: null, environments: [] },
  { server: "mail-01", service: "host", name: "disk.sda.read_bytes", origin: "counter", first_seen: "2026-11-16T08:00:00Z", last_seen: "2026-11-16T09:00:00Z", definition: null, environments: [] },
  { server: "mail-01", service: "host", name: "proc.count", origin: "gauge", first_seen: "2026-11-16T08:00:00Z", last_seen: "2026-11-16T09:00:00Z", definition: null, environments: [] },
  { server: "mail-02", service: "host", name: "cpu.usage_percent", origin: "gauge", first_seen: "2026-11-16T08:00:00Z", last_seen: "2026-11-16T09:00:00Z", definition: null, environments: [] },
];

async function fixture(page: Page, overrides: {
  series?: TestMetric[];
  onDefinition?: (definition: Record<string, unknown>) => void;
} = {}) {
  await page.clock.setFixedTime(new Date("2026-11-16T09:30:00Z"));
  await page.route("**/api/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const path = url.pathname;
    if (path === "/api/me") return route.fulfill({ json: { username: "demo", passkeys: 0 } });
    if (path === "/api/relays") return route.fulfill({ json: { relays: hosts } });
    if (path === "/api/agents") return route.fulfill({ json: { agents: [] } });
    if (path === "/api/series/sources") return route.fulfill({ json: { sources: [{ server: "mail-01", service: "host" }] } });
    if (path === "/api/series/favorites") return route.fulfill({ json: { keys: [] } });
    if (path === "/api/alerts/summary") return route.fulfill({ json: { enabled: false, alerts: [] } });
    if (path === "/api/series" && request.method() === "GET") {
      const selected = (overrides.series ?? metrics).filter((metric) =>
        (!url.searchParams.has("server") || metric.server === url.searchParams.get("server")) &&
        (!url.searchParams.has("service") || metric.service === url.searchParams.get("service")),
      );
      return route.fulfill({ json: { metrics: selected } });
    }
    if (path === "/api/series/query" && request.method() === "POST") {
      const body = request.postDataJSON();
      const step = body.step_seconds * 1000;
      const to = Date.parse(body.to);
      const buckets = Array.from({ length: 12 }, (_, index) => new Date(to - (11 - index) * step).toISOString());
      return route.fulfill({ json: {
        kind: body.origin === "counter" ? "delta" : "gauge",
        aggregation: body.origin === "counter" ? "sum" : "avg",
        unit: body.origin === "counter" ? "байт" : "%",
        buckets,
        series: [{ label: "", values: buckets.map((_bucket: string, index: number) => body.name.includes("bytes") ? (index + 1) * step * 1024 ** 2 : 20 + index) }],
      } });
    }
    if (path === "/api/series/definitions" && request.method() === "POST") {
      overrides.onDefinition?.(request.postDataJSON());
      return route.fulfill({ status: 201, json: { id: "created" } });
    }
    if (path === "/api/series/import/check") return route.fulfill({ json: { revision: "r1", errors: [], conflicts: [], metrics: [] } });
    if (path === "/api/series/import/apply") return route.fulfill({ json: { created: 1, replaced: 0, skipped: 0 } });
    if (path === "/api/account/passkeys") return route.fulfill({ json: { passkeys: [] } });
    return route.fulfill({ status: 404, json: { error: "not_found" } });
  });
}

test("host metrics page selects a registered host, queries hwagent series and opens host logs", async ({ page }) => {
  await fixture(page);
  const seriesRequests: { server: string; service: string; name: string }[] = [];
  const seriesResponses = new Map<string, number>();
  page.on("request", (request) => {
    if (request.url().endsWith("/api/series/query")) seriesRequests.push(request.postDataJSON());
  });
  page.on("response", (response) => {
    if (response.url().endsWith("/api/series/query")) {
      const { server } = response.request().postDataJSON();
      seriesResponses.set(server, (seriesResponses.get(server) ?? 0) + 1);
    }
  });
  await page.goto("/hosts");
  await expect(page.getByRole("combobox", { name: "Хост" })).toHaveValue("mail-01");
  await expect(page).toHaveURL(/\/hosts\?host=mail-01/);
  await expect(page.getByRole("region", { name: "CPU" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Диски" })).toBeVisible();
  await expect.poll(() => new Set(seriesRequests.filter((request) => request.server === "mail-01").map((request) => request.name)).size).toBe(4);
  expect(seriesRequests.every((request) => request.server === "mail-01" && request.service === "host")).toBe(true);
  const mail01Responses = seriesResponses.get("mail-01") ?? 0;
  await page.getByRole("combobox", { name: "Хост" }).selectOption("mail-02");
  await expect(page).toHaveURL(/\/hosts\?host=mail-02/);
  await expect.poll(() => (seriesResponses.get("mail-02") ?? 0) > 0).toBe(true);
  await expect(page.getByRole("region", { name: "CPU" })).toBeVisible();
  await expect.poll(() => seriesRequests.some((request) => request.server === "mail-02")).toBe(true);

  await page.getByRole("combobox", { name: "Хост" }).selectOption("mail-01");
  await expect.poll(() => (seriesResponses.get("mail-01") ?? 0) >= mail01Responses + 4).toBe(true);
  await expect(page.getByRole("region", { name: "CPU" })).toBeVisible();
  const plot = page.locator(".line-chart-plot").first();
  await expect(plot).toBeVisible();
  const box = (await plot.boundingBox())!;
  await page.mouse.move(box.x + 90, box.y + 120);
  await page.mouse.down();
  await page.mouse.move(box.x + Math.min(box.width - 20, 360), box.y + 120, { steps: 4 });
  await page.mouse.up();
  await expect(page).toHaveURL(/\/logs\?/);
  const logs = new URL(page.url());
  expect(logs.searchParams.get("q")).toContain('server:"mail-01"');
  expect(logs.searchParams.get("q")).toContain('backend:"host"');
  expect(logs.searchParams.get("q")).toContain('service:"host"');
});

test("host page shows series reported under a different server name than the registry host", async ({ page }) => {
  await fixture(page, { series: [{ ...metrics[0], server: "mail-01.example" }, { ...metrics[1], server: "mail-01.example" }] });
  const seriesRequests: { server: string; service: string }[] = [];
  page.on("request", (request) => {
    if (request.url().endsWith("/api/series/query")) seriesRequests.push(request.postDataJSON());
  });
  await page.goto("/hosts");
  await expect(page).toHaveURL(/\/hosts\?host=mail-01\.example/);
  await expect(page.getByRole("region", { name: "CPU" })).toBeVisible();
  await expect.poll(() => seriesRequests.length).toBeGreaterThan(0);
  expect(seriesRequests.every((request) => request.server === "mail-01.example" && request.service === "host")).toBe(true);
  const picker = page.getByRole("combobox", { name: "Хост" });
  await expect(picker.locator("option", { hasText: "mail-01 · нет данных" })).toHaveCount(1);
  await picker.selectOption("mail-01");
  await expect(page.getByText("Метрик пока нет")).toBeVisible();
});

test("Linux host template creates observed definitions without replacing existing ones", async ({ page }) => {
  const existing = { ...metrics[3], definition: { id: "existing", title: "Существующее определение" } };
  const created: Record<string, unknown>[] = [];
  await fixture(page, { series: [metrics[0], metrics[2], existing], onDefinition: (definition) => created.push(definition) });
  await page.goto("/metrics");
  await page.getByRole("button", { name: "Новая метрика", exact: true }).click();
  await page.getByRole("button", { name: "Linux-хост", exact: true }).click();
  await expect(page.locator(".metric-linux-row")).toHaveCount(3);
  await expect(page.getByText("Определение уже существует и останется без изменений.")).toBeVisible();
  const apply = page.getByRole("button", { name: "Создать определения (2)" });
  await expect(apply).toBeEnabled();
  await apply.click();
  await expect.poll(() => created.length).toBe(2);
  expect(created).toEqual(expect.arrayContaining([
    expect.objectContaining({ server: "mail-01", service: "host", name: "cpu.usage_percent", kind: "gauge", aggregation: "avg", rule: null }),
    expect.objectContaining({ server: "mail-01", service: "host", name: "disk.sda.read_bytes", kind: "delta", aggregation: "sum", rule: null }),
  ]));
  expect(created.some((definition) => definition.name === "proc.count")).toBe(false);
});
