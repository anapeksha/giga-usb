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
  await expect(page.getByRole('heading', { name: /^Giga\s?USB$/ })).toBeVisible();
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

test('status failure does not block streaming; acceptance is not execution and queue-full is not retried', async ({
  page,
  api,
  appURL,
}) => {
  api.commandStatus = 503;
  await page.goto(appURL);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  await expect(
    page.locator('[data-sonner-toast]').filter({ hasText: 'queue full' }).last(),
  ).toBeVisible();
  api.commandStatus = 202;
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(
    page.locator('[data-sonner-toast]').filter({ hasText: 'Request queued.' }),
  ).toBeVisible();
  await expect(page.getByText('No device reported', { exact: true })).toBeVisible();
  api.commandStatus = 503;
  await page.getByRole('button', { name: 'Stop', exact: true }).click();
  await expect(
    page.locator('[data-sonner-toast]').filter({ hasText: 'queue full' }).last(),
  ).toBeVisible();
  const polls = api.cursors.length;
  await page.waitForTimeout(300);
  expect(api.cursors).toHaveLength(polls);
  expect(api.commands).toEqual(['status', 'start', 'stop']);
});

test('connecting can be cancelled while requests are pending', async ({ page, api, appURL }) => {
  api.pending = true;
  await page.goto(appURL);
  const cancel = page.getByRole('button', { name: 'Cancel connection' });
  await expect(cancel).toHaveAttribute('aria-busy', 'true');
  await expect(cancel).toBeEnabled();
  await cancel.click();
  await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
  await expect(page.locator('[data-sonner-toast]')).toHaveText('Disconnected.');
  for (const region of [
    page.getByLabel('USB host status', { exact: true }),
    page.getByRole('region', { name: 'Trace counters' }),
  ]) {
    await expect(region).toHaveAttribute('aria-busy', 'false');
    await expect(region.locator('[aria-hidden="true"]')).toHaveCount(0);
  }
  await expect(page.getByRole('status').filter({ hasText: /^Loading / })).toHaveCount(0);
  api.pending = false;
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
});

for (const status of [202, 503]) {
  test(`status ${status} leaves connection loading until the first event page`, async ({
    page,
    api,
    appURL,
  }) => {
    api.commandStatus = status;
    const release = api.holdEvents();
    await page.goto(appURL);
    await expect.poll(() => api.commands).toEqual(['status']);
    await expect.poll(() => api.cursors.length).toBe(1);
    const host = page.getByLabel('USB host status', { exact: true });
    const counters = page.getByRole('region', { name: 'Trace counters' });
    const connected = page.locator('[data-sonner-toast]').filter({ hasText: /^Connected\.$/ });
    await expect(page.getByRole('button', { name: 'Cancel connection' })).toHaveAttribute(
      'aria-busy',
      'true',
    );
    await expect(page.getByText('Connecting', { exact: true })).toBeVisible();
    for (const region of [host, counters]) {
      await expect(region).toHaveAttribute('aria-busy', 'true');
      await expect(region.locator('[aria-hidden="true"]').first()).toBeVisible();
    }
    for (const text of [
      'Loading device details…',
      'Loading diagnostics and storage…',
      'Loading USB trace…',
    ])
      await expect(page.getByRole('status').filter({ hasText: text })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeVisible();
    await expect(connected).toHaveCount(0);
    if (status === 503) {
      await expect(
        page.locator('[data-sonner-toast]').filter({ hasText: 'queue full' }),
      ).toBeVisible();
      await expect(host).toHaveAttribute('aria-busy', 'true');
    }
    release();
    const disconnect = page.getByRole('button', { name: 'Disconnect', exact: true });
    await expect(disconnect).toBeVisible();
    await expect(disconnect).not.toHaveAttribute('aria-busy', 'true');
    for (const region of [host, counters]) {
      await expect(region).toHaveAttribute('aria-busy', 'false');
      await expect(region.locator('[aria-hidden="true"]')).toHaveCount(0);
    }
    await expect(page.getByText('Connected', { exact: true })).toBeVisible();
    await expect(page.getByRole('status').filter({ hasText: /^Loading / })).toHaveCount(0);
    const polls = api.cursors.length;
    await page.waitForTimeout(300);
    expect(api.cursors).toHaveLength(polls);
    await expect(page.getByText('Connected', { exact: true })).toBeVisible();
  });
}

for (const failure of ['incompatible hello']) {
  test(`${failure} clears initial loading without reporting connection success`, async ({
    page,
    api,
    appURL,
  }) => {
    if (failure === 'events unavailable') api.eventsStatus = 503;
    if (failure === 'incompatible hello') api.emit({ type: 'hello', protocol_version: 0 });
    if (failure === 'request deadline') api.pending = true;
    await page.goto(appURL);
    if (failure === 'request deadline') {
      await expect(page.getByRole('button', { name: 'Cancel connection' })).toHaveAttribute(
        'aria-busy',
        'true',
      );
    }
    const connect = page.getByRole('button', { name: 'Connect', exact: true });
    await expect(connect).toBeVisible({ timeout: 12000 });
    await expect(connect).toBeEnabled();
    await expect(connect).not.toHaveAttribute('aria-busy', 'true');
    for (const region of [
      page.getByLabel('USB host status', { exact: true }),
      page.getByRole('region', { name: 'Trace counters' }),
    ]) {
      await expect(region).toHaveAttribute('aria-busy', 'false');
      await expect(region.locator('[aria-hidden="true"]')).toHaveCount(0);
    }
    await expect(page.locator('[data-sonner-toast]').last()).toBeVisible();
    await expect(page.getByRole('alert')).toBeVisible();
    await expect(
      page.locator('[data-sonner-toast]').filter({ hasText: /^Connected\.$/ }),
    ).toHaveCount(0);
    await expect(page.getByText('Connecting', { exact: true })).toHaveCount(0);
  });
}

test('browser offline events do not pause local AP commands or reconnection', async ({
  page,
  api,
  appURL,
}) => {
  await page.goto(appURL);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event('offline')));
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect.poll(() => api.commands.includes('start')).toBe(true);
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  const polls = api.cursors.length;
  const statuses = api.commands.filter((command) => command === 'status').length;
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  await expect.poll(() => api.cursors.length).toBeGreaterThan(polls);
  await expect
    .poll(() => api.commands.filter((command) => command === 'status').length)
    .toBe(statuses + 1);
  await page.evaluate(() => window.dispatchEvent(new Event('online')));
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
  const search = page.getByRole('searchbox', { name: 'Search USB trace', exact: true });
  const context = trace.getByRole('button', { name: 'transfer context', exact: true });
  const setupPacket = trace.getByRole('button', { name: 'Sent · endpoint 0', exact: true });
  for (const query of ['80 06', '  gEt DeScRiPtOr  ']) {
    await search.fill(query);
    await expect.poll(() => api.traceRequests.at(-1)?.get('q')).toBe(query.trim());
    await expect(setupPacket).toBeVisible();
    await expect(context).toBeVisible();
    await expect(trace.getByText('Received · endpoint 129', { exact: true })).toHaveCount(0);
  }
  await setupPacket.click();
  await expect(trace.getByText(/Get descriptor/)).toBeVisible();
  await context.click();
  await expect(trace.locator('pre').filter({ hasText: '"transfer_context"' })).toContainText(
    '"transfer_id": 1',
  );
  await setupPacket.click();
  await search.fill('  RECEIVED ENDPOINT 129  ');
  await expect(trace.getByText('Received · endpoint 129', { exact: true })).toBeVisible();
  await expect(context).toHaveCount(0);
  await expect(setupPacket).toHaveCount(0);
  const exportedDownload = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Export trace', exact: true }).click();
  const exported = JSON.parse(await readFile((await (await exportedDownload).path())!, 'utf8'));
  expect(exported.map((record: { event: unknown }) => record.event)).toEqual(
    api.records.map((record) => record.event),
  );
  await search.fill('');
  await expect(context).toBeVisible();
  await expect(setupPacket).toBeVisible();
  await expect(trace.getByText('Received · endpoint 129', { exact: true })).toBeVisible();
  await page.getByRole('spinbutton', { name: 'Endpoint', exact: true }).fill('0');
  await page.getByRole('spinbutton', { name: 'Endpoint', exact: true }).press('Tab');
  await page.getByRole('combobox', { name: 'Direction', exact: true }).fill('OUT');
  await expect(page.getByRole('listbox')).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await page.getByRole('option', { name: 'OUT', exact: true }).click();
  await expect(page.getByRole('combobox', { name: 'Direction', exact: true })).toHaveValue('OUT');
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
  await expect(page.getByRole('checkbox', { name: 'Errors only' })).toBeEnabled();
  await page.getByRole('checkbox', { name: 'Errors only' }).focus();
  await page.keyboard.press('Space');
  await expect(page.getByRole('checkbox', { name: 'Errors only' })).toBeChecked();
  await expect(trace.getByText(/No matching events/)).toBeVisible();
  await expect
    .poll(() => Object.fromEntries(api.traceRequests.at(-1) ?? []))
    .toMatchObject({ endpoint: '0', direction: 'out', kind: 'control', errors: 'true' });
  const colors = await page.getByRole('checkbox', { name: 'Errors only' }).evaluate((input) => {
    const root = input.parentElement!;
    const control = root.querySelector('.size-4')!;
    const style = getComputedStyle(control);
    return {
      border: style.borderColor,
      background: style.backgroundColor,
      label: getComputedStyle(root.querySelector('label')!).color,
    };
  });
  expect(colors.border).toBe(colors.background);
  expect(colors.label).not.toBe(colors.background);
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  api.cursors.length = 0;
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect.poll(() => api.cursors.length).toBeGreaterThan(0);
  expect(api.cursors[0]).toBe(0);
  await expect(
    page.getByText('0 records · newest first · board time', { exact: true }),
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
    if (command === 'storage_list') {
      api.emit({
        type: 'host',
        event: { file: { index: 1, name: 'OTHER.BIN', size: 1, directory: false } },
      });
      api.emit({
        type: 'host',
        event: { file: { index: 0, name: 'TEST.BIN', size: bytes.length, directory: false } },
      });
    }
    if (typeof command === 'object' && 'storage_read' in command) {
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
  const release = api.holdCommand({ storage_read: { file: 0, offset: 0 } });
  const pending = page.waitForEvent('download');
  await downloadButton.click();
  await expect(downloadButton).toHaveAttribute('aria-busy', 'true');
  await expect(page.getByRole('button', { name: 'Stop', exact: true })).toBeEnabled();
  const other = page.getByRole('button', { name: 'Download OTHER.BIN', exact: true });
  await expect(other).toBeDisabled();
  await expect(other).not.toHaveAttribute('aria-busy', 'true');
  await expect(page.getByRole('button', { name: 'List files', exact: true })).toBeDisabled();
  release();
  const download = await pending;
  await expect(downloadButton).not.toHaveAttribute('aria-busy', 'true');
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

test('FAT32 file tree is keyboard collapsible and keeps unsupported entries read-only', async ({
  page,
  api,
  appURL,
}) => {
  api.emit({ type: 'host', event: { attached: { speed: 'full' } } });
  api.emit({
    type: 'host',
    event: {
      storage: { vendor: 'Fixture', product: 'FAT32', blocks: 1024, block_size: 512, fat32: true },
    },
  });
  api.onCommand = (command) => {
    if (command !== 'storage_list') return;
    for (const file of [
      { index: 0, name: '<b>.TXT', size: 0, directory: false },
      { index: 1, name: 'DOCS', size: 0, directory: true },
      { index: 2, name: 'LARGE.BIN', size: 65537, directory: false },
      { index: 3, name: 'LIMIT.BIN', size: 65536, directory: false },
    ])
      api.emit({ type: 'host', event: { file } });
  };
  await page.setViewportSize({ width: 320, height: 800 });
  await page.goto(appURL);
  const root = page.getByRole('button', { name: 'FAT32 root', exact: true });
  await expect(root).toHaveAttribute('aria-expanded', 'true');
  await page.getByRole('button', { name: 'List files', exact: true }).click();
  const files = page.getByRole('list', { name: 'Root files', exact: true });
  await expect(files.getByRole('button', { name: 'Download <b>.TXT', exact: true })).toBeEnabled();
  await expect(files.locator('b')).toHaveCount(0);
  await expect(
    files.getByRole('button', { name: 'Folder DOCS, unavailable', exact: true }),
  ).toBeDisabled();
  await expect(
    files.getByRole('button', { name: 'Download LARGE.BIN', exact: true }),
  ).toBeDisabled();
  await expect(
    files.getByRole('button', { name: 'Download LIMIT.BIN', exact: true }),
  ).toBeEnabled();
  const commands = api.commands.length;
  await root.focus();
  await page.keyboard.press('Space');
  await expect(root).toHaveAttribute('aria-expanded', 'false');
  await expect(files).not.toBeVisible();
  await page.keyboard.press('Space');
  await expect(files).toBeVisible();
  expect(api.commands).toHaveLength(commands);
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
});

test('malformed responses show actionable errors and remain accessible', async ({
  page,
  api,
  appURL,
}) => {
  api.malformed = true;
  await page.goto(appURL);
  await expect(
    page.getByRole('alert').filter({ hasText: 'Invalid events response' }),
  ).toBeVisible();
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
  await expect.poll(() => api.deliveredCursors.at(-1), { timeout: 15000 }).toBe(541);
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

test('pending commands mark only the clicked button busy, keep streaming and reset after errors', async ({
  page,
  api,
  appURL,
}) => {
  await page.goto(appURL);
  const start = page.getByRole('button', { name: 'Start', exact: true });
  const stop = page.getByRole('button', { name: 'Stop', exact: true });
  await expect(start).toBeEnabled();
  for (const status of [503, 202]) {
    const release = api.holdCommand('start', status);
    await start.click();
    await expect(start).toHaveAttribute('aria-busy', 'true');
    for (const name of ['Start', 'Stop', 'Reset USB', 'Refresh USB status']) {
      const button = page.getByRole('button', { name, exact: true });
      await expect(button).toBeDisabled();
      if (name !== 'Start') await expect(button).not.toHaveAttribute('aria-busy', 'true');
    }
    const polls = api.cursors.length;
    api.emit({ type: 'host', event: 'ready' });
    await expect.poll(() => api.deliveredCursors.at(-1)).toBe(api.records.length);
    expect(api.cursors).toHaveLength(polls);
    release();
    await expect(start).toBeEnabled();
    await expect(start).not.toHaveAttribute('aria-busy', 'true');
    await expect(stop).toBeEnabled();
    if (status === 503) {
      await expect(page.getByRole('alert').filter({ hasText: 'queue full' })).toBeVisible();
    } else {
      await expect(
        page.locator('[data-sonner-toast]').filter({ hasText: 'Request queued.' }),
      ).toBeVisible();
      await expect(page.getByRole('alert').filter({ hasText: 'queue full' })).toHaveCount(0);
    }
  }
  expect(api.commands).toEqual(['status', 'start', 'start']);
});

for (const width of [320, 375, 1280]) {
  test(`endpoint bounds, keyboard tooltip and persistent errors are accessible at ${width}px`, async ({
    page,
    api,
    appURL,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    for (const endpoint of [129, 130]) {
      api.emit({
        type: 'host',
        event: { transfer: { endpoint, direction: 'in', bytes: 1, pid: 2, data: [1] } },
      });
    }
    await page.goto(appURL);
    const disconnect = page.getByRole('button', { name: 'Disconnect', exact: true });
    await disconnect.focus();
    await expect(page.getByRole('tooltip')).toContainText(
      'Disconnecting leaves the USB host running',
    );
    await page.keyboard.press('Escape');
    const endpoint = page.getByRole('spinbutton', { name: 'Endpoint', exact: true });
    await endpoint.fill('255');
    await endpoint.press('Tab');
    await expect(endpoint).toHaveValue('255');
    const trace = page.getByRole('region', { name: 'USB trace records' });
    await expect(endpoint).not.toHaveAttribute('aria-invalid', 'true');
    await expect(trace.getByText(/No matching events/)).toBeVisible();
    await expect(
      trace.getByRole('button', { name: 'Received · endpoint 129', exact: true }),
    ).toHaveCount(0);
    for (const invalid of ['256', '-1']) {
      await endpoint.fill(invalid);
      await endpoint.press('Tab');
      await expect(endpoint).toHaveValue(invalid);
      await expect(endpoint).toHaveAttribute('aria-invalid', 'true');
      await expect(
        page.getByText('Enter a whole number from 0 to 255, or clear for all.', { exact: true }),
      ).toBeVisible();
      await expect(page.getByText('No matching events', { exact: false })).toBeVisible();
    }
    await endpoint.fill('0');
    await endpoint.press('Tab');
    await expect(endpoint).toHaveValue('0');
    await expect(endpoint).not.toHaveAttribute('aria-invalid', 'true');
    await endpoint.fill('');
    await endpoint.press('Tab');
    await expect(endpoint).toHaveValue('');
    for (const address of [129, 130]) {
      await expect(
        trace.getByRole('button', { name: `Received · endpoint ${address}`, exact: true }),
      ).toBeVisible();
    }
    await expect(endpoint).not.toHaveAttribute('aria-invalid', 'true');
    await expect(
      page.getByText('Enter a whole number from 0 to 255, or clear for all.', { exact: true }),
    ).toHaveCount(0);
    api.commandStatus = 503;
    await expect(page.getByRole('button', { name: 'Start', exact: true })).toBeEnabled();
    await page.getByRole('button', { name: 'Start', exact: true }).focus();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('alert').filter({ hasText: 'queue full' })).toBeVisible();
    await expect(
      page.locator('[data-sonner-toast]').filter({ hasText: 'queue full' }),
    ).toBeVisible();
    expect(
      await page.locator('body').evaluate((body) => body.scrollWidth <= window.innerWidth),
    ).toBe(true);

    expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  });
}

test('a rejected command from an old connection cannot overwrite the new session', async ({
  page,
  api,
  appURL,
}) => {
  await page.goto(appURL);
  const start = page.getByRole('button', { name: 'Start', exact: true });
  await expect(start).toBeEnabled();
  const release = api.holdCommand('start', 503);
  await start.click();
  await expect(start).toHaveAttribute('aria-busy', 'true');
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await expect(
    page.locator('[data-sonner-toast]').filter({ hasText: 'Disconnected.' }),
  ).toBeVisible();
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(start).toBeEnabled();
  release();
  const polls = api.cursors.length;
  await page.waitForTimeout(300);
  expect(api.cursors).toHaveLength(polls);
  await expect(page.getByRole('alert').filter({ hasText: 'queue full' })).toHaveCount(0);
  await expect(page.locator('[data-sonner-toast]').filter({ hasText: 'queue full' })).toHaveCount(
    0,
  );
  expect(api.commands.filter((command) => command === 'start')).toHaveLength(1);
});
