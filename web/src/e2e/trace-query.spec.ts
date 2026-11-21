import { test, expect } from './fixtures';
import type { RecordEvent } from '../lib/protocol';

const packet: RecordEvent = {
  type: 'host',
  event: { transfer: { endpoint: 129, direction: 'in', pid: 2, bytes: 2, data: [1, 2] } },
};

test('filter reconnect preserves canonical cursor and resets only trace history', async ({
  page,
  api,
  appURL,
}) => {
  const legacyPolls: string[] = [];
  page.on('request', (request) => {
    if (new URL(request.url()).pathname === '/api/trace') legacyPolls.push(request.url());
  });
  api.emit(packet);
  await page.goto(appURL);
  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();

  const admittedTrace = () =>
    api.admittedRequests.filter((params) => params.get('trace') === 'true');
  const search = page.getByRole('searchbox', { name: 'Search USB trace' });
  await search.fill('old');
  await expect.poll(() => admittedTrace().at(-1)?.get('q')).toBe('old');
  await expect(search).toBeEnabled();
  await search.fill('received');
  await expect.poll(() => admittedTrace().at(-1)?.get('q')).toBe('received');

  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole('region', { name: 'USB trace records' }).getByText('Received · endpoint 129'),
  ).toBeVisible();
  api.endStreams();
  await expect.poll(() => admittedTrace().length).toBe(3);
  expect(admittedTrace().at(-1)?.get('after')).toBe('1');
  expect(admittedTrace().at(-1)?.get('trace_after')).toBe('1');
  api.emit(packet);
  await expect.poll(() => api.deliveredCursors.at(-1)).toBe(2);
  await expect(
    page.getByText('2 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  await search.fill('');
  await expect(
    page.getByText('2 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  expect(api.cursors.filter((cursor) => cursor === 0)).toHaveLength(1);
  expect(admittedTrace().map((params) => params.get('trace_after'))).toEqual(['0', '0', '1']);
  await expect.poll(() => api.activeStreams).toBe(1);
  expect(legacyPolls).toEqual([]);
});

test('filtered display trusts streamed server authority and pause keeps canonical ingestion', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet);
  await page.goto(appURL);
  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  api.tracePage = () => ({
    records: [{ sequence: 1, record: api.records[0] }],
    cursor: api.records.length,
    dropped: 0,
  });
  await page
    .getByRole('searchbox', { name: 'Search USB trace' })
    .fill('cannot possibly match this packet');
  await expect.poll(() => api.traceRequests.length).toBe(1);
  await expect(
    page.getByRole('region', { name: 'USB trace records' }).getByText('Received · endpoint 129'),
  ).toBeVisible();
  await page.getByRole('button', { name: 'Pause display', exact: true }).click();
  await expect(page.getByRole('searchbox', { name: 'Search USB trace' })).toBeDisabled();
  await expect(page.getByRole('spinbutton', { name: 'Endpoint', exact: true })).toBeDisabled();
  api.emit(packet);
  await expect.poll(() => api.deliveredCursors.at(-1)).toBe(2);
  await page.getByRole('button', { name: 'Resume display', exact: true }).click();
  await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('');
  await expect(
    page.getByText('2 records · newest first · board time', { exact: true }),
  ).toBeVisible();
});

test('filtered stream HTTP failures automatically reconnect without resetting canonical history', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet);
  await page.goto(appURL);
  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  api.traceStatus = 503;
  await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('received endpoint 129');
  await expect(page.getByRole('alert').filter({ hasText: 'HTTP 503' })).toBeVisible();
  await expect.poll(() => api.traceRequests.length).toBeGreaterThan(1);
  api.emit(packet);
  api.traceStatus = 200;
  await expect(
    page.getByText('2 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  expect(api.cursors.slice(1).every((cursor) => cursor === 1)).toBe(true);
});

test('filtered loss snapshot remains separate from canonical history', async ({
  page,
  api,
  appURL,
}) => {
  for (let index = 0; index < 4; index++) api.emit(packet);
  await page.goto(appURL);
  await expect(
    page.getByText('4 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  api.tracePage = () => ({
    records: [{ sequence: 1, record: api.records[0] }],
    cursor: 4,
    dropped: 11,
  });
  await page.getByRole('spinbutton', { name: 'Endpoint', exact: true }).fill('129');
  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText('Earlier board history is no longer available.', { exact: true }),
  ).toBeVisible();
  await page.getByRole('spinbutton', { name: 'Endpoint', exact: true }).fill('');
  await expect(
    page.getByText('4 records · newest first · board time', { exact: true }),
  ).toBeVisible();
});
