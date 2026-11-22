import { test, expect } from './fixtures';

const descriptor = {
  usb_version: 512,
  vendor_id: 0x046d,
  product_id: 0xc548,
  device_version: 256,
  class: 0,
  subclass: 0,
  protocol: 0,
  max_packet_size: 64,
  configurations: 1,
};

test.beforeEach(async ({ page }) => {
  await page.route('**/favicon.ico', (route) => route.fulfill({ status: 204 }));
  page.on('pageerror', (error) => {
    throw error;
  });
  page.on('console', (message) => {
    if (message.type() === 'error') throw new Error(message.text());
  });
});

for (const width of [320, 768, 1280]) {
  test(`device details are bounded, scrollable and keyboard accessible at ${width}px`, async ({
    page,
    api,
    appURL,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    api.emit({ type: 'device', descriptor });
    for (let number = 0; number < 24; number++) {
      api.emit({
        type: 'interface',
        descriptor: { number, alternate: 0, class: 3, subclass: 1, protocol: 1, endpoints: 1 },
      });
      api.emit({
        type: 'endpoint',
        descriptor: {
          interface: number,
          alternate: 0,
          address: 129,
          attributes: 3,
          max_packet_size: 64,
          interval: 10,
        },
      });
    }
    await page.goto(appURL);
    const trigger = page.getByRole('button', { name: 'Device details', exact: true });
    await trigger.focus();
    await trigger.press('Enter');
    const region = page.getByRole('region', { name: 'USB device details', exact: true });
    await expect(
      region.getByRole('heading', { name: 'Interface 23 · alternate 0', exact: true }),
    ).toBeAttached({ timeout: 10000 });
    await trigger.hover();
    expect(await trigger.evaluate((element) => getComputedStyle(element).textDecorationLine)).toBe(
      'none',
    );
    await trigger.focus();
    await trigger.press('Tab');
    await expect(region).toBeFocused();
    const bounds = await region.boundingBox();
    expect(bounds).not.toBeNull();
    expect(bounds!.height).toBeLessThanOrEqual(width < 640 ? 400 : width < 1024 ? 460 : 520);
    expect(bounds!.x).toBeGreaterThanOrEqual(0);
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width);
    expect(await region.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(
      true,
    );
    await region.press('ArrowDown');
    await expect.poll(() => region.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
    expect(await page.locator('body').evaluate((body) => body.scrollWidth <= innerWidth)).toBe(
      true,
    );
  });
}

test('identity is an offline clue, resets with devices, and renders bounded storage text literally', async ({
  page,
  api,
  appURL,
}) => {
  api.emit({ type: 'device', descriptor });
  await page.goto(appURL);
  await page.getByRole('button', { name: 'Device details', exact: true }).click();
  const region = page.getByRole('region', { name: 'USB device details', exact: true });
  await expect(
    region.getByText('Likely Logitech, Inc. Logi Bolt Receiver', { exact: true }),
  ).toBeVisible();
  await expect(region.getByText(/Offline USB ID match/)).toBeVisible();
  api.emit({ type: 'host', event: { lifecycle: { state: 'enumerating', session_id: 2 } } });
  await expect(
    region.getByText('Likely Logitech, Inc. Logi Bolt Receiver', { exact: true }),
  ).toHaveCount(0);
  api.emit({
    type: 'device',
    descriptor: { ...descriptor, vendor_id: 0xffff, product_id: 0xabcd, class: 8 },
  });
  await expect(region.getByText('Likely USB storage', { exact: true })).toBeVisible();
  await expect(
    region.getByText('Descriptor classes only; make and model unknown', { exact: true }),
  ).toBeVisible();
  api.emit({
    type: 'host',
    event: {
      storage: {
        vendor: '<b>V</b>',
        product: '<img src=x>TAIL!',
        blocks: 100,
        block_size: 512,
        fat32: true,
      },
    },
  });
  await expect(
    region.getByText('Reported: <b>V</b> <img src=x>TAIL!', { exact: true }),
  ).toBeVisible();
  await expect(region.locator('img, b, script')).toHaveCount(0);
  api.emit({
    type: 'host',
    event: {
      storage: {
        vendor: 'V'.repeat(8),
        product: 'P'.repeat(16),
        blocks: 100,
        block_size: 512,
        fat32: true,
      },
    },
  });
  await expect(
    region.getByText(`Reported: ${'V'.repeat(8)} ${'P'.repeat(16)}`, { exact: true }),
  ).toBeVisible();
  await expect(region.getByText('Likely USB storage', { exact: true })).toHaveCount(0);
});

test('complete registry identifies unrelated manufacturers without Internet access', async ({
  page,
  api,
  appURL,
}) => {
  const origins = new Set<string>();
  page.on('request', (request) => origins.add(new URL(request.url()).origin));
  api.emit({
    type: 'device',
    descriptor: { ...descriptor, vendor_id: 0x05ac, product_id: 0x0201 },
  });
  await page.goto(appURL);
  await page.getByRole('button', { name: 'Device details', exact: true }).click();
  const region = page.getByRole('region', { name: 'USB device details', exact: true });
  await expect(
    region.getByText('Likely Apple, Inc. USB Keyboard [Alps or Logitech, M2452]', { exact: true }),
  ).toBeVisible();
  api.emit({
    type: 'device',
    descriptor: { ...descriptor, vendor_id: 0x0781, product_id: 0x0001 },
  });
  await expect(
    region.getByText('Likely SanDisk Corp. SDDR-05a ImageMate CompactFlash Reader', {
      exact: true,
    }),
  ).toBeVisible();
  api.emit({
    type: 'device',
    descriptor: { ...descriptor, vendor_id: 0x05ac, product_id: 0xc548 },
  });
  await expect(region.getByText('Likely Apple, Inc. USB device', { exact: true })).toBeVisible();
  await expect(page.getByText('USB ID database notices', { exact: true })).toBeVisible();
  expect([...origins]).toEqual([new URL(appURL).origin]);
});
