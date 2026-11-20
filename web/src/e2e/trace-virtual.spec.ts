import { test, expect } from './fixtures';

test.beforeEach(async ({ page }) => {
  await page.route('**/favicon.ico', (route) => route.fulfill({ status: 204 }));
  page.on('pageerror', (error) => {
    throw error;
  });
  page.on('console', (message) => {
    if (message.type() === 'error') throw new Error(message.text());
  });
});

test('bounded trace virtualizes, preserves a middle anchor and expanded records', async ({
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
  ).toBeVisible({ timeout: 15000 });
  const viewport = page.getByRole('region', { name: 'USB trace records' });
  const table = viewport.getByRole('table');
  await expect(table).toHaveAttribute('aria-rowcount', '501');
  const rows = viewport.locator('[data-row-key]');
  expect(await rows.count()).toBeLessThan(80);
  expect(await rows.count()).toBeGreaterThan(0);
  await viewport.evaluate((element) => {
    element.scrollTop = element.scrollHeight / 2;
  });
  await expect.poll(() => rows.first().getAttribute('aria-rowindex')).not.toBe('2');
  const anchor = await viewport.evaluate((element) => {
    const top = element.getBoundingClientRect().top;
    const record = [...element.querySelectorAll<HTMLElement>('[data-trace-record]')].find(
      (item) => item.getBoundingClientRect().top >= top + 40,
    );
    if (!record) throw new Error('No visible middle record');
    return {
      key: record.dataset.traceRecord!,
      offset: record.getBoundingClientRect().top - top,
      index: Number(record.closest('tr')!.getAttribute('aria-rowindex')),
    };
  });
  expect(anchor.key).toMatch(/^sequence-\d+$/);
  for (let i = 0; i < 24; i++) emit();
  await expect.poll(() => api.cursors.at(-1), { timeout: 10000 }).toBe(524);
  const anchored = viewport.locator(`[data-trace-record="${anchor.key}"]`);
  await expect(anchored).toBeAttached();
  await expect
    .poll(async () =>
      anchored.evaluate(
        (element) =>
          element.getBoundingClientRect().top -
          element.closest('[role="region"]')!.getBoundingClientRect().top,
      ),
    )
    .toBeCloseTo(anchor.offset, 0);
  expect(await viewport.evaluate((element) => element.scrollTop)).toBeGreaterThan(100);
  await expect(anchored.locator('xpath=ancestor::tr')).toHaveAttribute(
    'aria-rowindex',
    String(anchor.index + 24),
  );
  await anchored.getByRole('button').click();
  await expect(anchored.getByRole('button')).toHaveAttribute('aria-expanded', 'true');
  const position = await viewport.evaluate((element) => element.scrollTop);
  await viewport.evaluate((element) => {
    element.scrollTop = 0;
  });
  await expect(anchored).toHaveCount(0);
  await viewport.evaluate((element, offset) => {
    element.scrollTop = offset;
  }, position);
  await expect(anchored.getByRole('button')).toHaveAttribute('aria-expanded', 'true');
  await page.setViewportSize({ width: 375, height: 800 });
  await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('no such packet');
  await expect(viewport.getByText(/No matching events/)).toBeVisible();
  await page.getByRole('searchbox', { name: 'Search USB trace' }).fill('');
  await expect(table).toHaveAttribute('aria-rowcount', '501');
  expect(await rows.count()).toBeLessThan(80);
  await page.getByRole('button', { name: 'Clear trace', exact: true }).click();
  await expect(viewport.getByText(/No matching events/)).toBeVisible();
});

for (const width of [320, 375, 1280]) {
  test(`trace controls align and stay contained at ${width}px`, async ({ page, api, appURL }) => {
    expect(api.records).toHaveLength(0);
    await page.setViewportSize({ width, height: 900 });
    await page.goto(appURL);
    const endpoint = page.getByRole('spinbutton', { name: 'Endpoint', exact: true });
    const checkAlignment = async () => {
      if (width < 1280) return;
      const checkbox = await page.getByRole('checkbox', { name: 'Errors only' }).boundingBox();
      expect(checkbox).not.toBeNull();
      for (const control of [
        endpoint,
        page.getByRole('combobox', { name: 'Direction' }),
        page.getByRole('combobox', { name: 'Transfer' }),
        page.getByRole('searchbox', { name: 'Search USB trace' }),
      ]) {
        const box = await control.boundingBox();
        expect(box).not.toBeNull();
        expect(
          Math.abs(box!.y + box!.height / 2 - checkbox!.y - checkbox!.height / 2),
        ).toBeLessThanOrEqual(2);
      }
    };
    await checkAlignment();
    await endpoint.fill('256');
    await endpoint.press('Tab');
    await expect(endpoint).toHaveAttribute('aria-invalid', 'true');
    await checkAlignment();
    expect(await page.locator('body').evaluate((body) => body.scrollWidth <= innerWidth)).toBe(
      true,
    );
  });
}
