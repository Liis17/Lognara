import { expect, test, type Page } from '@playwright/test';

const createdAt = '2026-10-19T10:00:00Z';
const token = `lna_${'a'.repeat(64)}`;
const relayToken = `lnr_${'b'.repeat(64)}`;

async function fixture(page: Page, empty = false) {
  const state = {
    relays: empty ? [] : [
      { id: 1, name: 'relay-eu', created_at: createdAt },
      { id: 2, name: 'relay-us', created_at: createdAt },
      { id: 3, name: 'relay-empty', created_at: createdAt },
    ],
    agents: empty ? [] : [
      { id: 1, name: 'api', relay_id: 1, created_at: createdAt },
      { id: 2, name: 'worker', relay_id: 1, created_at: createdAt },
      { id: 3, name: 'api', relay_id: 2, created_at: createdAt },
    ],
    writes: [] as { path: string; body: unknown }[],
    failLoad: false,
    failRotate: false,
  };
  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    if (path === '/api/me') return route.fulfill({ json: { username: 'liis', passkeys: 0 } });
    if (request.method() === 'GET') {
      if (state.failLoad) return route.fulfill({ status: 502, json: { error: 'core_unavailable' } });
      if (path === '/api/relays') return route.fulfill({ json: { relays: state.relays } });
      if (path === '/api/agents') return route.fulfill({ json: { agents: state.agents } });
    }
    state.writes.push({ path, body: request.postData() });
    if (path.endsWith('/rotate-token')) {
      if (state.failRotate) return route.fulfill({ status: 502, json: { error: 'core_unavailable' } });
      const id = Number(path.split('/')[3]);
      return route.fulfill({ json: { agent: state.agents.find((agent) => agent.id === id), token } });
    }
    if (request.method() === 'POST' && path === '/api/relays') {
      const relay = { id: 4, name: request.postDataJSON().name, created_at: createdAt };
      state.relays.push(relay);
      return route.fulfill({ status: 201, json: { relay, token: relayToken } });
    }
    if (request.method() === 'POST' && path === '/api/agents') {
      const agent = { id: 4, ...request.postDataJSON(), created_at: createdAt };
      state.agents.push(agent);
      return route.fulfill({ status: 201, json: { agent, token } });
    }
    if (request.method() === 'DELETE') {
      const id = Number(path.split('/')[3]);
      if (path.startsWith('/api/agents/')) state.agents = state.agents.filter((agent) => agent.id !== id);
      else state.relays = state.relays.filter((relay) => relay.id !== id);
      return route.fulfill({ status: 204 });
    }
    return route.fulfill({ status: 404, json: { error: 'not_found' } });
  });
  await page.goto('/access');
  await expect(page.getByRole('heading', { name: 'Relay и агенты' })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Добавить relay', exact: true })).toBeDisabled();
  if (!empty) await expect(page.getByRole('article')).toHaveCount(3);
  return state;
}

test('groups agent tiles by relay and keeps both creation forms before the list', async ({ page }) => {
  await fixture(page);
  const eu = page.getByRole('region', { name: 'Relay relay-eu', exact: true });
  await expect(eu.getByRole('article')).toHaveCount(2);
  await expect(eu).toContainText('Агентов: 2');
  await expect(page.getByRole('region', { name: 'Relay relay-us', exact: true }).getByRole('article')).toHaveCount(1);
  await expect(page.getByRole('region', { name: 'Relay relay-empty', exact: true })).toContainText('пока нет агентов');
  const forms = await page.locator('.access-create').boundingBox();
  const list = await eu.boundingBox();
  expect(forms!.y + forms!.height).toBeLessThan(list!.y);
  const relayForm = await page.getByRole('form', { name: 'Создать relay', exact: true }).boundingBox();
  const agentForm = await page.getByRole('form', { name: 'Создать агента', exact: true }).boundingBox();
  expect(relayForm!.y).toBe(agentForm!.y);
});

test('creates relay, selects it for the next agent and blocks issuing another secret', async ({ page }) => {
  const state = await fixture(page, true);
  await expect(page.getByLabel('Relay агента')).toBeDisabled();
  await page.getByLabel('Имя нового relay').fill('new-relay');
  await page.getByRole('button', { name: 'Добавить relay', exact: true }).click();
  await expect(page.getByLabel('Строка для .env')).toHaveValue(`LOGNARA_RELAY_CORE_TOKEN=${relayToken}`);
  await expect(page.getByLabel('Relay агента')).toHaveValue('4');
  await page.getByLabel('Имя нового агента').fill('new-agent');
  await expect(page.getByRole('button', { name: 'Добавить агента', exact: true })).toBeDisabled();
  await page.getByRole('button', { name: 'Я сохранил токен' }).click();
  await page.getByRole('button', { name: 'Добавить агента', exact: true }).click();
  await expect(page.getByLabel('Строка для .env')).toHaveValue(`LOGNARA_AGENT_TOKEN=${token}`);
  await expect(page.getByRole('region', { name: 'Relay new-relay' }).getByRole('article')).toContainText('new-agent');
  expect(JSON.parse(state.writes[1].body as string)).toEqual({ name: 'new-agent', relay_id: 4 });
  await expect(page.getByRole('button', { name: 'Перевыпустить токен' })).toBeDisabled();
});

test('rotation requires confirmation, copies exact values and permits partial selection', async ({ page, context }) => {
  // Clipboard is browser-global; keep its assertions within this single test.
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);
  const state = await fixture(page);
  const rotate = page.getByRole('region', { name: 'Relay relay-eu', exact: true })
    .getByRole('article', { name: 'Агент api', exact: true }).getByRole('button', { name: 'Перевыпустить токен' });
  page.once('dialog', async (dialog) => {
    expect(dialog.message()).toContain('«api»');
    expect(dialog.message()).toContain('после синхронизации relay');
    await dialog.dismiss();
  });
  await rotate.click();
  expect(state.writes).toHaveLength(0);
  page.once('dialog', (dialog) => dialog.accept());
  await rotate.click();
  await expect(page.getByRole('region', { name: 'Выданный токен' })).toContainText('Токен агента «api» перевыпущен');
  expect(state.writes).toEqual([{ path: '/api/agents/1/rotate-token', body: null }]);
  const field = page.getByLabel('Строка для .env');
  await expect(field).toHaveValue(`LOGNARA_AGENT_TOKEN=${token}`);
  const bounds = (await field.boundingBox())!;
  await page.mouse.move(bounds.x + 14, bounds.y + 22);
  await page.mouse.down();
  await page.mouse.move(bounds.x + 70, bounds.y + 22, { steps: 8 });
  await page.mouse.up();
  const selected = await field.evaluate((el: HTMLTextAreaElement) => el.selectionEnd - el.selectionStart);
  expect(selected).toBeGreaterThan(0);
  expect(selected).toBeLessThan(token.length);
  await expect(field).toHaveAttribute('readonly', '');
  await page.getByRole('button', { name: 'Скопировать строку .env' }).click();
  await expect(page.getByRole('status')).toHaveText('Строка .env скопирована.');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(`LOGNARA_AGENT_TOKEN=${token}`);
  await page.getByRole('button', { name: 'Скопировать токен', exact: true }).click();
  await expect(page.getByRole('status')).toHaveText('Токен скопирован.');
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(token);
  await expect(rotate).toBeDisabled();
  await page.getByRole('button', { name: 'Я сохранил токен' }).click();
  await expect(rotate).toBeEnabled();
  await expect(field).toHaveCount(0);
});

test('keeps an issued token after list refresh failure and reports clipboard denial', async ({ page }) => {
  const state = await fixture(page);
  await page.evaluate(() => {
    Object.defineProperty(navigator, 'clipboard', { value: { writeText: () => Promise.reject(new Error('denied')) } });
  });
  state.failLoad = true;
  page.once('dialog', (dialog) => dialog.accept());
  await page.getByRole('button', { name: 'Перевыпустить токен' }).first().click();
  await expect(page.locator('.error')).toBeVisible();
  await expect(page.getByLabel('Строка для .env')).toHaveValue(`LOGNARA_AGENT_TOKEN=${token}`);
  await page.getByRole('button', { name: 'Скопировать токен', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('Выделите нужный текст');
  expect(state.writes).toHaveLength(1);
});

test('failed rotation allows explicit retry without automatic repeats', async ({ page }) => {
  const state = await fixture(page);
  state.failRotate = true;
  page.on('dialog', (dialog) => dialog.accept());
  const rotate = page.getByRole('button', { name: 'Перевыпустить токен' }).first();
  await rotate.click();
  await expect(page.locator('.error')).toBeVisible();
  await expect(rotate).toBeEnabled();
  expect(state.writes).toHaveLength(1);
  await expect(page.getByLabel('Строка для .env')).toHaveCount(0);
  state.failRotate = false;
  await rotate.click();
  await expect(page.getByLabel('Строка для .env')).toHaveValue(`LOGNARA_AGENT_TOKEN=${token}`);
  expect(state.writes).toHaveLength(2);
});

test('deletes an agent and empty relay with confirmation', async ({ page }) => {
  const state = await fixture(page);
  page.on('dialog', (dialog) => dialog.accept());
  const eu = page.getByRole('region', { name: 'Relay relay-eu', exact: true });
  await eu.getByRole('article', { name: 'Агент worker' }).getByRole('button', { name: 'Удалить', exact: true }).click();
  await expect(eu.getByRole('article')).toHaveCount(1);
  const empty = page.getByRole('region', { name: 'Relay relay-empty', exact: true });
  await empty.getByRole('button', { name: 'Удалить relay' }).click();
  await expect(empty).toHaveCount(0);
  expect(state.writes.map((write) => write.path)).toEqual(['/api/agents/2', '/api/relays/3']);
});

test('mobile layout stacks forms and tiles without horizontal overflow', async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 812 });
  await fixture(page);
  const relayForm = await page.getByRole('form', { name: 'Создать relay', exact: true }).boundingBox();
  const agentForm = await page.getByRole('form', { name: 'Создать агента', exact: true }).boundingBox();
  const list = await page.locator('.access-relays').boundingBox();
  expect(relayForm!.y + relayForm!.height).toBeLessThan(agentForm!.y);
  expect(agentForm!.y + agentForm!.height).toBeLessThan(list!.y);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(375);
  const tiles = page.getByRole('region', { name: 'Relay relay-eu', exact: true }).getByRole('article');
  const first = await tiles.nth(0).boundingBox();
  const second = await tiles.nth(1).boundingBox();
  expect(first!.y + first!.height).toBeLessThan(second!.y);
});
