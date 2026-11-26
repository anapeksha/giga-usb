import { test, expect } from './fixtures';

// Structural fixture only; intercepted board responses do not authenticate this file.
function otaFixture() {
  const bytes = Buffer.alloc(256 + 0x1e0000 + 4096);
  bytes.write('GIGAOTA1');
  for (const [offset, value] of [
    [8, 1],
    [12, 256],
    [16, 1],
    [20, 10],
    [24, 0x08020000],
    [28, 0x1e0000],
    [32, 0x08100000],
    [36, 4096],
    [104, 0x24080000],
    [108, 0x08020101],
    [112, 0x30020000],
    [116, 0x08100101],
  ])
    bytes.writeUInt32LE(value!, offset);
  bytes.write('candidate-2', 120);
  return bytes;
}

for (const failure of ['none', 'lost commit', 'status hiccup'] as const) {
  test(`sequential upload and exact confirmation: ${failure}`, async ({ page, appURL }) => {
    test.setTimeout(60000);
    const bytes = otaFixture();
    const posts: string[] = [];
    const offsets: number[] = [];
    let state = {
      build_id: 'old',
      phase: 'idle',
      token: null as number | null,
      expires_ms: 100000,
      offset: 0,
      total: 0,
      confirmed: true,
      maintenance: false,
      busy: false,
      cancel_requested: false,
      message: '',
    };
    let pending: typeof state | undefined;
    let boot = 0;
    let hiccup = false;
    await page.route('**/api/update/**', async (route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname.split('/').at(-1)!;
      if (request.method() === 'GET') {
        if (failure === 'status hiccup' && boot === 1 && !hiccup) {
          hiccup = true;
          await route.abort('connectionreset');
          return;
        }
        const reply = { ...state };
        if (pending) {
          state = pending;
          pending = undefined;
        }
        if (boot === 1) {
          state = { ...state, phase: 'checking', confirmed: false };
          boot++;
        } else if (boot === 2) {
          state = { ...state, build_id: 'candidate-2', phase: 'idle', confirmed: false };
          boot++;
        } else if (boot === 3) {
          state = { ...state, confirmed: true };
          boot++;
        }
        await route.fulfill({ json: reply });
        return;
      }
      posts.push(path);
      expect(request.headers()['x-gigausb-update']).toBe('1');
      expect(request.headers()['content-type']).toBe('application/octet-stream');
      if (path === 'authorize') state = { ...state, phase: 'authorized', token: 7 };
      else {
        expect(request.headers()['x-gigausb-token']).toBe('7');
        if (path === 'begin') {
          expect(request.postDataBuffer()).toHaveLength(256);
          state = { ...state, phase: 'receiving', offset: 256, total: bytes.length };
        }
        if (path === 'chunk') {
          const offset = Number(request.headers()['x-gigausb-offset']);
          expect(offset).toBe(state.offset);
          offsets.push(offset);
          const size = request.postDataBuffer()!.length;
          expect(size).toBeLessThanOrEqual(4096);
          const next = {
            ...state,
            offset: offset + size,
            phase: offset + size === bytes.length ? 'ready' : 'receiving',
            busy: false,
          };
          if (offset === 256) {
            state = { ...state, busy: true };
            pending = next;
          } else state = next;
        }
        if (path === 'commit') {
          state = { ...state, phase: failure === 'status hiccup' ? 'idle' : 'committing' };
          boot = 1;
          if (failure === 'lost commit') {
            await route.abort('connectionreset');
            return;
          }
        }
      }
      await route.fulfill({ status: 202, body: '' });
    });
    await page.goto(appURL);
    await page.getByText('Firmware update', { exact: true }).click();
    await page.locator('input[type=file]').setInputFiles({
      name: 'fixture.ota.bin',
      mimeType: 'application/octet-stream',
      buffer: bytes,
    });
    await page.getByRole('button', { name: 'Start upload', exact: true }).click();
    await expect(page.getByText('Board verified the update. Ready to install.')).toBeVisible({
      timeout: 45000,
    });
    expect(posts).not.toContain('commit');
    expect(offsets).toEqual(
      Array.from({ length: Math.ceil((bytes.length - 256) / 4096) }, (_, i) => 256 + i * 4096),
    );
    await page.getByRole('button', { name: 'Install and restart' }).click();
    await expect(page.getByText('New build confirmed.', { exact: true })).toBeVisible();
    expect(posts.filter((path) => path === 'commit')).toHaveLength(1);
  });
}

test('OTA is collapsed and explicit; old firmware gives bootstrap guidance', async ({
  page,
  appURL,
}) => {
  const requests: string[] = [];
  await page.route('**/api/update/**', async (route) => {
    requests.push(route.request().method());
    await route.fulfill({ status: 404, body: '' });
  });
  await page.goto(appURL);
  const card = page
    .locator('details')
    .filter({ has: page.locator('summary', { hasText: 'Firmware update' }) });
  await expect(card).not.toHaveAttribute('open');
  expect(requests).toEqual([]);
  await card.locator('summary').click();
  expect(requests).toEqual([]);
  await expect(page.getByRole('button', { name: 'Check status', exact: true })).toBeEnabled();
  await page.getByRole('button', { name: 'Check status', exact: true }).click();
  await expect(card.getByRole('status')).toContainText('bootstrap firmware');
  expect(requests).toEqual(['GET']);
});

for (const lostAuthorize of [false, true]) {
  test(`explicit cancellation after ${lostAuthorize ? 'lost authorization response' : 'reload and Check status'}`, async ({
    page,
    appURL,
  }) => {
    const requests: string[] = [];
    let state = {
      build_id: 'old',
      phase: 'receiving',
      token: 7 as number | null,
      expires_ms: 10000,
      offset: 256,
      total: 2000000,
      confirmed: true,
      maintenance: true,
      busy: false,
      cancel_requested: false,
      message: '',
    };
    let cancelling = false;
    await page.route('**/api/update/**', async (route) => {
      const request = route.request();
      const path = new URL(request.url()).pathname.split('/').at(-1)!;
      requests.push(`${request.method()} ${path}`);
      if (request.method() === 'GET') {
        const reply = { ...state };
        if (cancelling)
          state = {
            ...state,
            phase: 'cancelled',
            token: null,
            busy: false,
            cancel_requested: false,
          };
        await route.fulfill({ json: reply });
        return;
      }
      if (path === 'authorize' && lostAuthorize) {
        state = { ...state, phase: 'authorized', token: 9 };
        await route.abort('connectionreset');
        return;
      }
      expect(path).toBe('cancel');
      expect(request.headers()['x-gigausb-token']).toBe(lostAuthorize ? '9' : '11');
      state = { ...state, busy: true, cancel_requested: true };
      cancelling = true;
      await route.fulfill({ status: 202, body: '' });
    });
    await page.goto(appURL);
    if (!lostAuthorize) await page.reload();
    await page.getByText('Firmware update', { exact: true }).click();
    if (lostAuthorize) {
      await page.locator('input[type=file]').setInputFiles({
        name: 'fixture.ota.bin',
        mimeType: 'application/octet-stream',
        buffer: otaFixture(),
      });
      await page.getByRole('button', { name: 'Start upload', exact: true }).click();
      await expect(page.getByRole('button', { name: 'Check status', exact: true })).toBeEnabled();
    }
    await page.getByRole('button', { name: 'Check status', exact: true }).click();
    await expect(page.getByRole('button', { name: 'Cancel update', exact: true })).toBeEnabled();
    expect(requests.filter((request) => request === 'POST cancel')).toHaveLength(0);
    // A displayed token must not be reused: cancel fetches the current one.
    if (!lostAuthorize) state = { ...state, token: 11 };
    await page.getByRole('button', { name: 'Cancel update', exact: true }).click();
    await expect(page.getByText('Board confirmed cancellation.', { exact: true })).toBeVisible();
    await expect(page.getByText('Status: cancelled · boot confirmed')).toBeVisible();
    expect(requests.filter((request) => request === 'POST cancel')).toHaveLength(1);
    expect(requests).not.toContain('POST commit');
  });
}

test('status does not poll or authorize in the background', async ({ page, appURL }) => {
  const requests: string[] = [];
  await page.route('**/api/update/**', async (route) => {
    requests.push(route.request().method());
    await route.fulfill({
      json: {
        build_id: 'old',
        phase: 'checking',
        token: null,
        expires_ms: 0,
        offset: 0,
        total: 0,
        confirmed: false,
        maintenance: false,
        busy: false,
        cancel_requested: false,
        message: '',
      },
    });
  });
  await page.goto(appURL);
  await page.getByText('Firmware update', { exact: true }).click();
  await page.getByRole('button', { name: 'Check status', exact: true }).click();
  await expect(page.getByText('Current build: old')).toBeVisible();
  await expect(page.getByText('Status: checking · boot not confirmed')).toBeVisible();
  await page.waitForTimeout(1100);
  expect(requests).toEqual(['GET']);
  await expect(page.getByRole('button', { name: 'Install and restart' })).toBeDisabled();
});
