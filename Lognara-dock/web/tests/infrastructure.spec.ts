import { expect, test, type Page } from '@playwright/test';

const asOf = '2026-10-15T12:00:00Z';
const ago = (seconds: number) => new Date(Date.parse(asOf) - seconds * 1000).toISOString();
const source = (server: string) => ({ environment: 'prod', server, backend: 'api', service: 'billing', service_instance: null });

function node(kind: 'relay' | 'agent', id: number, name: string, status: string, extra: Record<string, unknown> = {}) {
  return {
    kind, id, name, relay_id: kind === 'agent' ? 1 : null, relay: kind === 'agent' ? 'relay-eu-1' : null, status, reason: null,
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

test('tree navigation keeps infrastructure active, supports direct route, back, and manual refresh', async ({ page }) => {
  await page.clock.install();
  await fixture(page, response);
  await page.goto('/infrastructure');
  await page.getByRole('link', { name: 'Дерево инфраструктуры' }).click();
  await expect(page.getByRole('heading', { name: 'Дерево инфраструктуры' })).toBeVisible();
  await expect(page.getByRole('navigation', { name: 'Разделы' }).getByRole('link', { name: 'Инфраструктура' }))
    .toHaveAttribute('aria-current', 'page');
  await page.getByRole('link', { name: '← Инфраструктура' }).click();
  await expect(page.getByRole('heading', { name: 'Узлы' })).toBeVisible();

  let requests = 0;
  await page.route('**/api/infrastructure', route => { requests++; return route.fulfill({ json: response }); });
  await page.goto('/infrastructure/tree');
  await expect(page.getByRole('heading', { name: 'Дерево инфраструктуры' })).toBeVisible();
  await expect(page.getByRole('button', { name: /^Обновлено/ })).toBeEnabled();
  const beforeRefresh = requests;
  await page.getByRole('button', { name: /^Обновлено/ }).click();
  await expect.poll(() => requests).toBe(beforeRefresh + 1);
});

test('tree relay cards show failures since start and keep null as no data', async ({ page }) => {
  await fixture(page, { ...response, nodes: [
    node('relay', 1, 'relay-with-failures', 'online', { delivery_failures: 17 }),
    node('relay', 2, 'relay-without-count', 'online', { delivery_failures: null }),
  ] });
  await page.goto('/infrastructure/tree');
  await expect(page.locator('[data-infra-node-key="relay-1"]')).toContainText('сбоев доставки с запуска 17');
  await expect(page.locator('[data-infra-node-key="relay-2"]')).toContainText('сбоев доставки с запуска нет данных');
});

test('tree cards select paths with pointer and keyboard; Escape remains available to logout dialog', async ({ page }) => {
  await fixture(page, response);
  await page.goto('/infrastructure/tree');
  const scroll = page.getByRole('group', { name: 'Дерево инфраструктуры' });
  const agent = page.locator('[data-infra-node-key="agent-1"]');
  const sibling = page.locator('[data-infra-node-key="agent-2"]');
  const relay = page.locator('[data-infra-node-key="relay-1"]');
  const core = page.locator('[data-infra-node-key="core"]');
  const dock = page.locator('[data-infra-node-key="dock"]');

  await agent.click();
  await expect(agent).toHaveAttribute('aria-pressed', 'true');
  await expect(relay).toHaveAttribute('aria-pressed', 'true');
  await expect(core).toHaveAttribute('aria-pressed', 'true');
  await expect(dock).toHaveAttribute('aria-pressed', 'true');
  await expect(sibling).toHaveAttribute('aria-pressed', 'false');
  await expect(page.locator('.infra-tree-selection')).toContainText('Агент srv-eu-12 (#1) → Relay relay-eu-1 (#1) → Core → Dock');
  await expect(page.locator('[data-infra-edge-key="agent-1:relay-1"]')).toHaveClass(/is-active/);
  await agent.click();
  await expect(agent).toHaveAttribute('aria-pressed', 'false');
  await expect(page.locator('.infra-tree-selection')).toContainText('Выберите карточку');

  await relay.focus();
  await page.keyboard.press('Space');
  await expect(relay).toHaveAttribute('aria-pressed', 'true');
  await expect(agent).toHaveAttribute('aria-pressed', 'true');
  await expect(sibling).toHaveAttribute('aria-pressed', 'true');
  await page.keyboard.press('Escape');
  await expect(relay).toHaveAttribute('aria-pressed', 'false');

  await agent.focus();
  await page.keyboard.press('Enter');
  await expect(agent).toHaveAttribute('aria-pressed', 'true');
  await page.getByRole('button', { name: 'Выйти', exact: true }).click();
  const logout = page.getByRole('dialog', { name: 'Выйти из Lognara?' });
  await expect(logout).toBeVisible();
  await page.keyboard.press('Escape');
  await expect(logout).toBeHidden();
  await expect(scroll).toBeVisible();
});

test('tree preserves selection and scroll through reordered polling, then clears deleted selection and restores focus', async ({ page }) => {
  await page.clock.install();
  await page.setViewportSize({ width: 390, height: 844 });
  const many = Array.from({ length: 24 }, (_, i) => node('agent', i + 1, `agent-${i + 1}`, 'online', { relay_id: 1 }));
  const initial = { ...response, nodes: [node('relay', 1, 'relay-eu-1', 'online'), ...many] };
  const reordered = {
    ...initial,
    nodes: [
      ...[...initial.nodes].reverse().map(item => item.kind === 'agent' && item.id === 1 ? { ...item, status: 'offline' } : item),
    ],
  };
  const deleted = { ...reordered, nodes: reordered.nodes.filter(item => !(item.kind === 'agent' && item.id === 1)) };
  let requests = 0;
  let snapshot = initial;
  await page.route('**/api/**', route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/me') return route.fulfill({ json: { username: 'liis', passkeys: 0 } });
    if (path === '/api/infrastructure') {
      requests++;
      return route.fulfill({ json: snapshot });
    }
    return route.fulfill({ status: 404, json: { error: 'not_found' } });
  });
  await page.goto('/infrastructure/tree');
  const scroll = page.getByRole('group', { name: 'Дерево инфраструктуры' });
  const agent = page.locator('[data-infra-node-key="agent-1"]');
  await expect(page.getByRole('button', { name: /^Обновлено/ })).toBeEnabled();
  await agent.click();
  await scroll.evaluate((element: HTMLElement) => { element.scrollLeft = 150; });
  await page.evaluate(() => window.scrollTo(0, 120));
  const beforeReorder = requests;
  snapshot = reordered;
  await page.clock.fastForward(10_001);
  await expect.poll(() => requests).toBe(beforeReorder + 1);
  await expect(agent).toContainText('Отключён');
  await expect(agent).toHaveAttribute('aria-pressed', 'true');
  const preserved = await scroll.evaluate((element: HTMLElement) => element.scrollLeft);
  expect(preserved).toBe(150);
  expect(await page.evaluate(() => window.scrollY)).toBe(120);

  const beforeDelete = requests;
  snapshot = deleted;
  await page.clock.fastForward(10_001);
  await expect.poll(() => requests).toBe(beforeDelete + 1);
  await expect(page.locator('[data-infra-node-key="agent-1"]')).toHaveCount(0);
  await expect(page.locator('.infra-tree-selection')).toContainText('Выберите карточку');
  await expect.poll(() => scroll.evaluate((element: HTMLElement) => document.activeElement === element)).toBe(true);
});

test('tree distinguishes initial loading, registry failure, empty registry, and unavailable core', async ({ page }) => {
  let release!: () => void;
  const pending = new Promise<void>(resolve => { release = resolve; });
  const partial = { ...response, core: { status: 'unavailable', version: null, visibility_lag_ms: null, wal_bytes: null, wal_limit_bytes: null }, storage: null, registry: false, nodes: [] };
  const empty = { ...response, registry: true, nodes: [] };
  let snapshot: 'initial-error' | 'partial' | 'empty' = 'initial-error';
  let requests = 0;
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/me') return route.fulfill({ json: { username: 'liis', passkeys: 0 } });
    if (path === '/api/infrastructure') {
      const requestSnapshot = snapshot;
      requests++;
      if (requestSnapshot === 'initial-error') { await pending; return route.fulfill({ status: 502, json: { error: 'core_unavailable' } }); }
      return route.fulfill({ json: requestSnapshot === 'partial' ? partial : empty });
    }
    return route.fulfill({ status: 404, json: { error: 'not_found' } });
  });
  await page.goto('/infrastructure/tree');
  await expect(page.getByRole('heading', { name: 'Дерево инфраструктуры' })).toBeVisible();
  await expect.poll(() => requests).toBeGreaterThan(0);
  await expect(page.getByRole('status')).toContainText('Загружаем дерево инфраструктуры');
  release();
  await expect(page.getByRole('alert')).toContainText('Снимок ещё не получен');
  await expect(page.getByText('Данные инфраструктуры пока недоступны.')).toBeVisible();
  snapshot = 'partial';
  await page.getByRole('button', { name: 'Обновить' }).click();
  await expect(page.getByRole('alert')).toContainText('список узлов из Core');
  await expect(page.getByRole('link', { name: 'Управление подключениями' })).toHaveAttribute('href', '/access');
  await expect(page.locator('[data-infra-node-key="core"]')).toContainText('Недоступен');
  await expect(page.locator('[data-infra-node-key="dock"]')).toContainText('Онлайн');
  await expect(page.locator('[data-infra-node-key^="agent-"]')).toHaveCount(0);

  snapshot = 'empty';
  await page.getByRole('button', { name: /^Обновлено/ }).click();
  await expect(page.getByText('Нет зарегистрированных relay и агентов.')).toBeVisible();
  await expect(page.locator('[data-infra-node-key="core"]')).toContainText('Онлайн');
  await expect(page.getByRole('link', { name: 'Подключениях' })).toHaveAttribute('href', '/access');
});

test('failed tree refresh keeps a stale snapshot and removes fresh Dock health styling', async ({ page }) => {
  await page.clock.install();
  await fixture(page, { ...response, nodes: [node('relay', 1, 'relay-eu-1', 'online'), node('agent', 1, 'agent-one', 'online', { relay_id: 1 })] });
  await page.goto('/infrastructure/tree');
  const agent = page.locator('[data-infra-node-key="agent-1"]');
  await agent.click();
  await expect(page.locator('.infra-tree-flow')).toHaveCount(2);
  await page.route('**/api/infrastructure', route => route.fulfill({ status: 502, json: { error: 'core_unavailable' } }));
  await page.clock.fastForward(10_001);
  await expect(page.getByRole('alert')).toContainText('данные устарели');
  await expect(page.locator('.infra-tree-stale')).toBeVisible();
  await expect(page.locator('.infra-tree-flow')).toHaveCount(0);
  await expect(agent).toHaveAttribute('aria-pressed', 'true');
  await expect(page.locator('[data-infra-node-key="dock"]')).toContainText('Не подтверждено');
  await expect(page.locator('.infra-tree-edge.is-active')).toHaveCount(3);
});

test('tree wraps long names and scrolls only horizontally inside the graph on a 390px screen with 500 agents', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  const longName = 'agent-with-a-long-name-'.repeat(6);
  const agents = Array.from({ length: 500 }, (_, i) => node('agent', i + 1, i === 0 ? longName : `agent-${i + 1}`, 'online', { relay_id: 1 }));
  await fixture(page, { ...response, nodes: [node('relay', 1, 'relay-eu-1', 'online'), ...agents] });
  await page.goto('/infrastructure/tree');
  const scroll = page.getByRole('group', { name: 'Дерево инфраструктуры' });
  await expect(page.locator('[data-infra-node-key^="agent-"]')).toHaveCount(500);
  const longCard = page.locator('[data-infra-node-key="agent-1"]');
  await expect(longCard).toContainText(longName);
  const dimensions = await page.evaluate(() => {
    const viewport = document.querySelector<HTMLElement>('.infra-tree-scroll')!;
    const canvas = document.querySelector<HTMLElement>('.infra-tree-canvas')!;
    return { pageWidth: document.documentElement.scrollWidth, clientWidth: viewport.clientWidth, scrollWidth: viewport.scrollWidth,
      clientHeight: viewport.clientHeight, scrollHeight: viewport.scrollHeight, canvasHeight: canvas.offsetHeight,
      longCardHeight: document.querySelector<HTMLElement>('[data-infra-node-key="agent-1"]')!.getBoundingClientRect().height };
  });
  expect(dimensions.pageWidth).toBeLessThanOrEqual(390);
  expect(dimensions.scrollWidth).toBeGreaterThan(dimensions.clientWidth);
  expect(dimensions.scrollHeight).toBeLessThanOrEqual(dimensions.clientHeight + 1);
  expect(dimensions.longCardHeight).toBeGreaterThan(58);

  await expect(async () => {
    const geometry = await page.evaluate(() => {
      const canvas = document.querySelector<HTMLElement>('.infra-tree-canvas')!;
      const agent = document.querySelector<HTMLElement>('[data-infra-card-key="agent-1"]')!.getBoundingClientRect();
      const relay = document.querySelector<HTMLElement>('[data-infra-card-key="relay-1"]')!.getBoundingClientRect();
      const d = document.querySelector<SVGPathElement>('[data-infra-edge-key="agent-1:relay-1"]')!.getAttribute('d')!;
      const values = d.match(/^M ([\d.]+) ([\d.]+) C [\d.]+ [\d.]+, [\d.]+ [\d.]+, ([\d.]+) ([\d.]+)$/)!;
      return { startX: Number(values[1]), startY: Number(values[2]), endX: Number(values[3]), endY: Number(values[4]),
        expectedStartX: agent.right - canvas.getBoundingClientRect().left,
        expectedStartY: (agent.top + agent.bottom) / 2 - canvas.getBoundingClientRect().top,
        expectedEndX: relay.left - canvas.getBoundingClientRect().left,
        expectedEndY: (relay.top + relay.bottom) / 2 - canvas.getBoundingClientRect().top };
    });
    expect(Math.abs(geometry.startX - geometry.expectedStartX)).toBeLessThan(1);
    expect(Math.abs(geometry.startY - geometry.expectedStartY)).toBeLessThan(1);
    expect(Math.abs(geometry.endX - geometry.expectedEndX)).toBeLessThan(1);
    expect(Math.abs(geometry.endY - geometry.expectedEndY)).toBeLessThan(1);
  }).toPass({ timeout: 5_000 });
  await scroll.evaluate((element: HTMLElement) => { element.scrollLeft = 700; });
  expect(await scroll.evaluate((element: HTMLElement) => element.scrollLeft)).toBeGreaterThan(0);
  await page.evaluate(() => window.scrollTo(0, 1200));
  expect(await page.evaluate(() => window.scrollY)).toBeGreaterThan(0);
  await page.locator('[data-infra-node-key="agent-500"]').click();
  await expect(page.locator('.infra-tree-selection')).toContainText('Агент agent-500');
  const sticky = await page.evaluate(() => {
    const viewport = document.querySelector<HTMLElement>('.infra-tree-scroll')!;
    const summary = document.querySelector<HTMLElement>('.infra-tree-selection')!;
    return { left: summary.getBoundingClientRect().left, viewportLeft: viewport.getBoundingClientRect().left + viewport.clientLeft };
  });
  expect(Math.abs(sticky.left - sticky.viewportLeft)).toBeLessThan(1);
});

test('agent card keeps name, status and last event in two lines', async ({ page }) => {
  await fixture(page, { ...response, nodes: [node('relay', 1, 'relay-eu-1', 'online'), node('agent', 123, 'srv-eu-12-prod', 'idle', { last_event_at: ago(259_200) })] });
  await page.goto('/infrastructure/tree');
  const card = page.locator('[data-infra-node-key="agent-123"]');
  await expect(card).toContainText('последнее событие');
  const head = await card.locator('.infra-tree-node-head').boundingBox();
  const sub = await card.locator('.infra-tree-node-sub').first().boundingBox();
  const relay = await page.locator('[data-infra-card-key="relay-1"]').boundingBox();
  expect(head!.height).toBeLessThan(26);
  expect(sub!.height).toBeLessThan(20);
  expect(head!.x + head!.width).toBeLessThan(relay!.x);
});

test('double click on an agent opens its logs with a range covering its last event', async ({ page }) => {
  await fixture(page, { ...response, nodes: [node('relay', 1, 'relay-eu-1', 'online'), node('agent', 3, 'srv-eu-07', 'idle', { last_event_at: ago(90_000) })] });
  await page.goto('/infrastructure/tree');
  await page.locator('[data-infra-node-key="agent-3"]').dblclick();
  await expect(page).toHaveURL(/\/logs\?/);
  const params = new URL(page.url()).searchParams;
  expect(params.get('view')).toBe('events');
  expect(params.get('range')).toBe('7d');
  expect(params.get('q')).toContain('server:"srv-eu-07"');
});

test('flow animation runs only on links leaving online agents and relays', async ({ page }) => {
  await fixture(page, { ...response, nodes: [
    node('relay', 1, 'relay-eu-1', 'online'),
    node('agent', 1, 'agent-online', 'online', { relay_id: 1 }),
    node('agent', 2, 'agent-offline', 'offline', { relay_id: 1 }),
  ] });
  await page.goto('/infrastructure/tree');
  await expect(page.locator('[data-infra-node-key="agent-2"]')).toBeVisible();
  await expect(page.locator('.infra-tree-flow')).toHaveCount(2);
  await expect(page.locator('[data-infra-flow-key="agent-1:relay-1"]')).toHaveCount(1);
  await expect(page.locator('[data-infra-flow-key="relay-1:core"]')).toHaveCount(1);
  await expect(page.locator('[data-infra-flow-key="agent-2:relay-1"]')).toHaveCount(0);
});
