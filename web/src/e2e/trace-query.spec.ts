import { test, expect } from './fixtures';
import type { RecordEvent } from '../lib/protocol';
import type { Route } from '@playwright/test';

test('changing filters discards a late server response without resetting canonical history', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet);
  await page.goto(appURL);
  await expect.poll(() => api.cursors.at(-1)).toBe(1);
  let held: Route | undefined;
  await page.route('**/api/trace?**', async (route) => {
    if (new URL(route.request().url()).searchParams.get('q') === 'old') {
      held = route;
      return;
    }
    await route.fulfill({ json: { records: [], cursor: 1, dropped: 0 } });
  });
  const search = page.getByRole('searchbox', { name: 'Search USB trace' });
  await search.fill('old');
  await expect.poll(() => !!held).toBe(true);
  await search.fill('new');
  await expect(
    page.getByText('0 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  await held!.fulfill({
    json: {
      records: [{ sequence: 1, record: { timestamp_ms: 1, event: packet } }],
      cursor: 1,
      dropped: 0,
    },
  });
  api.emit(packet);
  await expect.poll(() => api.cursors.at(-1)).toBe(2);
  await expect(
    page.getByRole('region', { name: 'USB trace records' }).getByText('Received · endpoint 129'),
  ).toHaveCount(0);
  await search.fill('');
  await expect(
    page.getByText('2 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  expect(api.cursors.filter((cursor) => cursor === 0)).toHaveLength(1);
});

const packet: RecordEvent = {
  type: 'host',
  event: { transfer: { endpoint: 129, direction: 'in', pid: 2, bytes: 2, data: [1, 2] } },
};

test('filtered display trusts server authority; canonical polling continues and filters pause with display', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet);
  await page.goto(appURL);
  await expect(
    page.getByText('1 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  expect(api.traceRequests).toHaveLength(0);
  await page.route('**/api/trace?**', (route) =>
    route.fulfill({
      json: {
        records: [{ sequence: 42, record: { timestamp_ms: 42, event: packet } }],
        cursor: 42,
        dropped: 0,
      },
    }),
  );
  await page
    .getByRole('searchbox', { name: 'Search USB trace' })
    .fill('cannot possibly match this packet');
  await expect(
    page.getByRole('region', { name: 'USB trace records' }).getByText('Received · endpoint 129'),
  ).toBeVisible();
  await page.getByRole('button', { name: 'Pause display', exact: true }).click();
  await expect(page.getByRole('searchbox', { name: 'Search USB trace' })).toBeDisabled();
  await expect(page.getByRole('spinbutton', { name: 'Endpoint', exact: true })).toBeDisabled();
  await expect(page.getByRole('checkbox', { name: 'Errors only' })).toBeDisabled();
  api.emit(packet);
  await expect.poll(() => api.cursors.at(-1)).toBe(2);
  await page.getByRole('button', { name: 'Resume display', exact: true }).click();
  await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('');
  await expect(
    page.getByText('2 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  expect(api.cursors.filter((cursor) => cursor === 0)).toHaveLength(1);
});

test('search uses retained server history and a 404 is retryable without disconnecting canonical events', async ({
  page,
  api,
  appURL,
}) => {
  api.emit(packet);
  await page.goto(appURL);
  await expect.poll(() => api.cursors.at(-1)).toBe(1);
  api.traceStatus = 404;
  await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('received endpoint 129');
  await expect(page.getByRole('alert').filter({ hasText: 'HTTP 404' })).toBeVisible();
  api.emit(packet);
  await expect.poll(() => api.cursors.at(-1)).toBe(2);
  api.traceStatus = 200;
  await page.getByRole('button', { name: 'Retry search' }).click();
  await expect(
    page.getByText('2 records · newest first · board time', { exact: true }),
  ).toBeVisible();
  await expect(page.getByRole('alert').filter({ hasText: 'HTTP 404' })).toHaveCount(0);
  expect(api.traceRequests.at(-1)?.get('q')).toBe('received endpoint 129');
});
