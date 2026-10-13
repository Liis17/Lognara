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

type Call = { path: string; body: Record<string, any> };
async function fixture(page: Page, initiallyAuthenticated = true) {
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
    if (path === '/api/account/password') return reply({});
    if (path === '/api/relays') return reply({ relays: [{ id: 1, name: 'relay-eu', created_at: new Date(now).toISOString() }] });
    if (path === '/api/agents') return reply({ agents: [] });
    if (path === '/api/logs/search') {
      let rows = events;
      for (const [field, values] of Object.entries(body.filters ?? {})) {
        rows = rows.filter((event) => (values as string[]).includes(String(event[field as keyof typeof event])));
      }
      if (body.text) rows = rows.filter((event) => event.message.toLowerCase().includes(body.text.query.toLowerCase()));
      const offset = body.cursor === 'page-2' ? 50 : 0;
      return reply({ events: rows.slice(offset, offset + body.limit), next_cursor: offset + body.limit < rows.length ? 'page-2' : null, as_of: new Date(now).toISOString() });
    }
    if (path.startsWith('/api/traces/')) return reply({ events: events.slice(0, 4).reverse(), next_cursor: null, as_of: new Date(now).toISOString() });
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
  await page.getByLabel('Текущий пароль', { exact: true }).fill('test-password');
  await page.getByLabel('Новый пароль', { exact: true }).fill('new-long-password');
  await page.getByLabel('Повторите новый пароль', { exact: true }).fill('new-long-password');
  await page.getByRole('button', { name: 'Сменить пароль', exact: true }).click();
  await expect(page.getByText('Пароль изменён, остальные сессии завершены')).toBeVisible();
});

test('history filter uses typed core query, cursor and text-scoped counts', async ({ page }) => {
  const calls = await fixture(page);
  await page.goto('/logs');
  await expect(page.getByRole('button', { name: /Database connection refused/ })).toBeVisible();
  await page.getByLabel('Поисковый запрос', { exact: true }).fill('service:billing AND level:(ERROR OR WARN)');
  await page.getByRole('button', { name: 'Найти', exact: true }).click();
  await expect.poll(() => calls.filter((call) => call.path === '/api/logs/search').at(-1)?.body.filters)
    .toEqual({ service: ['billing'], level: ['error', 'warn'] });
  await page.getByRole('button', { name: /Загрузить ещё/ }).click();
  await expect.poll(() => calls.filter((call) => call.path === '/api/logs/search').at(-1)?.body.cursor).toBe('page-2');
  await expect(page.getByRole('button', { name: /Payment retry 69/ })).toBeVisible();
  await page.getByLabel('Поисковый запрос', { exact: true }).fill('"connection refused"');
  await page.getByRole('button', { name: 'Найти', exact: true }).click();
  await expect.poll(() => calls.filter((call) => call.path === '/api/logs/search').at(-1)?.body.text)
    .toEqual({ query: 'connection refused', mode: 'phrase' });
  await expect(page.getByText(/текст.*(подсчёт|гистограмм)|гистограмм.*текст/i)).toBeVisible();
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
  await expect.poll(() => calls.filter((call) => call.path === '/api/stats/histogram').length).toBeGreaterThanOrEqual(3);
  await page.locator('.metric').filter({ hasText: 'Доля ERROR' }).getByRole('button').click();
  await expect(page).toHaveURL(/\/logs\?/);
  await expect.poll(() => calls.filter((call) => call.path === '/api/logs/search').at(-1)?.body.filters)
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
});

test('desktop screenshots preserve actual template composition', async ({ page }) => {
  await fixture(page);
  await page.goto('/logs');
  await page.getByRole('button', { name: /Database connection refused/ }).click();
  await page.screenshot({ path: test.info().outputPath('logs-desktop.png'), fullPage: true });
  await page.goto('/analytics');
  await expect(page.locator('.metric').filter({ hasText: 'Доля ERROR' })).toContainText('7%');
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
