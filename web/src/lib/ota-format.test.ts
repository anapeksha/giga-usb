import { describe, expect, it } from 'vitest';
import { parseHeader, parseOtaHeader } from './ota-format.ts';

function header() {
  const bytes = new Uint8Array(256);
  bytes.set(new TextEncoder().encode('GIGAOTA1'));
  const view = new DataView(bytes.buffer);
  for (const [offset, value] of [
    [8, 1],
    [12, 256],
    [16, 1],
    [20, 10],
    [24, 0x08020000],
    [28, 0x1e0000],
    [32, 0x08100000],
    [36, 4096],
    [104, 0x24080000],
    [108, 0x08020401],
    [112, 0x30020000],
    [116, 0x08100401],
  ])
    view.setUint32(offset, value, true);
  bytes.set(new TextEncoder().encode('fixture-a'), 120);
  return bytes;
}
describe('browser OTA structural header', () => {
  it('matches Rust field offsets and supports nonzero byte offsets without crypto', () => {
    const storage = new Uint8Array(300);
    storage.set(header(), 17);
    expect(parseHeader(storage.subarray(17))).toMatchObject({
      buildId: 'fixture-a',
      activeLen: 0x1e0000,
      dashboardLen: 4096,
      total: 256 + 0x1e0000 + 4096,
    });
  });
  it('accepts all 48 build ID bytes but rejects noncanonical padding', () => {
    const bytes = header();
    bytes.fill(65, 120, 168);
    expect(parseOtaHeader(bytes).buildId).toHaveLength(48);
    bytes[121] = 0;
    expect(() => parseOtaHeader(bytes)).toThrow('build ID');
  });
  it('rejects truncated headers and malformed structural fields', () => {
    expect(() => parseOtaHeader(header().subarray(0, 255))).toThrow();
    for (const offset of [0, 8, 12, 16, 20, 24, 28, 32, 36, 104, 112, 120, 168, 191]) {
      const bytes = header();
      bytes[offset] ^= 0x80;
      expect(() => parseOtaHeader(bytes), `offset ${offset}`).toThrow();
    }
  });
  it('does not pretend to verify signatures or payload hashes', () => {
    const bytes = header();
    bytes.fill(255, 40, 104);
    bytes.fill(255, 192);
    expect(parseOtaHeader(bytes).buildId).toBe('fixture-a');
  });
});
