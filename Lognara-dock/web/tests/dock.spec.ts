import { expect, test, type Page } from '@playwright/test';

const traceId = '4bf92f3577b34da6a3ce929d0e0e4736';
const now = Date.now();
const events = Array.from({ length: 70 }, (_, index) => ({
  id: `00000000-0000-4000-8000-${String(index + 1).padStart(12, '0')}`,
  timestamp: new Date(now - index * 1000).toISOString(),
  ingested_at: new Date(now - index * 1000 + 30).toISOString(),
  environment: 'prod', server: 'prod-eu-03', backend: 'payments', service: 'billing',
  service_instance: index % 2 ? 'billing-2a11' : 'billing-7f9c', action: 'charge.create',
  level: index % 4 ? 'warn' : 'error',
  message: index === 0 ? 'Database connection refused: host=10.4.2.18 port=5432'
    : index === 1 ? 'Pool exhausted, waiting 2000 ms' : `Payment retry ${index}`,
  trace_id: traceId, span_id: '00f067aa0ba902b7', parent_span_id: null,
  request_id: `req_${index}`, attributes: { 'db.host': '10.4.2.18', duration_ms: 5012, retry: 3 },
}));

// Следующая страница догружается при прокрутке к концу таблицы; кнопка остаётся запасным путём.
async function loadNextPage(page: Page) {
  await page.getByRole('button', { name: 'Загрузить ещё 50', exact: true }).scrollIntoViewIfNeeded();
  await expect(page.getByRole('button', { name: 'Загрузить ещё 50', exact: true })).toHaveCount(0);
}

type Call = { path: string; body: Record<string, any> };
async function fixture(page: Page, initiallyAuthenticated = true, dataset = events) {
  let authenticated = initiallyAuthenticated;
  const calls: Call[] = [];
  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const body = request.postDataJSON() ?? {};
    calls.push({ path, body });
    const reply = (json: unknown, status = 200) => route.fulfill({ status, json });
    if (path === '/api/auth/login') {
      expect(body).toEqual({ username: 'liis', password: 'test-password' });
      authenticated = true;
      return reply({});
    }
    if (path === '/api/auth/logout') { authenticated = false; return reply({}); }
    if (!authenticated) return reply({ error: 'unauthorized' }, 401);
    if (path === '/api/me') return reply({ username: 'liis', passkeys: 0 });
    if (path === '/api/account/passkeys') return reply({ passkeys: [] });
    if (path === '/api/alerts') return reply({
      enabled: false, token_configured: false, send_paused: false,
      send_pause_reason: null, core_status: 'unknown', core_checked_at: null,
      core_error: null, recipients: [], groups: [], alerts: [],
      pending_deliveries: 0, permanent_errors: 0,
    });
    if (path === '/api/account/password') return reply({});
    if (path === '/api/relays') return reply({ relays: [{ id: 1, name: 'relay-eu', created_at: new Date(now).toISOString() }] });
    if (path === '/api/agents') return reply({ agents: [] });
    function matchingRows() {
      let rows = dataset;
      for (const [field, values] of Object.entries(body.filters ?? {})) {
        rows = rows.filter((event) => (values as string[]).includes(String(event[field as keyof typeof event])));
      }
      if (body.text) rows = rows.filter((event) => event.message.toLowerCase().includes(body.text.query.toLowerCase()));
      if (body.filters?.id && body.limit === 1000) rows = rows.filter(event => event.timestamp === body.from);
      return rows;
    }
    if (path === '/api/logs/search') {
      const rows = matchingRows();
      const offset = body.cursor === 'page-2' ? 50 : 0;
      return reply({ events: rows.slice(offset, offset + body.limit), next_cursor: offset + body.limit < rows.length ? 'page-2' : null, as_of: new Date(now).toISOString() });
    }
    if (path.startsWith('/api/traces/')) return reply({ events: dataset.slice(0, 4).reverse(), next_cursor: null, as_of: new Date(now).toISOString() });
    if (path === '/api/stats/summary') {
      const rows = matchingRows();
      const levelNames = ['trace', 'debug', 'info', 'warn', 'error', 'fatal', 'unknown'];
      const levels = body.search_limit
        ? Object.fromEntries(levelNames.map(level => [level, rows.filter(event => event.level === level).length]))
        : { info: 697, warn: 210, error: 70, fatal: 20, debug: 1, trace: 1, unknown: 1 };
      const total = Object.values(levels).reduce((sum, value) => sum + value, 0);
      const measuredDuration = (count: number) => {
        const measured = Math.floor(count * 0.8);
        return { p95_ms: measured ? 5012 : null, measured, coverage: count ? measured / count : 0, method: 'tdigest' };
      };
      const duration = measuredDuration(total);
      const buckets = Array.from({ length: 12 }, (_, index) => {
        const counts = Object.fromEntries(levelNames.map(level => [level, Math.floor(levels[level] / 12) + (index < levels[level] % 12 ? 1 : 0)]));
        return { timestamp: new Date(Date.parse(body.from) + index * body.interval_seconds * 1000).toISOString(),
          count: Object.values(counts).reduce((sum, value) => sum + value, 0), levels: counts };
      });
      return reply({ as_of: new Date(now).toISOString(), total, buckets, levels, duration,
        error_services: [{ service: 'billing', count: 70 }, { service: 'worker', count: 12 }],
        actions: [{ action: 'charge.create', count: Math.max(0, total - 10), duration: measuredDuration(Math.max(0, total - 10)) },
          { action: null, count: Math.min(10, total), duration: { p95_ms: null, measured: 0, coverage: 0, method: 'tdigest' } }],
        page: body.search_limit ? { events: rows.slice(0, body.search_limit), next_cursor: rows.length > body.search_limit ? 'page-2' : null, as_of: new Date(now).toISOString() } : null });
    }
    if (path === '/api/stats/histogram') {
      const start = Date.parse(body.from);
      const step = body.interval_seconds * 1000;
      return reply({ buckets: Array.from({ length: 12 }, (_, index) => ({ timestamp: new Date(start + index * step).toISOString(), count: index % 3 === 0 ? 0 : index + 1 })), as_of: new Date(now).toISOString() });
    }
    if (path === '/api/stats/group-by') {
      const field = body.group_by[0];
      const groups = field === 'level' ? [
        { values: { level: 'info' }, count: 700 }, { values: { level: 'warn' }, count: 210 },
        { values: { level: 'error' }, count: 70 }, { values: { level: 'fatal' }, count: 20 },
      ] : [{ values: { service: 'billing' }, count: 70 }, { values: { service: 'worker' }, count: 12 }];
      return reply({ groups, as_of: new Date(now).toISOString() });
    }
    return reply({ error: 'not_found' }, 404);
  });
  return calls;
}

test('password login and existing account forms remain usable', async ({ page }) => {
  await fixture(page, false);
  await page.goto('/');
  await page.getByLabel('Логин', { exact: true }).fill('liis');
  await page.getByLabel('Пароль', { exact: true }).fill('test-password');
  await page.getByRole('button', { name: 'Войти', exact: true }).click();
  await expect(page.getByRole('link', { name: 'Настройки', exact: true })).toBeVisible();
  await page.getByRole('link', { name: 'Настройки', exact: true }).click();
  await expect(page).toHaveURL(/\/settings$/);
  await expect(page.getByRole('heading', { name: 'Пароль', exact: true })).toBeVisible();
  await page.getByLabel('Текущий пароль', { exact: true }).fill('test-password');
  await page.getByLabel('Новый пароль', { exact: true }).fill('new-long-password');
  await page.getByLabel('Повторите новый пароль', { exact: true }).fill('new-long-password');
  await page.getByRole('button', { name: 'Сменить пароль', exact: true }).click();
  await expect(page.getByText('Пароль изменён, остальные сессии завершены')).toBeVisible();
});

test('logout asks for confirmation and cancel keeps the session', async ({ page }) => {
  const calls = await fixture(page);
  await page.goto('/logs');
  await page.getByRole('banner').getByRole('button', { name: 'Выйти', exact: true }).click();
  const dialog = page.getByRole('dialog', { name: 'Выйти из Lognara?' });
  await expect(dialog).toBeVisible();
  await dialog.getByRole('button', { name: 'Отмена', exact: true }).click();
  await expect(dialog).toBeHidden();
  expect(calls.filter(call => call.path === '/api/auth/logout')).toHaveLength(0);
  await page.getByRole('banner').getByRole('button', { name: 'Выйти', exact: true }).click();
  await dialog.getByRole('button', { name: 'Выйти', exact: true }).click();
  await expect(page.getByLabel('Логин', { exact: true })).toBeVisible();
  expect(calls.filter(call => call.path === '/api/auth/logout')).toHaveLength(1);
});

test('dragging over the events chart narrows the range and reset restores the preset', async ({ page }) => {
  const calls = await fixture(page);
  await page.goto('/logs');
  const plot = page.getByRole('group', { name: /График событий/ });
  await expect(plot).toBeVisible();
  const before = new URL(page.url()).searchParams;
  const box = (await plot.boundingBox())!;
  await page.mouse.move(box.x + box.width * 0.2, box.y + box.height * 0.5);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * 0.5, box.y + box.height * 0.8, { steps: 5 });
  await page.mouse.up();
  await expect(page.getByText(/^Выбрано: .* логи ограничены этим диапазоном$/)).toBeVisible();
  const after = new URL(page.url()).searchParams;
  expect(Date.parse(after.get('from')!)).toBeGreaterThan(Date.parse(before.get('from')!));
  expect(Date.parse(after.get('to')!)).toBeLessThan(Date.parse(before.get('to')!));
  await expect.poll(() => calls.filter(call => call.path === '/api/stats/summary').at(-1)?.body.from).toBe(after.get('from'));
  await expect(page.getByLabel('Период', { exact: true })).toHaveValue('fixed');
  await page.getByRole('button', { name: 'Сбросить выбор и масштаб', exact: true }).click();
  await expect(page.getByLabel('Период', { exact: true })).toHaveValue('1h');
  await expect(page.getByText(/^Выбрано: /)).toHaveCount(0);
});

test('failed auto-load does not retry by itself and the button retries once', async ({ page }) => {
  await fixture(page);
  let cursorCalls = 0;
  await page.route('**/api/logs/search', route => {
    if (!route.request().postDataJSON()?.cursor) return route.fallback();
    cursorCalls += 1;
    return route.fulfill({ status: 503, json: { error: 'query_unavailable' } });
  });
  await page.goto('/logs');
  const more = page.getByRole('button', { name: 'Загрузить ещё 50', exact: true });
  await more.scrollIntoViewIfNeeded();
  await expect(page.getByRole('alert')).toBeVisible();
  await expect(more).toBeEnabled();
  // Наблюдатель срабатывает сразу при повторной подписке; короткая пауза ловит возможный цикл.
  await page.waitForTimeout(500);
  expect(cursorCalls).toBe(1);
  await more.click();
  await expect.poll(() => cursorCalls).toBe(2);
  await expect(more).toBeEnabled();
  await page.waitForTimeout(500);
  expect(cursorCalls).toBe(2);
});

test('patterns list shows share, growth tone and the selected sort', async ({ page }) => {
  await fixture(page);
  const pattern = (id: string, level: string, text: string, count: number, growth: number) => ({
    pattern_id: id, environment: 'prod', service: 'billing', level, template: text,
    parts: [{ kind: 'literal', text }, { kind: 'variable', name: 'ms', variable_type: 'number' }], exact: false,
    count, share: count / 300, previous_count: 10, growth_percent: growth, is_new: false,
    first_seen: new Date(now - 7_200_000).toISOString(), last_seen: new Date(now).toISOString(),
    buckets: [0, 1, 2].map(index => ({ timestamp: new Date(now - (3 - index) * 60_000).toISOString(), count: index + 1 })),
  });
  const sorts: string[] = [];
  await page.route('**/api/patterns/hidden', route => route.fulfill({ json: { pattern_ids: [] } }));
  await page.route('**/api/patterns/search**', route => {
    sorts.push(route.request().postDataJSON().sort);
    return route.fulfill({ json: {
      algorithm_version: 1, as_of: new Date(now).toISOString(), watermark: 5, core_watermark: 5,
      history_from: new Date(now - 86_400_000).toISOString(), history_complete: true,
      total_events: 300, total_patterns: 2, visible_patterns: 2, new_patterns: 0,
      patterns: [pattern('a'.repeat(32), 'error', 'gateway timeout after ', 214, 120), pattern('b'.repeat(32), 'info', 'request took ', 86, -12)],
    } });
  });
  await page.goto('/logs?view=patterns');
  await expect(page.getByRole('heading', { name: 'Паттерны', exact: true })).toBeVisible();
  const rows = page.locator('.pattern-row');
  await expect(rows).toHaveCount(2);
  await expect(rows.first().locator('.pattern-growth')).toHaveClass(/growth-up/);
  await expect(rows.nth(1).locator('.pattern-growth')).toHaveClass(/growth-down/);
  await expect(page.getByText(/Показано 2 из 2 видимых паттернов/)).toBeVisible();
  await expect(page.getByRole('button', { name: /^Live/ })).toHaveCount(0);
  await page.screenshot({ path: test.info().outputPath('patterns-desktop.png'), fullPage: true });
  await page.getByLabel('Сортировка', { exact: true }).selectOption('growth');
  await expect.poll(() => sorts.at(-1)).toBe('growth');
});

test('history filter uses typed core query, cursor and text-scoped counts', async ({ page }) => {
  const calls = await fixture(page);
  await page.goto('/logs');
  await expect(page.getByRole('button', { name: /Database connection refused/ })).toBeVisible();
  await page.getByLabel('Поисковый запрос', { exact: true }).fill('service:billing AND level:(ERROR OR WARN)');
  await page.getByRole('button', { name: 'Найти', exact: true }).click();
  await expect.poll(() => calls.filter((call) => call.path === '/api/stats/summary').at(-1)?.body.filters)
    .toEqual({ service: ['billing'], level: ['error', 'warn'] });
  await loadNextPage(page);
  await expect.poll(() => calls.filter((call) => call.path === '/api/logs/search').at(-1)?.body.cursor).toBe('page-2');
  await expect(page.getByRole('button', { name: /Payment retry 69/ })).toBeVisible();
  await expect(page.getByText('В диапазоне 70', { exact: true })).toBeVisible();
  await page.getByLabel('Поисковый запрос', { exact: true }).fill('"connection refused"');
  await page.getByRole('button', { name: 'Найти', exact: true }).click();
  await expect.poll(() => calls.filter((call) => call.path === '/api/stats/summary').at(-1)?.body.text)
    .toEqual({ query: 'connection refused', mode: 'phrase' });
  await expect(page.getByText('В диапазоне 1', { exact: true })).toBeVisible();
  await expect(page.locator('.row-button')).toHaveCount(1);
  await expect(page.getByText(/Общий снимок сводки и первой страницы/)).toBeVisible();
  expect(calls.filter(call => call.path === '/api/stats/histogram')).toHaveLength(0);
  expect(calls.filter(call => call.path === '/api/logs/search')).toHaveLength(1); // Только курсор, не первая страница.
  expect(calls.filter(call => call.path === '/api/stats/summary').at(-1)?.body.search_limit).toBe(50);
});

test('event details open correlated trace and preserve shared link state', async ({ page }) => {
  await fixture(page);
  await page.goto('/logs');
  await page.getByRole('button', { name: /Database connection refused/ }).click();
  await expect(page.locator('.event-details')).toContainText('prod-eu-03');
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.getByRole('button', { name: 'Копировать ссылку', exact: true }).click();
  await expect(page.getByText('Ссылка скопирована.', { exact: true })).toBeVisible();
  const shared = await page.evaluate(() => navigator.clipboard.readText());
  expect(new URL(shared).searchParams.get('selected')).toBe(events[0].id);
  await page.goto(shared);
  await expect(page.locator('.event-details')).toContainText(events[0].message);
  await page.getByRole('button', { name: 'Открыть Trace', exact: true }).click();
  await expect(page.locator('.row-button')).toHaveCount(4);
  await expect(page).toHaveURL(new RegExp(`trace_id=${traceId}`));
  await expect(page.getByRole('button', { name: 'Payment retry 3', exact: true })).toBeVisible();
  await expect(page.getByRole('heading', { name: 'Связанные логи Trace', exact: true })).toBeVisible();
});

test('analytics shows real aggregates and drills down to level filter', async ({ page }) => {
  const calls = await fixture(page);
  await page.goto('/analytics');
  await expect(page.getByRole('heading', { name: 'Аналитика', exact: true })).toBeVisible();
  await expect(page.locator('.metric').filter({ hasText: 'Доля ERROR' })).toContainText('7%');
  await expect(page.getByRole('button', { name: /ERROR.*70.*7%/ })).toBeVisible();
  await page.getByRole('button', { name: 'Сравнить с прошлым окном' }).click();
  await expect.poll(() => calls.filter((call) => call.path === '/api/stats/summary').length).toBeGreaterThanOrEqual(3);
  await page.locator('.metric').filter({ hasText: 'Доля ERROR' }).getByRole('button').click();
  await expect(page).toHaveURL(/\/logs\?/);
  await expect.poll(() => calls.filter((call) => call.path === '/api/stats/summary').at(-1)?.body.filters)
    .toEqual({ level: ['error'] });
});

test('mobile layout keeps controls and expanded event fields visible', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await fixture(page);
  await page.goto('/logs');
  await page.getByRole('button', { name: /Database connection refused/ }).click();
  await expect(page.getByRole('button', { name: 'Открыть Trace', exact: true })).toBeVisible();
  const overflows = await page.evaluate(() => document.documentElement.scrollWidth > window.innerWidth);
  expect(overflows).toBe(false);
  await page.screenshot({ path: test.info().outputPath('logs-mobile.png'), fullPage: true });
  await page.goto('/analytics');
  await expect(page.getByRole('button', { name: 'charge.create', exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
  await page.screenshot({ path: test.info().outputPath('analytics-mobile.png'), fullPage: true });
});

test('desktop screenshots preserve actual template composition', async ({ page }) => {
  await fixture(page);
  await page.goto('/logs');
  await page.getByRole('button', { name: /Database connection refused/ }).click();
  await page.screenshot({ path: test.info().outputPath('logs-desktop.png'), fullPage: true });
  await page.goto('/analytics');
  await expect(page.locator('.metric').filter({ hasText: 'Доля ERROR' })).toContainText('7%');
  const plotInsets = await page.locator('svg').evaluate((svg) => {
    const outer = svg.getBoundingClientRect();
    const bars = svg.querySelectorAll('.histogram-bar');
    return [bars[0].getBoundingClientRect().left - outer.left,
      outer.right - bars[bars.length - 1].getBoundingClientRect().right];
  });
  expect(Math.max(...plotInsets)).toBeLessThan(8);
  await page.screenshot({ path: test.info().outputPath('analytics-desktop.png'), fullPage: true });
});

test('live pause buffers new events and Space inside search does not toggle it', async ({ page }) => {
  await fixture(page);
  await page.addInitScript(() => {
    class Stream extends EventTarget {
      static CLOSED = 2;
      readyState = 1;
      onerror = null;
      constructor() { super(); (window as any).__testStream = this; }
      close() { this.readyState = 2; }
    }
    (window as any).EventSource = Stream;
  });
  await page.goto('/logs');
  await page.getByRole('button', { name: 'Live выкл', exact: true }).click();
  await expect.poll(() => page.evaluate(() => Boolean((window as any).__testStream))).toBe(true);
  await page.evaluate((batch) => (window as any).__testStream.dispatchEvent(new MessageEvent('logs', { data: JSON.stringify(batch) })), events.slice(0, 2));
  await expect(page.locator('.row-button')).toHaveCount(2);
  await page.getByLabel('Поисковый запрос', { exact: true }).press('Space');
  await expect(page.getByRole('button', { name: 'Пауза', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Пауза', exact: true }).click();
  await page.evaluate((batch) => (window as any).__testStream.dispatchEvent(new MessageEvent('logs', { data: JSON.stringify(batch) })), [events[2]]);
  await expect(page.getByText('На паузе · в буфере 1 строк', { exact: true })).toBeVisible();
  await expect(page.locator('.row-button')).toHaveCount(2);
  await page.getByRole('button', { name: 'Продолжить', exact: true }).click();
  await expect(page.locator('.row-button')).toHaveCount(3);
  await page.evaluate(() => (window as any).__testStream.dispatchEvent(new MessageEvent('lagged', { data: '{}' })));
  await expect(page.getByText(/Live пропустил пачки событий/)).toBeVisible();
});

test('repeated event ids open one occurrence and shared link restores its timestamp', async ({ page }) => {
  const repeated = { ...events[1], id: events[0].id, message: 'Repeated ID second event' };
  await fixture(page, true, [...events.slice(0, 50), repeated]);
  await page.goto('/logs');
  await loadNextPage(page);
  await page.getByRole('button', { name: 'Repeated ID second event', exact: true }).click();
  await expect(page.locator('.row-button[aria-expanded=true]')).toHaveCount(1);
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']);
  await page.getByRole('button', { name: 'Копировать ссылку', exact: true }).click();
  const shared = await page.evaluate(() => navigator.clipboard.readText());
  expect(new URL(shared).searchParams.get('selected_timestamp')).toBe(repeated.timestamp);
  await page.goto(shared);
  await expect(page.getByRole('region', { name: 'Событие из ссылки', exact: true })).toContainText('Repeated ID second event');
  await expect(page.locator('.row-button[aria-expanded=true]')).toHaveCount(0);
});

test('long account name does not overflow the mobile navigation', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await fixture(page);
  await page.route('**/api/me', route => route.fulfill({ json: { username: 'a'.repeat(100), passkeys: 0 } }));
  await page.goto('/settings');
  await expect(page.getByRole('heading', { name: 'Пароль', exact: true })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
});

test('switching to Trace clears occurrence selection when equal ids and timestamps reverse order', async ({ page }) => {
  const first = { ...events[0], message: 'Occurrence A' };
  const second = { ...events[0], message: 'Occurrence B' };
  await fixture(page, true, [second, first]);
  await page.goto('/logs');
  await page.getByRole('button', { name: 'Occurrence A', exact: true }).click();
  await expect(page.locator('.row-button[aria-expanded=true]')).toHaveCount(1);
  await expect(page).toHaveURL(/selected_occurrence=1/);
  await page.getByRole('button', { name: 'Открыть Trace', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Связанные логи Trace', exact: true })).toBeVisible();
  await page.screenshot({ path: test.info().outputPath('trace-desktop.png'), fullPage: true });
  expect(new URL(page.url()).searchParams.has('selected')).toBe(false);
  await expect(page.locator('.row-button[aria-expanded=true]')).toHaveCount(0);
  await expect(page.locator('.row-button').first()).toHaveText('▸Occurrence A');
});

test('invalid URL dates show a recoverable error instead of crashing', async ({ page }) => {
  await fixture(page);
  await page.goto('/logs?from=bad&to=bad');
  await expect(page.getByRole('alert')).toContainText('Диапазон времени в ссылке некорректен');
  await page.getByLabel('Период', { exact: true }).selectOption('1h');
  await expect(page.getByRole('button', { name: /Database connection refused/ })).toBeVisible();
});


test('summary renders seven stacked levels, duration coverage and event action drilldown', async ({ page }) => {
  const calls = await fixture(page);
  await page.goto('/analytics');
  await expect(page.locator('.metric').filter({ hasText: 'p95 duration_ms' })).toContainText(/5\s012 мс/);
  await expect(page.locator('.metric').filter({ hasText: 'p95 duration_ms' })).toContainText('Покрытие 80% · 800 измерений · приблизительно t-digest');
  for (const level of ['trace', 'debug', 'info', 'warn', 'error', 'fatal', 'unknown']) {
    await expect(page.locator(`.histogram-bar[data-level=${level}]`)).toHaveCount(12);
  }
  await expect(page.locator('.histogram-bucket').first()).toHaveAttribute('aria-label', /TRACE 1.*DEBUG 1.*INFO 59.*WARN 18.*ERROR 6.*FATAL 2.*UNKNOWN 1/);
  await page.getByText('Числа диаграммы', { exact: true }).click();
  await expect(page.locator('.histogram-data th')).toHaveCount(9);
  await expect(page.locator('.actions-table')).toContainText('Нет измерений');
  await page.getByRole('button', { name: 'charge.create', exact: true }).click();
  await expect.poll(() => calls.filter(call => call.path === '/api/stats/summary').at(-1)?.body.filters).toEqual({ action: ['charge.create'] });
  expect(calls.filter(call => call.path === '/api/stats/histogram')).toHaveLength(0);
});

test('empty summary uses zeros and no duration measurements; failed refresh clears old values', async ({ page }) => {
  await fixture(page);
  let failed = false;
  await page.route('**/api/stats/summary', route => route.fulfill(failed
    ? { status: 503, json: { error: 'query_unavailable' } }
    : { json: { as_of: new Date(now).toISOString(), total: 0, buckets: [], levels: Object.fromEntries(['trace', 'debug', 'info', 'warn', 'error', 'fatal', 'unknown'].map(level => [level, 0])),
      error_services: [], duration: { p95_ms: null, measured: 0, coverage: 0, method: 'tdigest' }, actions: [], page: null } }));
  await page.goto('/analytics');
  await expect(page.locator('.metric').filter({ hasText: 'Всего событий' }).getByRole('button')).toHaveText('0');
  await expect(page.locator('.metric').filter({ hasText: 'p95 duration_ms' })).toContainText('Нет измерений');
  await expect(page.locator('.metric').filter({ hasText: 'p95 duration_ms' })).toContainText('Покрытие 0%');
  await expect(page.getByText('Нет данных для диаграммы.', { exact: true })).toBeVisible();
  failed = true;
  await page.getByRole('button', { name: 'Обновить', exact: true }).click();
  await expect(page.getByRole('alert')).toContainText('Поиск временно недоступен');
  await expect(page.locator('.metric').filter({ hasText: 'Всего событий' })).toContainText('Нет данных');
  await expect(page.locator('.metric').filter({ hasText: 'p95 duration_ms' })).not.toContainText('Покрытие 0%');
});

test('obsolete summary cannot replace a newer text result and its coupled count', async ({ page }) => {
  await fixture(page);
  let resolveOld: (() => void) | undefined;
  await page.route('**/api/stats/summary', async route => {
    const body = route.request().postDataJSON();
    if (body.text?.query !== 'slow') return route.fallback();
    await new Promise<void>(resolve => { resolveOld = resolve; });
    await route.fulfill({ json: { as_of: new Date(now).toISOString(), total: 999, buckets: [], levels: {}, duration: {}, error_services: [], actions: [],
      page: { events: [{ ...events[0], message: 'Obsolete result' }], next_cursor: null, as_of: new Date(now).toISOString() } } }).catch(() => {});
  });
  await page.goto('/logs');
  await expect(page.locator('.row-button')).toHaveCount(50);
  await page.getByLabel('Поисковый запрос', { exact: true }).fill('slow');
  await page.getByRole('button', { name: 'Найти', exact: true }).click();
  await expect.poll(() => Boolean(resolveOld)).toBe(true);
  await page.getByLabel('Поисковый запрос', { exact: true }).fill('"connection refused"');
  await page.getByRole('button', { name: 'Найти', exact: true }).click();
  await expect(page.getByText('В диапазоне 1', { exact: true })).toBeVisible();
  resolveOld!();
  await expect(page.getByRole('button', { name: /Database connection refused/ })).toBeVisible();
  await expect(page.getByText('В диапазоне 999', { exact: true })).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Obsolete result', exact: true })).toHaveCount(0);
});


test('90-day history clamps summary interval to one day', async ({ page }) => {
  const calls = await fixture(page);
  const to = '2026-10-14T00:00:00.000Z';
  const from = new Date(Date.parse(to) - 90 * 86400000).toISOString();
  await page.goto(`/logs?${new URLSearchParams({ from, to })}`);
  await expect(page.locator('.row-button')).toHaveCount(50);
  expect(calls.filter(call => call.path === '/api/stats/summary').at(-1)?.body.interval_seconds).toBe(86400);
  expect(calls.filter(call => call.path === '/api/logs/search')).toHaveLength(0);
  await expect(page.getByText('В диапазоне 70', { exact: true })).toBeVisible();
});

test('history beyond the UTC bucket cap keeps cursor search without inventing totals or a chart', async ({ page }) => {
  const calls = await fixture(page);
  // 10000 days plus one nanosecond needs 10001 UTC buckets despite Date.parse truncation.
  const from = '2000-01-01T00:00:00.000Z';
  const to = new Date(Date.parse(from) + 10000 * 86400000).toISOString().replace('.000Z', '.000000001Z');
  await page.goto(`/logs?${new URLSearchParams({ from, to })}`);
  await expect(page.locator('.row-button')).toHaveCount(50);
  await expect(page.getByText(/Сводка недоступна для такого длинного диапазона/)).toBeVisible();
  expect(calls.filter(call => call.path === '/api/stats/summary')).toHaveLength(0);
  await expect(page.getByText(/^В диапазоне /)).toHaveCount(0);
  await expect(page.locator('.histogram')).toHaveCount(0);
  await loadNextPage(page);
  await expect(page.locator('.row-button')).toHaveCount(70);
  expect(calls.filter(call => call.path === '/api/logs/search').at(-1)?.body.cursor).toBe('page-2');
  // The exact exclusive bound still fits, so the summary becomes available again.
  await page.goto(`/logs?${new URLSearchParams({ from, to: to.replace('.000000001Z', '.000Z') })}`);
  await expect(page.getByText('В диапазоне 70', { exact: true })).toBeVisible();
  expect(calls.filter(call => call.path === '/api/stats/summary').at(-1)?.body.interval_seconds).toBe(86400);
});
