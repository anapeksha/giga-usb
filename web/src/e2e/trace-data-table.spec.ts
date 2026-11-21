import AxeBuilder from '@axe-core/playwright';
import type { Locator } from '@playwright/test';
import { test, expect } from './fixtures';

const optional = ['Time', 'Endpoint', 'Transfer'];
const headers = ['Time (ms)', 'Endpoint', 'Transfer', 'Details'];

async function expectColumns(table: Locator, names: string[]) {
  await expect(table).toHaveAttribute('aria-colcount', String(names.length));
  await expect(table.getByRole('columnheader')).toHaveText(names);
  for (const header of await table.getByRole('columnheader').all()) {
    await expect(header).toHaveAttribute('scope', 'col');
    await expect(header.getByRole('button')).toHaveCount(0);
  }
  await expect(table.locator('col')).toHaveCount(names.length);
  for (const spacer of await table.locator('tbody tr[aria-hidden="true"] td').all()) {
    await expect(spacer).toHaveAttribute('colspan', String(names.length));
  }
  for (const row of await table.locator('[data-row-key]').all()) {
    await expect(row.getByRole('cell')).toHaveCount(names.length);
  }
}

test.beforeEach(async ({ page }) => {
  await page.route('**/favicon.ico', (route) => route.fulfill({ status: 204 }));
  page.on('pageerror', (error) => {
    throw error;
  });
});

test('optional columns hide and restore without changing the ordinary USB interfaces table', async ({
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
  api.emit({
    type: 'host',
    event: { transfer: { endpoint: 129, direction: 'in', bytes: 4, pid: 2, data: [1, 2, 3, 4] } },
  });
  const response = await page.goto(appURL);
  expect(response?.headers()['content-encoding']).toBe('gzip');
  await page.getByText('Device details', { exact: true }).click();
  const topology = page.getByRole('table', { name: 'USB interfaces', exact: true });
  const endpoints = page.getByRole('table', {
    name: 'Interface 0, alternate 0: endpoints',
    exact: true,
  });
  await expect(endpoints.getByRole('cell', { name: '0x81 IN', exact: true })).toBeVisible();
  const endpointText = await endpoints.innerText();
  const topologyText = await topology.innerText();
  const topologyHeaders = await topology.getByRole('columnheader').allTextContents();
  const table = page.getByRole('table', { name: 'USB transfers and diagnostics' });
  await expect(table.locator('[data-row-key]')).toHaveCount(1);
  await expectColumns(table, headers);
  await expect(page.getByRole('button', { name: 'Columns', exact: true })).toHaveCount(1);
  await expect(topology.getByRole('button', { name: 'Columns' })).toHaveCount(0);
  await page.getByRole('button', { name: 'Columns', exact: true }).click();
  await expect(page.getByRole('menuitemcheckbox')).toHaveText(optional);
  await expect(page.getByRole('menuitemcheckbox', { name: 'Details' })).toHaveCount(0);
  for (let i = 0; i < optional.length; i++) {
    const item = page.getByRole('menuitemcheckbox', { name: optional[i], exact: true });
    await expect(item).toHaveAttribute('aria-checked', 'true');
    await item.click();
    await expect(item).toHaveAttribute('aria-checked', 'false');
    await page.keyboard.press('Escape');
    await expectColumns(
      table,
      headers.filter((_, index) => index > i),
    );
    await page.getByRole('button', { name: 'Columns', exact: true }).click();
  }
  for (let i = optional.length - 1; i >= 0; i--) {
    const item = page.getByRole('menuitemcheckbox', { name: optional[i], exact: true });
    await item.click();
    await expect(item).toHaveAttribute('aria-checked', 'true');
    await page.keyboard.press('Escape');
    await expectColumns(table, headers.slice(i));
    await page.getByRole('button', { name: 'Columns', exact: true }).click();
  }
  await page.keyboard.press('Escape');
  await expect(topology).toHaveText(topologyText, { useInnerText: true });
  await expect(topology.getByRole('columnheader')).toHaveText(topologyHeaders);
  await expect(topology).not.toHaveAttribute('aria-colcount');
  await expect(endpoints).toHaveText(endpointText, { useInnerText: true });
});

for (const width of [320, 375, 1280]) {
  test(`column menu is keyboard accessible and trace scroll stays internal at ${width}px`, async ({
    page,
    api,
    appURL,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    api.emit({
      type: 'host',
      event: { transfer: { endpoint: 129, direction: 'in', bytes: 4, pid: 2, data: [1, 2, 3, 4] } },
    });
    await page.goto(appURL);
    const viewport = page.getByRole('region', { name: 'USB trace records' });
    const table = viewport.getByRole('table');
    await expect(table.locator('[data-row-key]')).toHaveCount(1);
    const trigger = page.getByRole('button', { name: 'Columns', exact: true });
    await trigger.focus();
    await page.keyboard.press('Enter');
    const time = page.getByRole('menuitemcheckbox', { name: 'Time', exact: true });
    await expect(time).toBeFocused();
    await page.keyboard.press('Space');
    await expect(time).toHaveAttribute('aria-checked', 'false');
    await page.keyboard.press('ArrowDown');
    await expect(
      page.getByRole('menuitemcheckbox', { name: 'Endpoint', exact: true }),
    ).toBeFocused();
    expect.soft((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    await page.keyboard.press('Escape');
    await expect(page.getByRole('menu')).toHaveCount(0);
    await expect(trigger).toBeFocused();
    await expectColumns(table, headers.slice(1));
    expect.soft((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
    expect(await page.locator('body').evaluate((body) => body.scrollWidth <= innerWidth)).toBe(
      true,
    );
    const dimensions = await viewport.evaluate((element) => ({
      client: element.clientWidth,
      scroll: element.scrollWidth,
      overflow: getComputedStyle(element).overflowX,
    }));
    expect(['auto', 'scroll']).toContain(dimensions.overflow);
    if (width < 640) {
      expect(dimensions.scroll).toBeGreaterThan(dimensions.client);
      await viewport.evaluate((element) => {
        element.scrollLeft = element.scrollWidth;
      });
      expect(await viewport.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0);
    }
    await expect(table.getByRole('columnheader', { name: 'Details', exact: true })).toBeVisible();
  });
}

test('500-record history keeps its anchor with hidden columns and expansion through toggles, resize and recycling', async ({
  page,
  api,
  appURL,
}) => {
  test.setTimeout(45000);
  const emit = () =>
    api.emit({
      type: 'host',
      event: { transfer: { endpoint: 129, direction: 'in', bytes: 4, pid: 2, data: [1, 2, 3, 4] } },
    });
  for (let i = 0; i < 500; i++) emit();
  await page.goto(appURL);
  await expect(
    page.getByText('500 records · newest first · board time', { exact: true }),
  ).toBeVisible({
    timeout: 15000,
  });
  const viewport = page.getByRole('region', { name: 'USB trace records' });
  const table = viewport.getByRole('table');
  const trigger = page.getByRole('button', { name: 'Columns', exact: true });
  await trigger.click();
  for (const name of optional)
    await page.getByRole('menuitemcheckbox', { name, exact: true }).click();
  await page.keyboard.press('Escape');
  await expectColumns(table, ['Details']);
  await expect(table).toHaveAttribute('aria-rowcount', '501');
  expect(await table.locator('[data-row-key]').count()).toBeLessThan(80);
  await viewport.evaluate((element) => {
    element.scrollTop = element.scrollHeight / 2;
  });
  await expect
    .poll(() => table.locator('[data-row-key]').first().getAttribute('aria-rowindex'))
    .not.toBe('2');
  const anchor = await viewport.evaluate((element) => {
    const top = element.getBoundingClientRect().top;
    const record = [...element.querySelectorAll<HTMLElement>('[data-trace-record]')].find(
      (item) => item.getBoundingClientRect().top >= top + 40,
    );
    if (!record) throw new Error('Missing visible middle record');
    return {
      key: record.dataset.traceRecord!,
      offset: record.getBoundingClientRect().top - top,
      index: Number(record.closest('tr')!.getAttribute('aria-rowindex')),
    };
  });
  for (let i = 0; i < 24; i++) emit();
  await expect.poll(() => api.cursors.at(-1), { timeout: 10000 }).toBe(524);
  const record = viewport.locator(`[data-trace-record="${anchor.key}"]`);
  await expect(record).toBeAttached();
  await expect
    .poll(() =>
      record.evaluate(
        (element) =>
          element.getBoundingClientRect().top -
          element.closest('[role="region"]')!.getBoundingClientRect().top,
      ),
    )
    .toBeCloseTo(anchor.offset, 0);
  await expect(record.locator('xpath=ancestor::tr')).toHaveAttribute(
    'aria-rowindex',
    String(anchor.index + 24),
  );
  await expect(table).toHaveAttribute('aria-rowcount', '501');
  await record.getByRole('button').click();
  await expect(record.getByRole('button')).toHaveAttribute('aria-expanded', 'true');
  await trigger.click();
  await page.getByRole('menuitemcheckbox', { name: 'Time', exact: true }).click();
  await page.keyboard.press('Escape');
  await expectColumns(table, ['Time (ms)', 'Details']);
  await expect(record.getByRole('button')).toHaveAttribute('aria-expanded', 'true');
  await expect(record.getByText('RAW BYTES (4)', { exact: false })).toBeVisible();
  for (const width of [320, 375, 1280]) {
    await page.setViewportSize({ width, height: 900 });
    await expect(record.getByRole('button')).toHaveAttribute('aria-expanded', 'true');
  }
  const position = await viewport.evaluate((element) => element.scrollTop);
  await viewport.evaluate((element) => {
    element.scrollTop = 0;
  });
  await expect(record).toHaveCount(0);
  await viewport.evaluate((element, offset) => {
    element.scrollTop = offset;
  }, position);
  await expect(record.getByRole('button')).toHaveAttribute('aria-expanded', 'true');
  await expect(record.getByText('RAW BYTES (4)', { exact: false })).toBeVisible();
  expect(await table.locator('[data-row-key]').count()).toBeLessThan(80);
});
