import { expect, it } from 'vitest';
import { encodeHex, HEX_WARNING } from './hex.ts';

export function decodeHex(text: string) {
  let upper = 0;
  const regions: { address: number; data: Buffer }[] = [];
  const lines = text.trim().split('\n');
  expect(lines.pop()).toBe(':00000001FF');
  for (const line of lines) {
    expect(line).toMatch(/^:[0-9A-F]+$/);
    const bytes = Buffer.from(line.slice(1), 'hex');
    expect(bytes.length).toBe(bytes[0] + 5);
    expect(bytes.reduce((sum, byte) => sum + byte, 0) & 255).toBe(0);
    const address = bytes.readUInt16BE(1);
    if (bytes[3] === 4) {
      expect(address).toBe(0);
      expect(bytes[0]).toBe(2);
      upper = bytes.readUInt16BE(4) * 65536;
    } else {
      expect(bytes[3]).toBe(0);
      expect(bytes[0]).toBeGreaterThan(0);
      expect(bytes[0]).toBeLessThanOrEqual(16);
      expect(address + bytes[0]).toBeLessThanOrEqual(65536);
      regions.push({ address: upper + address, data: bytes.subarray(4, -1) });
    }
  }
  return regions;
}

it('encodes known checksums, sorts sparse regions and splits 64-KiB boundaries', () => {
  expect(encodeHex([{ address: 0, data: Buffer.from([1, 2, 3]) }])).toBe(
    ':020000040000FA\n:03000000010203F7\n:00000001FF\n',
  );
  const regions = decodeHex(
    encodeHex([
      { address: 0xffffffff, data: Buffer.from([0xaa]) },
      { address: 0x9000fff9, data: Buffer.alloc(40, 0x55) },
    ]),
  );
  expect(regions.map((region) => region.address)).toEqual([
    0x9000fff9, 0x90010000, 0x90010010, 0x90010020, 0xffffffff,
  ]);
  expect(Buffer.concat(regions.slice(0, -1).map((region) => region.data))).toEqual(
    Buffer.alloc(40, 0x55),
  );
});

it('rejects overlaps, empty regions and invalid or overflowing addresses', () => {
  for (const address of [-1, 0.5, NaN, Infinity, 0x100000000])
    expect(() => encodeHex([{ address, data: Buffer.from([1]) }])).toThrow();
  expect(() => encodeHex([{ address: 0xffffffff, data: Buffer.alloc(2) }])).toThrow();
  expect(() => encodeHex([{ address: 0, data: Buffer.alloc(0) }])).toThrow();
  expect(() =>
    encodeHex([
      { address: 1, data: Buffer.alloc(2) },
      { address: 2, data: Buffer.alloc(1) },
    ]),
  ).toThrow('Overlapping');
  expect(encodeHex([])).toBe(':00000001FF\n');
  expect(HEX_WARNING).toContain('separately validated');
});
