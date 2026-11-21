import { readFile } from 'node:fs/promises';
import { randomBytes } from 'node:crypto';
import { test, expect } from './fixtures';

const packet = (endpoint: number, value: number) => ({
  type: 'host' as const,
  event: { transfer: { endpoint, direction: 'in' as const, bytes: 1, pid: 2, data: [value] } },
});

// These are real loopback streams, including admission and the first reset frame.
test('two remembered slots reject active duplicates and resume inactive tokens until eviction', async ({
  api,
  appURL,
}) => {
  api.emit(packet(129, 1));
  const tokens = ['a', 'b', 'c'].map((value) => value.repeat(32));
  const open = async (client: string, resume?: string) => {
    const attempt = randomBytes(16).toString('hex');
    const params = new URLSearchParams({
      client,
      attempt,
      after: '1',
      trace_after: '1',
      trace: 'true',
    });
    if (resume) params.set('resume', resume);
    const response = await fetch(new URL(`/api/events?${params}`, appURL));
    expect(response.status).toBe(200);
    const reader = response.body!.getReader();
    const decoder = new TextDecoder();
    let first = '';
    while (!first.includes('event: trace\n') || !first.endsWith('\n\n')) {
      const chunk = await reader.read();
      if (chunk.done) throw new Error('Stream ended before its initial pages');
      first += decoder.decode(chunk.value, { stream: true });
    }
    return { reader, first, attempt };
  };
  const first = await open(tokens[0]);
  expect(
    first.first.startsWith(
      `event: reset\ndata: ${JSON.stringify({ resume: first.attempt })}\n\nevent: events\n`,
    ),
  ).toBe(true);
  expect(first.first).toContain('"records":[{"timestamp_ms":1');
  expect(first.first).toContain('"sequence":1');
  expect(first.first).toContain('"cursor":1');
  const duplicate = await fetch(
    new URL(
      `/api/events?client=${tokens[0]}&attempt=${randomBytes(16).toString('hex')}&after=1&trace_after=1&trace=false`,
      appURL,
    ),
  );
  expect(duplicate.status).toBe(503);
  const second = await open(tokens[1]);
  expect(second.first).toMatch(/^event: reset\n/);
  const full = await fetch(
    new URL(
      `/api/events?client=${tokens[2]}&attempt=${randomBytes(16).toString('hex')}&after=1&trace_after=1&trace=false`,
      appURL,
    ),
  );
  expect(full.status).toBe(503);
  await first.reader.cancel();
  await expect.poll(() => api.activeStreams).toBe(1);
  const resumed = await open(tokens[0], first.attempt);
  expect(resumed.first).toMatch(/^event: events\n/);
  expect(resumed.first).toContain('"records":[],"cursor":1');
  await resumed.reader.cancel();
  await expect.poll(() => api.activeStreams).toBe(1);
  const resumedAgain = await open(tokens[0], first.attempt);
  expect(resumedAgain.first).toMatch(/^event: events\n/);
  await resumedAgain.reader.cancel();
  await expect.poll(() => api.activeStreams).toBe(1);
  const unacknowledged = await open(tokens[0]);
  expect(unacknowledged.first).toMatch(/^event: reset\n/);
  await unacknowledged.reader.cancel();
  await expect.poll(() => api.activeStreams).toBe(1);
  const stale = await open(tokens[0], first.attempt);
  expect(stale.first).toMatch(/^event: reset\n/);
  await stale.reader.cancel();
  await expect.poll(() => api.activeStreams).toBe(1);
  const replacement = await open(tokens[2]);
  expect(replacement.first).toMatch(/^event: reset\n/);
  await replacement.reader.cancel();
  await expect.poll(() => api.activeStreams).toBe(1);
  const evicted = await open(tokens[0]);
  expect(evicted.first).toMatch(/^event: reset\n/);
  await evicted.reader.cancel();
  await second.reader.cancel();
  await expect.poll(() => api.activeStreams).toBe(0);
});

test('continuity token survives filter changes and retries but changes on manual Connect', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet(129, 1));
  await page.goto(appURL);
  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  const token = api.requests[0].get('client');
  expect(token).toMatch(/^[0-9a-f]{32}$/);
  await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('received');
  await expect.poll(() => api.admittedRequests.length).toBe(2);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  api.endStreams();
  await expect.poll(() => api.admittedRequests.length).toBe(3);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  api.breakStreams();
  await expect.poll(() => api.admittedRequests.length).toBe(4);
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  expect(api.requests.every((params) => params.get('client') === token)).toBe(true);
  const attempts = api.requests.map((params) => params.get('attempt'));
  for (const attempt of attempts) expect(attempt).toMatch(/^[0-9a-f]{32}$/);
  expect(new Set(attempts).size).toBe(attempts.length);
  expect(api.requests[0].has('resume')).toBe(false);
  expect(api.requests.slice(1).every((params) => params.get('resume') === attempts[0])).toBe(true);
  expect(api.resets).toEqual([token]);
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await expect.poll(() => api.activeStreams).toBe(0);
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  const next = api.requests.at(-1)!.get('client');
  expect(next).toMatch(/^[0-9a-f]{32}$/);
  expect(next).not.toBe(token);
  expect(api.requests.at(-1)!.has('resume')).toBe(false);
  expect(api.resets).toEqual([token, next]);
});

for (const dropReset of [false, true]) {
  test(`reset ${dropReset ? 'lost before processing' : 'received'} replaces old state when new history exceeds the old cursor, preserving filters and pause`, async ({
    page,
    api,
    appURL,
  }) => {
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
    api.emit(packet(129, 77));
    api.emit({
      type: 'host',
      event: {
        status: { enabled: true, attached: true, address: 7, transfers: 42, errors: 5, dropped: 3 },
      },
    });
    await page.goto(appURL);
    await page.getByText('Device details', { exact: true }).click();
    await expect(page.getByRole('cell', { name: '0x81 IN', exact: true })).toBeVisible();
    await page.getByRole('spinbutton', { name: 'Endpoint', exact: true }).fill('129');
    await expect.poll(() => api.traceRequests.length).toBe(1);
    await expect(
      page.getByText('1 records · newest first · board time', { exact: true }),
    ).toBeVisible();
    await page.getByRole('button', { name: 'Pause display', exact: true }).click();
    const token = api.requests[0].get('client');
    const oldCursor = api.records.length;
    const oldProof = api.requests[0].get('attempt');
    const admissions = api.admittedRequests.length;
    api.dropNextReset = dropReset;
    api.reboot();
    for (let value = 0; value < 9; value++) api.emit(packet(130, value));
    expect(api.records.length).toBeGreaterThan(oldCursor);
    await expect.poll(() => api.resets.length).toBe(2);
    await expect.poll(() => api.deliveredCursors.at(-1)).toBe(9);
    expect(api.requests.at(-1)!.get('after')).toBe(String(oldCursor));
    expect(api.requests.at(-1)!.get('client')).toBe(token);
    const rebootRequests = api.admittedRequests.slice(admissions);
    expect(rebootRequests).toHaveLength(dropReset ? 2 : 1);
    for (const params of rebootRequests) {
      expect(params.get('resume')).toBe(oldProof);
      expect(params.get('after')).toBe(String(oldCursor));
      expect(params.get('trace_after')).toBe(String(oldCursor));
    }
    expect(api.droppedResets).toHaveLength(dropReset ? 1 : 0);
    if (dropReset) {
      expect(api.droppedResets[0]).toBe(rebootRequests[0]);
      expect(rebootRequests[1].get('attempt')).not.toBe(rebootRequests[0].get('attempt'));
    }
    const processedProof = rebootRequests.at(-1)!.get('attempt');
    await expect(page.getByRole('button', { name: 'Resume display', exact: true })).toBeVisible();
    await expect(page.getByRole('spinbutton', { name: 'Endpoint', exact: true })).toHaveValue(
      '129',
    );
    await page.getByRole('button', { name: 'Resume display', exact: true }).click();
    await expect(
      page.getByText('0 records · newest first · board time', { exact: true }),
    ).toBeVisible();
    await expect(page.getByRole('cell', { name: '0x81 IN', exact: true })).toHaveCount(0);
    const counters = page.getByRole('region', { name: 'Trace counters' });
    await expect(counters.getByText('9', { exact: true })).toBeVisible();
    await expect(counters.getByText('42', { exact: true })).toHaveCount(0);
    await expect(counters.getByText('5', { exact: true })).toHaveCount(0);
    await expect(counters.getByText('3', { exact: true })).toHaveCount(0);
    await page.getByRole('spinbutton', { name: 'Endpoint', exact: true }).fill('');
    await expect(
      page.getByText('9 records · newest first · board time', { exact: true }),
    ).toBeVisible();
    const pending = page.waitForEvent('download');
    await page.getByRole('button', { name: 'Export trace', exact: true }).click();
    const download = await pending;
    const records = JSON.parse(await readFile((await download.path())!, 'utf8'));
    expect(
      records.map(({ timestamp_ms, event }: { timestamp_ms: number; event: unknown }) => ({
        timestamp_ms,
        event,
      })),
    ).toEqual(api.records);
    expect(api.requests.at(-1)!.get('after')).toBe('9');
    expect(api.requests.at(-1)!.get('resume')).toBe(processedProof);
  });
}

for (const interruption of ['transport loss', 'new board'] as const) {
  test(`pending download is cancelled on ${interruption} and late file data cannot complete it`, async ({
    page,
    api,
    appURL,
  }) => {
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
          event: { file: { index: 0, name: 'TEST.BIN', size: 1, directory: false } },
        });
    };
    await page.goto(appURL);
    await page.getByRole('button', { name: 'List files', exact: true }).click();
    const button = page.getByRole('button', { name: 'Download TEST.BIN', exact: true });
    let downloads = 0;
    page.on('download', () => downloads++);
    await button.click();
    await expect(button).toHaveAttribute('aria-busy', 'true');
    await expect
      .poll(() => api.commands.filter((command) => typeof command === 'object').length)
      .toBe(1);
    const admissions = api.admittedRequests.length;
    if (interruption === 'new board') api.reboot();
    else api.breakStreams();
    await expect(
      page.locator('[data-sonner-toast]').filter({ hasText: 'download cancelled' }),
    ).toBeVisible();
    await expect.poll(() => api.admittedRequests.length).toBe(admissions + 1);
    await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
    api.emit({
      type: 'host',
      event: { file_data: { index: 0, offset: 0, total_size: 1, data: [99], eof: true } },
    });
    await page.waitForTimeout(300);
    expect(downloads).toBe(0);
    expect(api.commands.filter((command) => typeof command === 'object')).toHaveLength(1);
  });
}
