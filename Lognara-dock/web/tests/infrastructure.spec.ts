import { expect, test, type Page } from '@playwright/test';

const asOf = '2026-10-15T12:00:00Z';
const ago = (seconds: number) => new Date(Date.parse(asOf) - seconds * 1000).toISOString();
const source = (server: string) => ({ environment: 'prod', server, backend: 'api', service: 'billing', service_instance: null });

function node(kind: 'relay' | 'agent', id: number, name: string, status: string, extra: Record<string, unknown> = {}) {
  return {
    kind, id, name, relay: kind === 'agent' ? 'relay-eu-1' : null, status, reason: null,
    heartbeat_at: ago(2), last_event_at: ago(1), version: '0.1.0',
    spool: { used_bytes: 4, limit_bytes: 100, records: 1 }, delivery_failures: 0,
    events_per_second: kind === 'relay' ? 2100 : null, source: kind === 'agent' ? source(name) : null, ...extra,
  };
}

const response = {
  as_of: asOf,
  thresholds: { heartbeat_ttl_seconds: 60, idle_seconds: 300, spool_warn_percent: 80 },
  core: { status: 'online', version: '0.3.0', visibility_lag_ms: 600, wal_bytes: 2 * 1024 ** 3, wal_limit_bytes: 4 * 1024 ** 3 },
  storage: { segments: 12, parquet_bytes: 412 * 1024 ** 3, index_bytes: 38 * 1024 ** 3, oldest_event_at: '2026-09-13T08:00:00Z',
    retention_seconds: 604_800, disk_total_bytes: 1000 * 1024 ** 3, disk_free_bytes: 310 * 1024 ** 3 },
  dock: { version: '0.1.0', sse_clients: 3, sessions: 4 },
  registry: true,
  nodes: [
    node('agent', 1, 'srv-eu-12', 'offline', { heartbeat_at: ago(2460), last_event_at: ago(2460), spool: null }),
    node('agent', 2, 'srv-eu-19', 'degraded', { reason: 'spool', spool: { used_bytes: 74, limit_bytes: 90, records: 9 } }),
    node('relay', 1, 'relay-eu-1', 'online'),
    node('agent', 3, 'srv-eu-07', 'idle', { last_event_at: ago(840) }),
    node('agent', 4, 'srv-new', 'unknown', { reason: 'no_heartbeat', heartbeat_at: null, last_event_at: null, version: null, spool: null, source: null }),
  ],
};

async function fixture(page: Page, body: unknown) {
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/me') return route.fulfill({ json: { username: 'liis', passkeys: 0 } });
    if (path === '/api/infrastructure') return route.fulfill({ json: body });
    return route.fulfill({ status: 404, json: { error: 'not_found' } });
  });
}

test('infrastructure shows pipeline, problem nodes first and storage', async ({ page }) => {
  await fixture(page, response);
  await page.goto('/infrastructure');
  await expect(page.getByRole('heading', { name: '2 узла требуют внимания' })).toBeVisible();
  await expect(page.getByText('lag публикации в Core 0,6 с')).toBeVisible();
  const pipe = page.getByRole('region', { name: 'Цепочка доставки' });
  await expect(pipe).toContainText('4узла');
  await expect(pipe).toContainText('1 отключено');
  await expect(pipe).toContainText(/2\s100событий\/с/);
  await expect(pipe).toContainText('спул макс. 82% · srv-eu-19');
  await expect(pipe).toContainText('3SSE-клиента');
  const rows = page.locator('.nodes-table tbody tr');
  await expect(rows).toHaveCount(5);
  await expect(rows.nth(0)).toContainText('agent srv-eu-12');
  await expect(rows.nth(0)).toContainText('Отключён');
  await expect(rows.nth(0)).toContainText('41 мин');
  await expect(rows.nth(1)).toContainText('Деградирован');
  await expect(rows.nth(1)).toContainText('спул заполнен');
  await expect(rows.nth(3)).toContainText('Нет новых логов');
  await expect(rows.nth(3)).toContainText('14 мин');
  await expect(rows.nth(4)).toContainText('heartbeat ещё не приходил');
  const logs = rows.nth(0).getByRole('link', { name: 'фильтр srv-eu-12' });
  expect(decodeURIComponent(await logs.getAttribute('href') ?? '')).toContain('server:"srv-eu-12"');
  const storage = page.getByRole('region', { name: 'Хранилище' });
  await expect(storage).toContainText('retention 7 дн');
  await expect(storage).toContainText('412 ГБ');
  await expect(storage).toContainText('31% · 310 ГБ');
  await expect(page.getByRole('link', { name: 'Управление подключениями' })).toHaveAttribute('href', '/access');
});

test('infrastructure reports unavailable core and fits a narrow screen', async ({ page }) => {
  await fixture(page, { ...response, core: { status: 'unavailable', version: null, visibility_lag_ms: null, wal_bytes: null, wal_limit_bytes: null },
    storage: null, registry: false, nodes: [] });
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/infrastructure');
  await expect(page.getByRole('heading', { name: 'Core недоступен' })).toBeVisible();
  await expect(page.getByText('Не удалось получить список узлов из Core.')).toBeVisible();
  await expect(page.getByText('Core недоступен: размеры хранилища неизвестны.')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
});

test('infrastructure completes a slow request before scheduling the next refresh', async ({ page }) => {
  await page.clock.install();
  await fixture(page, response);
  await page.goto('/infrastructure');
  await expect(page.getByRole('heading', { name: '2 узла требуют внимания' })).toBeVisible();
  let requests = 0;
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  await page.route('**/api/infrastructure', async route => {
    requests++;
    await pending;
    await route.fulfill({ json: response });
  });
  const refresh = page.getByRole('button', { name: /^Обновлено/ });
  await refresh.click();
  await expect.poll(() => requests).toBe(1);
  await page.clock.fastForward(11_000);
  expect(requests).toBe(1);
  release();
  await expect(refresh).toBeEnabled();
  await page.clock.fastForward(10_001);
  await expect.poll(() => requests).toBe(2);
});

test('failed refresh marks the previous snapshot stale and stops claiming health', async ({ page }) => {
  await page.clock.install();
  await fixture(page, { ...response, nodes: [node('relay', 1, 'relay-eu-1', 'online')] });
  await page.goto('/infrastructure');
  await expect(page.getByRole('heading', { name: 'Узлы на связи' })).toBeVisible();
  await page.route('**/api/infrastructure', route => route.fulfill({ status: 502, json: { error: 'core_unavailable' } }));
  await page.clock.fastForward(10_001);
  await expect(page.getByRole('alert')).toContainText('статусы ниже не подтверждены');
  await expect(page.getByRole('heading', { name: 'Данные инфраструктуры устарели' })).toBeVisible();
  await expect(page.locator('.infra-stale')).toBeVisible();
});

test('not-ready core and unknown nodes never claim a healthy pipeline', async ({ page }) => {
  await fixture(page, { ...response, core: { ...response.core, status: 'not_ready' } });
  await page.goto('/infrastructure');
  await expect(page.getByRole('heading', { name: 'Core не готов' })).toBeVisible();
  await page.route('**/api/infrastructure', route => route.fulfill({ json: {
    ...response, nodes: [node('relay', 1, 'new-relay', 'unknown', { heartbeat_at: null, last_event_at: null, version: null, spool: null, delivery_failures: null, events_per_second: null })],
  } }));
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Состояние 1 узла неизвестно' })).toBeVisible();
  await expect(page.getByRole('region', { name: 'Цепочка доставки' })).toContainText('нет данных');
});
