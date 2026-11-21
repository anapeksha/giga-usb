import { readFile } from 'node:fs/promises';
import { test, expect } from './fixtures';
import type { RecordEvent } from '../lib/protocol';

const packet = (value: number): RecordEvent => ({
  type: 'host',
  event: { transfer: { endpoint: 129, direction: 'in', bytes: 1, pid: 2, data: [value] } },
});
const label = (count: number) => `${count} records · newest first · board time`;

test('healthy idle holds one real stream through heartbeats; EOF resumes without duplicates', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet(1));
  await page.goto(appURL);
  await expect(page.getByText(label(1), { exact: true })).toBeVisible();
  await page.waitForTimeout(4300);
  expect(api.cursors).toEqual([0]);
  expect(api.activeStreams).toBe(1);
  api.endStreams();
  await expect.poll(() => api.cursors).toEqual([0, 1]);
  api.emit(packet(2));
  await expect(page.getByText(label(2), { exact: true })).toBeVisible();
  const pending = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Export trace', exact: true }).click();
  const download = await pending;
  expect(
    JSON.parse(await readFile((await download.path())!, 'utf8')).map(
      (record: { timestamp_ms: number }) => record.timestamp_ms,
    ),
  ).toEqual([1, 2]);
});

test('stream ingests a paused backlog once across three-record pages', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet(0));
  await page.goto(appURL);
  await expect(page.getByText(label(1), { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Pause display', exact: true }).click();
  for (let index = 1; index < 65; index++) api.emit(packet(index));
  await expect.poll(() => api.deliveredCursors.at(-1)).toBe(65);
  await expect(page.getByText(`${label(1)} · display paused`, { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Resume display', exact: true }).click();
  await expect(page.getByText(label(65), { exact: true })).toBeVisible();
  expect(api.cursors).toEqual([0]);
});

for (const phase of ['headers', 'read', 'backoff'] as const) {
  test(`cancel during ${phase} prevents further stream requests`, async ({ page, api, appURL }) => {
    api.pending = phase === 'headers';
    if (phase === 'backoff') api.eventsStatus = 503;
    await page.goto(appURL);
    await expect.poll(() => api.cursors.length).toBeGreaterThan(0);
    if (phase === 'read')
      await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
    if (phase === 'backoff')
      await expect(
        page.getByRole('alert').filter({ hasText: 'Connection interrupted. Reconnecting' }),
      ).toBeVisible();
    await page
      .getByRole('button', {
        name: phase === 'read' ? 'Disconnect' : 'Cancel connection',
        exact: true,
      })
      .click();
    await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
    const requests = api.cursors.length;
    await page.waitForTimeout(1200);
    expect(api.cursors).toHaveLength(requests);
    await expect.poll(() => api.activeStreams).toBe(0);
  });
}

test('stalled body times out and automatically resumes the committed cursor', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet(1));
  await page.goto(appURL);
  await expect(page.getByText(label(1), { exact: true })).toBeVisible();
  api.stalled = true;
  await expect(
    page.getByRole('alert').filter({ hasText: 'Connection interrupted. Reconnecting' }),
  ).toBeVisible({
    timeout: 12000,
  });
  api.stalled = false;
  await expect.poll(() => api.cursors.length, { timeout: 10000 }).toBeGreaterThan(1);
  expect(api.cursors[1]).toBe(1);
  api.emit(packet(2));
  await expect(page.getByText(label(2), { exact: true })).toBeVisible();
});

test('transient HTTP failure reconnects automatically until cancelled', async ({
  page,
  api,
  appURL,
}) => {
  api.eventsStatus = 503;
  await page.goto(appURL);
  await expect(
    page.getByRole('alert').filter({ hasText: 'Connection interrupted. Reconnecting' }),
  ).toBeVisible();
  await expect.poll(() => api.cursors.length).toBeGreaterThan(1);
  api.eventsStatus = 200;
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
});

test('two loopback clients retain independent streams while commands remain responsive', async ({
  page,
  context,
  api,
  appURL,
}) => {
  await page.goto(appURL);
  const other = await context.newPage();
  await other.goto(appURL);
  await expect.poll(() => api.activeStreams).toBe(2);
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await other.getByRole('button', { name: 'Stop', exact: true }).click();
  await expect
    .poll(() => api.commands.includes('start') && api.commands.includes('stop'))
    .toBe(true);
  api.emit(packet(1));
  for (const client of [page, other])
    await expect(client.getByText(label(1), { exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await expect.poll(() => api.activeStreams).toBe(1);
  api.emit(packet(2));
  await expect(other.getByText(label(2), { exact: true })).toBeVisible();
  await other.close();
});

test('broken TCP stream reconnects without replaying committed records', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet(1));
  await page.goto(appURL);
  await expect(page.getByText(label(1), { exact: true })).toBeVisible();
  api.breakStreams();
  await expect.poll(() => api.cursors).toEqual([0, 1]);
  api.emit(packet(2));
  await expect(page.getByText(label(2), { exact: true })).toBeVisible();
});

test('malformed stream is fatal and waits for explicit Connect', async ({ page, api, appURL }) => {
  api.malformed = true;
  await page.goto(appURL);
  await expect(
    page.getByRole('alert').filter({ hasText: 'Invalid events response' }),
  ).toBeVisible();
  await expect(page.getByRole('button', { name: 'Connect', exact: true })).toBeVisible();
  await page.waitForTimeout(1200);
  expect(api.cursors).toEqual([0]);
  api.malformed = false;
  await page.getByRole('button', { name: 'Connect', exact: true }).click();
  await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  expect(api.cursors).toEqual([0, 0]);
});
