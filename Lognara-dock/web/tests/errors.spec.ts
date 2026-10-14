import { expect, test, type Page } from '@playwright/test';

const now = Date.now();
const iso = (offset: number) => new Date(now - offset * 60_000).toISOString();
const fp = (n: number) => `f1_${String(n).repeat(64)}`;
const stack = ['System.NullReferenceException: Object reference not set',
  ...Array.from({ length: 14 }, (_, index) => `   at Frame${index}.Run() in /src/Frame${index}.cs:line ${index + 10}`)].join('\n');
const buckets = Array.from({ length: 24 }, (_, index) => ({ timestamp: iso((24 - index) * 60), count: index > 20 ? 1 : 0 }));

function issue(n: number, overrides: Record<string, unknown>) {
  return {
    fingerprint: fp(n), environment: 'prod', service: 'billing', action: 'invoice.sync',
    exception_type: null, frames: [], pattern_id: `p1_${'a'.repeat(64)}`,
    template: 'Timeout after <duration_1>', parts: [{ kind: 'literal', text: 'Timeout after ' }, { kind: 'variable', name: 'duration_1', variable_type: 'duration' }],
    exact: false, count: 3, servers: 1, is_new: false, first_seen: iso(3000), last_seen: iso(5), buckets,
    state: { status: 'open', resolved_at: null, resolved_by: null }, ...overrides,
  };
}

async function fixture(page: Page) {
  const calls: { path: string; body: Record<string, any> }[] = [];
  let resolved = false;
  await page.route('**/api/**', async (route) => {
    const path = new URL(route.request().url()).pathname;
    const body = route.request().postDataJSON() ?? {};
    calls.push({ path, body });
    const reply = (json: unknown, status = 200) => route.fulfill({ status, json });
    if (path === '/api/me') return reply({ username: 'liis', passkeys: 0 });
    if (path === '/api/stats/group-by') return reply({ groups: [{ values: { service: 'billing' }, count: 7 }], as_of: iso(0) });
    const nullRef = issue(1, {
      exception_type: 'System.NullReferenceException', frames: ['Frame0.Run() in /src/Frame0.cs:line '],
      count: 4, servers: 2, is_new: true, first_seen: iso(30),
      buckets: buckets.map((bucket, index) => ({ ...bucket, count: index === 23 ? 4 : 0 })),
      state: resolved ? { status: 'resolved', resolved_at: iso(0), resolved_by: 'liis' } : { status: 'open', resolved_at: null, resolved_by: null },
    });
    if (path === '/api/issues/search') {
      const open = resolved ? [issue(2, {})] : [nullRef, issue(2, {})];
      return reply({ algorithm_version: 1, as_of: iso(0), watermark: 40, core_watermark: 40, history_from: iso(5000), history_complete: true,
        counts: { open: open.length, new: resolved ? 0 : 1, regressed: 0, resolved: resolved ? 1 : 0 },
        status_events: body.status === 'resolved' ? 4 : resolved ? 3 : 7,
        issues: body.status === 'resolved' ? (resolved ? [nullRef] : []) : body.status === 'new' ? (resolved ? [] : [nullRef]) : open });
    }
    if (path === `/api/issues/${fp(1)}/details`) {
      const { state, ...core } = nullRef;
      return reply({ as_of: iso(0), issue: core, servers: [{ value: 'prod-eu-01', count: 3 }, { value: 'prod-eu-02', count: 1 }], other_servers: 0,
        examples: [{ id: '00000000-0000-4000-8000-000000000001', timestamp: iso(5), ingested_at: iso(5), environment: 'prod', server: 'prod-eu-01',
          backend: 'api', service: 'billing', service_instance: null, action: 'invoice.sync', level: 'error', message: 'Mapping failed for invoice 42',
          trace_id: '4bf92f3577b34da6a3ce929d0e0e4736', span_id: null, parent_span_id: null, request_id: null,
          attributes: { 'exception.type': 'System.NullReferenceException', 'exception.stacktrace': stack } }],
        state, history: resolved ? [{ action: 'resolved', actor: 'liis', at: iso(0), event_at: null }] : [] });
    }
    if (path === `/api/issues/${fp(1)}/resolve`) { resolved = true; return route.fulfill({ status: 204 }); }
    return reply({ error: 'not_found' }, 404);
  });
  return calls;
}

test('errors page groups issues, shows stack trace and resolves an issue', async ({ page }) => {
  const calls = await fixture(page);
  await page.goto('/errors');
  await expect(page.getByRole('heading', { name: '7 ошибок → 2 проблемы' })).toBeVisible();
  await expect(page.getByRole('button', { name: /^Открытые\s*2$/ })).toHaveAttribute('aria-pressed', 'true');
  const rows = page.locator('.issues-table tbody tr');
  await expect(rows).toHaveCount(2);
  await expect(rows.first()).toContainText('System.NullReferenceException');
  await expect(rows.first()).toContainText(`billing · invoice.sync · fp ${'1'.repeat(6)}`);
  await expect(rows.first().locator('.issue-status')).toHaveText('Новая');
  await expect(rows.nth(1).locator('.issue-status')).toHaveText('Открыта');
  const search = calls.find((call) => call.path === '/api/issues/search')!;
  expect(search.body).toMatchObject({ status: 'open', limit: 100, interval_seconds: 3600, filters: {} });

  await rows.first().getByRole('button', { expanded: false }).click();
  await expect(page.getByText('Mapping failed for invoice 42')).toBeVisible();
  const trace = page.locator('.issue-stack');
  await expect(trace).toContainText('Frame10.Run()');
  await expect(trace).not.toContainText('Frame11.Run()');
  await page.getByRole('button', { name: '+ 3 строки' }).click();
  await expect(trace).toContainText('Frame13.Run()');
  await expect(page.locator('.issue-servers')).toContainText('prod-eu-02');
  await expect(page.locator('.issue-history')).toContainText('впервые в известной истории');
  const all = page.getByRole('link', { name: 'Все 4 события' });
  expect(decodeURIComponent(await all.getAttribute('href') ?? '')).toContain(`fingerprint:"${fp(1)}"`);
  expect(await page.getByRole('link', { name: 'Поиск первопричины' }).getAttribute('href')).toContain('trace_id=4bf92f3577b34da6a3ce929d0e0e4736');

  await page.getByRole('button', { name: 'Отметить решённой' }).click();
  await expect(page.getByRole('heading', { name: '3 ошибки → 1 проблема' })).toBeVisible();
  expect(calls.some((call) => call.path === `/api/issues/${fp(1)}/resolve`)).toBe(true);
  await page.getByRole('button', { name: /^Решённые\s*1$/ }).click();
  await expect(page).toHaveURL(/status=resolved/);
  await expect(rows.first().locator('.issue-status')).toHaveText('Решена');
  await rows.first().getByRole('button', { expanded: false }).click();
  await expect(page.locator('.issue-history')).toContainText('отмечена решённой · liis');
  await expect(page.getByRole('button', { name: 'Открыть снова' })).toBeVisible();
});

test('errors page fits a narrow screen', async ({ page }) => {
  await fixture(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/errors');
  await expect(page.locator('.issues-table tbody tr')).toHaveCount(2);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
});
