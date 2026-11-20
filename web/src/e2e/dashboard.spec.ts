import AxeBuilder from '@axe-core/playwright';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { test, expect } from './fixtures';

const setup = [128, 6, 0, 1, 0, 0, 18, 0];

test('gzip bundle renders dark, keyboard-operable and accessible', async ({
  page,
  api,
  appURL,
}) => {
  await page.emulateMedia({ colorScheme: 'light' });
  const response = await page.goto(appURL);
  expect(response?.headers()['content-encoding']).toBe('gzip');
  await expect(page.getByRole('heading', { name: 'GigaUSB', exact: true })).toBeVisible();
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  expect(api.cursors.length).toBeGreaterThan(0);
  expect(
    await page.locator('html').evaluate((element) => getComputedStyle(element).colorScheme),
  ).toBe('dark');
  await page.keyboard.press('Tab');
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeFocused();
  expect(
    await page.locator('body').evaluate((body) => getComputedStyle(body).backgroundColor),
  ).not.toBe('rgb(255, 255, 255)');
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
});

test('status failure does not block polling; acceptance is not execution and queue-full is not retried', async ({
  page,
  api,
  appURL,
}) => {
  api.commandStatus = 503;
  await page.goto(appURL);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  await expect(page.getByRole('status')).toContainText('queue full');
  api.commandStatus = 202;
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.getByRole('status')).toHaveText('Request accepted.');
  await expect(page.getByText('No device reported', { exact: true })).toBeVisible();
  api.commandStatus = 503;
  await page.getByRole('button', { name: 'Stop', exact: true }).click();
  await expect(page.getByRole('status')).toContainText('queue full');
  const polls = api.cursors.length;
  await expect.poll(() => api.cursors.length).toBeGreaterThan(polls + 2);
  expect(api.commands).toEqual(['status', 'start', 'stop']);
});

test('connecting can be cancelled while requests are pending', async ({ page, api, appURL }) => {
  api.pending = true;
  await page.goto(appURL);
  await page.getByRole('button', { name: 'Cancel connection' }).click();
  await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
  await expect(page.getByRole('status')).toHaveText('Disconnected.');
  api.pending = false;
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
});

test('trace filters decode fixture records; reconnect resets cursor without replay duplicates', async ({
  page,
  api,
  appURL,
}) => {
  api.emit({
    type: 'host',
    event: {
      transfer_context: {
        session_id: 1,
        transfer_id: 1,
        endpoint: 0,
        kind: 'control',
        stage: 'setup',
        elapsed_us: 3,
        retries: 0,
      },
    },
  });
  api.emit({
    type: 'host',
    event: { transfer: { endpoint: 0, direction: 'out', bytes: 8, pid: 3, data: setup } },
  });
  api.emit({
    type: 'host',
    event: { transfer: { endpoint: 129, direction: 'in', bytes: 2, pid: 2, data: [1, 2] } },
  });
  await page.goto(appURL);
  const trace = page.getByRole('region', { name: 'USB trace records' });
  await expect(trace.getByText('Received · endpoint 129', { exact: true })).toBeVisible();
  await page.getByRole('combobox', { name: 'Endpoint', exact: true }).fill('0');
  await page.getByRole('option', { name: '0', exact: true }).click();
  await page.getByRole('combobox', { name: 'Direction', exact: true }).fill('OUT');
  await expect(page.getByRole('listbox')).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  await page.getByRole('combobox', { name: 'Transfer', exact: true }).fill('Control');
  await page.getByRole('option', { name: 'Control', exact: true }).click();
  await expect(trace.getByText('Received · endpoint 129', { exact: true })).toHaveCount(0);
  const details = trace.getByRole('button', { name: 'Sent · endpoint 0' });
  await details.focus();
  await page.keyboard.press('Enter');
  await expect(details).toHaveAttribute('aria-expanded', 'true');
  await expect(trace.getByText(/Get descriptor/)).toBeVisible();
  await expect(trace.getByText(/RAW BYTES \(8\)/)).toContainText('80 06 00 01 00 00 12 00');
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await page.getByRole('checkbox', { name: 'Errors only' }).focus();
  await page.keyboard.press('Space');
  await expect(page.getByRole('checkbox', { name: 'Errors only' })).toBeChecked();
  await expect(trace.getByText(/No matching events/)).toBeVisible();
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  api.cursors.length = 0;
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect.poll(() => api.cursors.length).toBeGreaterThan(1);
  expect(api.cursors[0]).toBe(0);
  await expect(
    page.getByText('3 records · newest first · board time', { exact: true }),
  ).toBeVisible();
});

test('partial topology survives a failure, with accessible rendered descriptors', async ({
  page,
  api,
  appURL,
}) => {
  api.emit({ type: 'host', event: { lifecycle: { state: 'enumerating', session_id: 1 } } });
  api.emit({
    type: 'interface',
    descriptor: { number: 0, alternate: 0, class: 3, subclass: 1, protocol: 1, endpoints: 1 },
  });
  api.emit({
    type: 'endpoint',
    descriptor: {
      interface: 0,
      alternate: 0,
      address: 129,
      attributes: 3,
      max_packet_size: 8,
      interval: 10,
    },
  });
  api.emit({ type: 'host', event: { lifecycle: { state: 'error', session_id: 1 } } });
  api.emit({ type: 'host', event: { error: { code: 'stall' } } });
  await page.goto(appURL);
  await page.getByText('Device details', { exact: true }).click();
  await expect(page.getByRole('table', { name: 'USB interfaces', exact: true })).toBeVisible();
  await expect(page.getByRole('cell', { name: '0x81 IN', exact: true })).toBeVisible();
  await expect(page.getByRole('definition').filter({ hasText: /^Error$/ })).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  api.emit({ type: 'host', event: { lifecycle: { state: 'attached', session_id: 2 } } });
  await expect(page.getByRole('cell', { name: '0x81 IN', exact: true })).toHaveCount(0);
});

test('storage downloads 301 bytes in ordered 128-byte chunks with matching SHA-256', async ({
  page,
  api,
  appURL,
}) => {
  const bytes = Buffer.from(Array.from({ length: 301 }, (_, index) => index % 251));
  api.emit({ type: 'host', event: { attached: { speed: 'full' } } });
  api.emit({
    type: 'host',
    event: {
      storage: {
        vendor: 'Fixture',
        product: 'Read-only',
        blocks: 1024,
        block_size: 512,
        fat32: true,
      },
    },
  });
  api.onCommand = (command) => {
    if (command === 'storage_list')
      api.emit({
        type: 'host',
        event: { file: { index: 0, name: 'TEST.BIN', size: bytes.length, directory: false } },
      });
    if (typeof command === 'object') {
      const { offset } = command.storage_read;
      api.emit({
        type: 'host',
        event: {
          file_data: {
            index: 0,
            offset,
            total_size: bytes.length,
            data: [...bytes.subarray(offset, offset + 128)],
            eof: offset + 128 >= bytes.length,
          },
        },
      });
    }
  };
  await page.goto(appURL);
  await page.getByRole('button', { name: 'List files', exact: true }).click();
  const downloadButton = page.getByRole('button', { name: 'Download TEST.BIN', exact: true });
  await expect(downloadButton).toBeEnabled();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  const pending = page.waitForEvent('download');
  await downloadButton.click();
  const download = await pending;
  expect(download.suggestedFilename()).toBe('TEST.BIN');
  const path = await download.path();
  expect(path).not.toBeNull();
  const actual = await readFile(path!);
  expect(actual.length).toBe(301);
  expect(createHash('sha256').update(actual).digest('hex')).toBe(
    createHash('sha256').update(bytes).digest('hex'),
  );
  expect(api.commands.filter((command) => typeof command === 'object')).toEqual(
    [0, 128, 256].map((offset) => ({ storage_read: { file: 0, offset } })),
  );
});

test('malformed responses show actionable errors and remain accessible', async ({
  page,
  api,
  appURL,
}) => {
  api.malformed = true;
  await page.goto(appURL);
  await expect(page.getByRole('status')).toContainText('Invalid events response');
  await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
});

test('non-USB floods preserve trace, open details and USB-only JSON export', async ({
  page,
  api,
  appURL,
}) => {
  api.emit({
    type: 'host',
    event: { transfer: { endpoint: 129, direction: 'in', bytes: 2, pid: 2, data: [1, 2] } },
  });
  await page.goto(appURL);
  const trace = page.getByRole('region', { name: 'USB trace records' });
  const packet = trace.getByRole('button', { name: 'Received · endpoint 129' });
  await packet.click();
  for (let i = 0; i < 180; i++) {
    api.emit({
      type: 'host',
      event: {
        network: {
          info: { mode: 'setup', mac: [0, 1, 2, 3, 4, 5], ip: '192.168.4.1', error: null },
        },
      },
    });
    api.emit({
      type: 'host',
      event: {
        status: { enabled: true, attached: true, address: 7, transfers: 42, errors: 0, dropped: 0 },
      },
    });
    api.emit({ type: 'host', event: 'ready' });
  }
  await expect.poll(() => api.cursors.at(-1), { timeout: 15000 }).toBe(541);
  await expect(packet).toHaveAttribute('aria-expanded', 'true');
  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole('definition').filter({ hasText: /^7$/ })).toBeVisible();
  api.emit({ type: 'host', event: { error: { code: 'stall' } } });
  await expect(trace.getByRole('button', { name: 'stall', exact: true })).toBeVisible();
  await expect(packet).toHaveAttribute('aria-expanded', 'true');
  const pending = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Export trace', exact: true }).click();
  const download = await pending;
  const exported = JSON.parse(await readFile((await download.path())!, 'utf8'));
  expect(exported.map((r: { event: unknown }) => r.event)).toEqual([
    {
      type: 'host',
      event: { transfer: { endpoint: 129, direction: 'in', bytes: 2, pid: 2, data: [1, 2] } },
    },
    { type: 'host', event: { error: { code: 'stall' } } },
  ]);
  await page.setViewportSize({ width: 390, height: 844 });
  expect(await page.locator('body').evaluate((el) => el.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
});
