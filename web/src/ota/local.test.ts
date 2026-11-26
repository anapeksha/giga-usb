import { generateKeyPairSync, verify } from 'node:crypto';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { main, parseOptions, signDistribution, USAGE } from './local.ts';

describe('local TEST-ONLY artifact builder', () => {
  it('requires an absolute new target destination and rejects unknown flags', () => {
    const output = resolve('../target/local-ota-test');
    expect(parseOptions(['--output', output])).toEqual({ output, defmt: false });
    expect(parseOptions(['--defmt', '--output', output])).toEqual({ output, defmt: true });
    for (const args of [
      [],
      ['--output', 'target/test'],
      ['--output', '/tmp/test'],
      ['--output', output, '--install'],
      ['--defmt', '--defmt', '--output', output],
    ])
      expect(() => parseOptions(args)).toThrow();
  });

  it('help does not start builds or create output and warns about external provisioning', async () => {
    await expect(main(['--help'])).resolves.toBeUndefined();
    expect(USAGE).toContain('0x90000000');
    expect(USAGE).toContain('separately validated');
    expect(USAGE).not.toContain('--install');
  });

  it('signs the exact MCUboot header/payload digest with the ephemeral key', () => {
    const { privateKey, publicKey } = generateKeyPairSync('ed25519');
    const payload = Buffer.from('test-only archive');
    const image = signDistribution(payload, privateKey, publicKey);
    expect(image.readUInt32LE(0)).toBe(0x96f3b83d);
    expect(image.readUInt16LE(8)).toBe(512);
    expect(image.readUInt32LE(12)).toBe(payload.length);
    expect(image.readUInt32LE(16)).toBe(0x10);
    expect(image.subarray(32, 512).every((byte) => byte === 255)).toBe(true);
    expect(image.subarray(512, 512 + payload.length)).toEqual(payload);
    const offset = 512 + payload.length;
    expect(image.readUInt16LE(offset)).toBe(0x6907);
    expect(image.readUInt16LE(offset + 2)).toBe(144);
    const digest = image.subarray(offset + 44, offset + 76);
    expect(verify(null, digest, publicKey, image.subarray(offset + 80))).toBe(true);
    const other = generateKeyPairSync('ed25519').publicKey;
    expect(verify(null, digest, other, image.subarray(offset + 80))).toBe(false);
  });
});
