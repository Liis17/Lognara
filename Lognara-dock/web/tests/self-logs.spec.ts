import { expect, test } from '@playwright/test';

test('settings load, save and report errors without exposing server credentials', async ({ page }) => {
  let settings = { core: true, dock: true, relay: true };
  let fail = false;
  await page.route('**/api/**', async route => {
    const path = new URL(route.request().url()).pathname;
    if (path === '/api/me') return route.fulfill({ json: { username: 'operator', passkeys: 0 } });
    if (path === '/api/account/passkeys') return route.fulfill({ json: { passkeys: [] } });
    if (path === '/api/settings/self-logs') {
      if (route.request().method() === 'GET') return route.fulfill({ json: settings });
      if (fail) return route.fulfill({ status: 503, json: { error: 'core_unavailable' } });
      settings = route.request().postDataJSON();
      return route.fulfill({ status: 204 });
    }
    return route.fulfill({ json: {} });
  });
  await page.goto('/settings');
  await expect(page.getByRole('checkbox', { name: 'lognara-core' })).toBeChecked();
  await page.getByRole('checkbox', { name: 'lognara-relay' }).uncheck();
  await page.getByRole('button', { name: 'Сохранить сбор логов' }).click();
  await expect(page.getByRole('status')).toHaveText('Настройки сохранены');
  expect(settings).toEqual({ core: true, dock: true, relay: false });
  await page.reload();
  await expect(page.getByRole('checkbox', { name: 'lognara-relay' })).not.toBeChecked();
  fail = true;
  await page.getByRole('checkbox', { name: 'lognara-relay' }).check();
  await page.getByRole('button', { name: 'Сохранить сбор логов' }).click();
  await expect(page.locator('.error')).toBeVisible();
  expect(settings.relay).toBe(false);
});
