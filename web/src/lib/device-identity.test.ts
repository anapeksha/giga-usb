import { test } from 'vitest';
import assert from 'node:assert/strict';
import { deviceIdentity } from './device-identity';
import { createState, type InterfaceDescriptor } from './protocol';

function state(vendor_id = 0xffff, product_id = 0xffff, deviceClass = 0) {
  return {
    ...createState(),
    device: {
      vendor_id,
      product_id,
      class: deviceClass,
      subclass: 0,
      protocol: 0,
      usb_version: 0x200,
      device_version: 0x100,
      max_packet_size: 64,
      configurations: 1,
    },
  };
}
const face = (classCode: number, protocol = 0, subclass = 0): InterfaceDescriptor => ({
  number: 0,
  alternate: 0,
  class: classCode,
  subclass,
  protocol,
  endpoints: 1,
});

test('model guesses require an exact vendor-scoped product match', () => {
  assert.equal(
    deviceIdentity(state(0x046d, 0xc548))?.label,
    'Likely Logitech, Inc. Logi Bolt Receiver',
  );
  assert.equal(deviceIdentity(state(0xffff, 0xc548))?.label, 'Likely USB device');
  const unknownProduct = deviceIdentity(state(0x046d, 0x1234));
  assert.equal(unknownProduct?.label, 'Likely Logitech, Inc. USB device');
  assert.match(unknownProduct!.provenance, /model unknown/);
});

test('unknown IDs use device or interface classes without claiming a receiver', () => {
  assert.equal(deviceIdentity(state(0xffff, 2, 8))?.label, 'Likely USB storage');
  const current = state();
  current.interfaces = [face(3, 1, 1), face(3, 2, 1), face(3, 2, 1), face(8)];
  assert.equal(deviceIdentity(current)?.label, 'Likely HID keyboard / HID mouse / USB storage');
  current.interfaces = [face(3, 1, 0)];
  assert.equal(deviceIdentity(current)?.label, 'Likely HID input device');
  current.interfaces = [face(255)];
  assert.equal(deviceIdentity(current)?.label, 'Likely USB device');
});

test('storage inquiry text is bounded, trimmed and explicitly reported', () => {
  const current = state();
  current.storage = {
    vendor: ' VENDOR ',
    product: ' PRODUCT ',
    blocks: 1,
    block_size: 512,
    fat32: true,
  };
  assert.equal(deviceIdentity(current)?.label, 'Reported: VENDOR PRODUCT');
  current.storage.vendor = 'v'.repeat(1000);
  current.storage.product = 'p'.repeat(1000);
  assert.equal(deviceIdentity(current)?.label, `Reported: ${'v'.repeat(8)} ${'p'.repeat(16)}`);
  current.storage.vendor = '\0\n';
  current.storage.product = '   ';
  assert.equal(deviceIdentity(current)?.label, 'Likely USB device');
  current.storage.product = '<img src=x>';
  assert.equal(deviceIdentity(current)?.label, 'Reported: <img src=x>');
});

test('identity is derived, retained with partial topology and absent after session clear', () => {
  const current = state(0x046d, 0xc548);
  current.attached = false;
  current.errors = 1;
  assert.match(deviceIdentity(current)!.label, /Logi Bolt/);
  assert.equal(deviceIdentity(createState()), null);
  assert.equal(deviceIdentity(state())?.label, 'Likely USB device');
});
