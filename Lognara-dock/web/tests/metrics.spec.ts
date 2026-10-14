import { expect, test, type Page } from '@playwright/test';

const metric = { service: 'billing', name: 'requests', origin: 'counter', first_seen: '2026-01-01T00:00:00Z',
  last_seen: '2026-10-15T12:00:00Z', definition: null, environments: ['prod', 'stage'] };

async function fixture(page: Page) {
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/me') return route.fulfill({ json: { username: 'liis', passkeys: 0 } });
    if (path === '/api/series') return route.fulfill({ json: { metrics: [metric] } });
    if (path === '/api/series/query') return route.fulfill({ json: {
      kind: 'delta', aggregation: 'sum', unit: 'requests',
      buckets: ['2026-10-15T11:00:00Z', '2026-10-15T12:00:00Z'],
      series: [{ label: 'billing', values: [10, 20] }], as_of: '2026-10-15T12:00:00Z',
    } });
    if (path === '/api/series/preview') return route.fulfill({ json: { scanned: 2, matched: 1, samples: [
      { timestamp: '2026-10-15T12:00:00Z', value: 12, labels: '{}', message: 'duration=12' },
    ] } });
    if (path === '/api/series/definitions') return route.fulfill({ status: 201, json: { id: 'new-definition' } });
    return route.fulfill({ status: 404, json: { error: 'not_found' } });
  });
}

test('master metrics stay reachable beside design pages and pass filters to core', async ({ page }) => {
  await fixture(page);
  await page.goto('/metrics');
  for (const name of ['Логи', 'Ошибки', 'Аналитика', 'Инфраструктура', 'Метрики']) {
    await expect(page.getByRole('navigation').getByRole('link', { name, exact: true })).toBeVisible();
  }
  await page.getByRole('button', { name: 'requests счётчик' }).click();
  await expect(page.locator('.uplot canvas')).toBeVisible();
  const filtered = page.waitForRequest(r => r.url().endsWith('/api/series/query') && r.postDataJSON().environment === 'prod');
  await page.getByLabel('Окружение', { exact: true }).selectOption('prod');
  expect((await filtered).postDataJSON()).toMatchObject({ service: 'billing', name: 'requests', environment: 'prod', split: 'none' });
  const split = page.waitForRequest(r => r.url().endsWith('/api/series/query') && r.postDataJSON().split === 'instance');
  await page.getByLabel('Разбиение').selectOption('instance');
  expect((await split).postDataJSON()).toMatchObject({ environment: 'prod', split: 'instance' });
  const all = page.waitForRequest(r => r.url().endsWith('/api/series/query') && !('environment' in r.postDataJSON()));
  await page.getByLabel('Окружение', { exact: true }).selectOption('');
  await all;
});

test('metric editor preserves master preview and definition payloads on mobile', async ({ page }) => {
  await fixture(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/metrics');
  await page.getByRole('button', { name: 'requests счётчик' }).click();
  await expect(page.locator('.uplot canvas')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  await page.getByRole('button', { name: 'Новая метрика из текста логов' }).click();
  await page.getByLabel('Сервис', { exact: true }).fill('billing');
  await page.getByLabel('Имя', { exact: true }).fill('latency');
  await page.getByLabel('Подстрока для отбора строк').fill('duration=');
  await page.getByLabel('Регулярное выражение').fill('duration=(?<ms>\\d+)');
  await page.getByLabel(/^Группа значения/).fill('ms');
  await page.getByLabel('Окружение (необязательно)', { exact: true }).fill('prod');
  const preview = page.waitForRequest(r => r.url().endsWith('/api/series/preview'));
  await page.getByRole('button', { name: 'Проверить', exact: true }).click();
  expect((await preview).postDataJSON()).toMatchObject({ service: 'billing', environment: 'prod', minutes: 60, rule: { value_group: 'ms' } });
  await expect(page.getByText('просмотрено 2, совпало 1')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(390);
  const save = page.waitForRequest(r => r.url().endsWith('/api/series/definitions'));
  await page.getByRole('button', { name: 'Сохранить', exact: true }).click();
  expect((await save).postDataJSON()).toMatchObject({ service: 'billing', name: 'latency', enabled: true,
    rule: { contains: 'duration=', pattern: 'duration=(?<ms>\\d+)', value_group: 'ms', environment: 'prod' } });
  await expect(page.locator('.metric-editor')).toHaveCount(0);
});
