import { readFile } from 'node:fs/promises';
import type { Page } from '@playwright/test';
import type { ProtocolRecord } from '../lib/protocol';
import { test, expect } from './fixtures';

function gate() {
  let release!: () => void;
  const wait = new Promise<void>((resolve) => (release = resolve));
  return { wait, release };
}

const packet = (value: number): ProtocolRecord['event'] => ({
  type: 'host',
  event: {
    transfer: { endpoint: 129, direction: 'in', bytes: 1, pid: 2, data: [value] },
  },
});

async function exported(page: Page): Promise<ProtocolRecord[]> {
  const download = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Export trace', exact: true }).click();
  return JSON.parse(await readFile((await (await download).path())!, 'utf8'));
}

const countLabel = (count: number) => `${count} records · newest first · board time`;

test('polling caps persistent full-page bursts and continues cursors without overlap', async ({
  page,
  api,
  appURL,
}) => {
  expect(api.records).toHaveLength(0);
  const eighth = gate();
  const seventeenth = gate();
  const requests: { cursor: number; started: number; finished: number }[] = [];
  let active = 0;
  let maximumActive = 0;
  await page.route('**/api/events?*', async (route) => {
    const cursor = Number(new URL(route.request().url()).searchParams.get('after'));
    const entry = { cursor, started: Date.now(), finished: 0 };
    requests.push(entry);
    active++;
    maximumActive = Math.max(maximumActive, active);
    try {
      if (requests.length === 8) await eighth.wait;
      if (requests.length === 17) await seventeenth.wait;
      entry.finished = Date.now();
      await route.fulfill({
        json: {
          records: Array.from({ length: 3 }, (_, index) => ({
            timestamp_ms: cursor + index + 1,
            event: packet((cursor + index) % 256),
          })),
          cursor: cursor + 3,
          dropped: 0,
        },
      });
    } finally {
      active--;
    }
  });
  try {
    await page.goto(appURL);
    await expect.poll(() => requests.length).toBe(8);
    // Hold a full page across interval ticks: no parallel drain may start.
    await page.waitForTimeout(250);
    expect(requests).toHaveLength(8);
    await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('responsive');
    await expect(page.getByRole('searchbox', { name: 'Search USB trace' })).toHaveValue(
      'responsive',
    );
    eighth.release();
    await expect.poll(() => requests.length).toBe(17);
    expect(requests.map(({ cursor }) => cursor)).toEqual(
      Array.from({ length: 17 }, (_, index) => index * 3),
    );
    // Generous lower bound, not an exact wall-clock assertion for the 150 ms interval.
    expect(requests[8].started - requests[7].finished).toBeGreaterThanOrEqual(75);
    expect(requests[16].started - requests[15].finished).toBeGreaterThanOrEqual(75);
    expect(maximumActive).toBe(1);
    await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  } finally {
    eighth.release();
    seventeenth.release();
  }
});

test('polling ingests a paused backlog once in order across page and batch boundaries', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet(0));
  await page.goto(appURL);
  await expect(page.getByText(countLabel(1), { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Pause display', exact: true }).click();
  for (let index = 1; index < 65; index++) {
    if (index === 24) {
      api.emit({
        type: 'host',
        event: {
          transfer_context: {
            session_id: 1,
            transfer_id: 42,
            endpoint: 0,
            kind: 'control',
            stage: 'setup',
            elapsed_us: 3,
            retries: 0,
          },
        },
      });
    } else if (index === 25) {
      api.emit({
        type: 'host',
        event: {
          transfer: {
            endpoint: 0,
            direction: 'out',
            bytes: 8,
            pid: 3,
            data: [128, 6, 0, 1, 0, 0, 18, 0],
          },
        },
      });
    } else api.emit(packet(index));
  }
  await expect.poll(() => api.cursors.at(-1)).toBe(65);
  await expect(page.getByText(`${countLabel(1)} · display paused`, { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Resume display', exact: true }).click();
  await expect(page.getByText(countLabel(65), { exact: true })).toBeVisible();
  expect(
    (await exported(page)).map(({ timestamp_ms, event }) => ({ timestamp_ms, event })),
  ).toEqual(api.records);
  const trace = page.getByRole('region', { name: 'USB trace records' });
  await trace.evaluate((element) => {
    element.scrollTop = element.scrollHeight / 2;
  });
  await expect(trace.getByRole('button', { name: 'transfer context', exact: true })).toBeVisible();
  await expect(trace.getByRole('button', { name: 'Sent · endpoint 0', exact: true })).toBeVisible();
});

test('polling retains and exports a successful partial batch after a 503', async ({
  page,
  api,
  appURL,
}) => {
  for (let index = 0; index < 3; index++) api.emit(packet(index));
  const cursors: number[] = [];
  await page.route('**/api/events?*', async (route) => {
    const cursor = Number(new URL(route.request().url()).searchParams.get('after'));
    cursors.push(cursor);
    if (cursor === 0) {
      await route.fulfill({ json: { records: api.records, cursor: 3, dropped: 0 } });
    } else await route.fulfill({ status: 503 });
  });
  await page.goto(appURL);
  await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
  await expect(
    page
      .locator('[data-sonner-toast]')
      .filter({ hasText: 'Connection failed: Events failed: HTTP 503' }),
  ).toBeVisible();
  await expect(page.getByText(countLabel(3), { exact: true })).toBeVisible();
  expect(
    (await exported(page)).map(({ timestamp_ms, event }) => ({ timestamp_ms, event })),
  ).toEqual(api.records);
  expect(cursors).toEqual([0, 3]);
});

test('polling discards an old held mid-batch response after reconnect', async ({
  page,
  api,
  appURL,
}) => {
  for (let index = 0; index < 3; index++) api.emit(packet(index));
  const held = gate();
  const cursors: number[] = [];
  let oldResponseFinished = false;
  await page.route('**/api/events?*', async (route) => {
    const cursor = Number(new URL(route.request().url()).searchParams.get('after'));
    cursors.push(cursor);
    if (cursors.length === 2) {
      await held.wait;
      await route.fulfill({
        json: { records: [{ timestamp_ms: 999, event: packet(255) }], cursor: 4, dropped: 0 },
      });
      oldResponseFinished = true;
      return;
    }
    const records = api.records.slice(cursor, cursor + 3);
    await route.fulfill({ json: { records, cursor: cursor + records.length, dropped: 0 } });
  });
  try {
    await page.goto(appURL);
    await expect.poll(() => cursors.length).toBe(2);
    await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
    api.records.length = 0;
    api.emit(packet(77));
    await page.getByRole('button', { name: 'Connect', exact: true }).click();
    await expect(page.getByText(countLabel(1), { exact: true })).toBeVisible();
    expect(cursors[2]).toBe(0);
    held.release();
    await expect.poll(() => oldResponseFinished).toBe(true);
    const polls = cursors.length;
    await expect.poll(() => cursors.length).toBeGreaterThan(polls);
    expect(
      (await exported(page)).map(({ timestamp_ms, event }) => ({ timestamp_ms, event })),
    ).toEqual(api.records);
    await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  } finally {
    held.release();
  }
});
