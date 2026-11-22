import { test, expect } from 'vitest';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import source from './usb.ids?raw';
import { parseUsbIds, usbId } from './usb-ids';

test('complete pinned snapshot has reproducible identity and record counts', () => {
  expect(
    createHash('sha256')
      .update(readFileSync(new URL('./usb.ids', import.meta.url)))
      .digest('hex'),
  ).toBe('f5a48b0cc8dae1607c2f0bae6b8dc13f2ecef69dbeaeaf34b4be8e280d34dba4');
  const vendors = parseUsbIds(source);
  expect(vendors.size).toBe(3427);
  expect([...vendors.values()].reduce((count, vendor) => count + vendor.products.size, 0)).toBe(
    20537,
  );
});

test('exact vendor/product lookups span manufacturers and do not leak products', () => {
  expect(usbId(0x05ac, 0x0201)).toEqual({
    vendor: 'Apple, Inc.',
    product: 'USB Keyboard [Alps or Logitech, M2452]',
  });
  expect(usbId(0x0781, 0x0001)).toEqual({
    vendor: 'SanDisk Corp.',
    product: 'SDDR-05a ImageMate CompactFlash Reader',
  });
  expect(usbId(0x046d, 0xc548).product).toBe('Logi Bolt Receiver');
  expect(usbId(0xffff, 0xc548)).toEqual({ vendor: undefined, product: undefined });
  expect(usbId(0x05ac, 0xc548).product).toBeUndefined();
  expect(usbId(0x05ac, 0xffff).vendor).toBe('Apple, Inc.');
});

test('interfaces/classes are not products; registry text stays literal', () => {
  const vendors = parseUsbIds(
    '1234  <Vendor>\n\t0001  <img src=x>\n\t\t0002  interface\nC 01  Audio\n\t0003  subclass\n',
  );
  expect(vendors.get(0x1234)?.products.get(1)).toBe('<img src=x>');
  expect(vendors.get(0x1234)?.products.size).toBe(1);
});
