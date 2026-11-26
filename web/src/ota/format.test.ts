import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';
import { describe, expect, it } from 'vitest';
import { assetImage, crc32 } from '../asset-image.ts';
import { buildActive, createBootstrapFiles, main } from './build.ts';
import {
  allocationRecord,
  buildOta,
  journalSeed,
  otaLayout,
  validateOtaManifest,
  verifyOta,
  verifyOtaFile,
} from './format.ts';

import { bootstrapHex } from './hex.ts';

const { privateKey, publicKey } = generateKeyPairSync('ed25519');
const dashboard = assetImage(gzipSync('<!doctype html><title>OTA fixture</title>'));
function active() {
  const bytes = Buffer.alloc(0x1e0000, 255);
  bytes.writeUInt32LE(0x24080000, 0);
  bytes.writeUInt32LE(0x08020401, 4);
  bytes.writeUInt32LE(0x30020000, 0xe0000);
  bytes.writeUInt32LE(0x08100401, 0xe0004);
  return bytes;
}
function resign(header: Buffer) {
  const digest = createHash('sha256')
    .update('GigaUSB OTA manifest v1\0')
    .update(header.subarray(0, 192))
    .digest();
  sign(null, digest, privateKey).copy(header, 192);
}
function elf(base: number, sp: number) {
  const bytes = Buffer.alloc(0x508);
  Buffer.from([127, 69, 76, 70, 1, 1, 1]).copy(bytes);
  bytes.writeUInt16LE(2, 16);
  bytes.writeUInt16LE(40, 18);
  bytes.writeUInt32LE(1, 20);
  bytes.writeUInt32LE(base + 0x401, 24);
  bytes.writeUInt32LE(52, 28);
  bytes.writeUInt32LE(0x05000400, 36);
  bytes.writeUInt16LE(52, 40);
  bytes.writeUInt16LE(32, 42);
  bytes.writeUInt16LE(1, 44);
  for (const [offset, value] of [
    [52, 1],
    [56, 0x100],
    [60, base],
    [64, base],
    [68, 0x408],
    [72, 0x408],
    [76, 5],
    [80, 4],
  ])
    bytes.writeUInt32LE(value, offset);
  bytes.writeUInt32LE(sp, 0x100);
  bytes.writeUInt32LE(base + 0x401, 0x104);
  return bytes;
}
const m7 = () => elf(0x08020000, 0x24080000);
const m4 = () => elf(0x08100000, 0x30020000);
const boot = () => elf(0x08000000, 0x24080000);

describe('Node OTA artifacts', () => {
  it('maps exact bootstrap bytes to internal flash and external NOR aliases', () => {
    const files = createBootstrapFiles(boot(), m7(), m4(), dashboard, privateKey, 'fixture-a');
    const hex = bootstrapHex(files, dashboard, publicKey);
    const expected = [
      [0x08000000, Buffer.concat([boot().subarray(0x100), Buffer.alloc(0x20000 - 0x408, 255)])],
      [0x08020000, files.get('ota-active.bin')!],
      [0x90200000, files.get('ota-state.bin')!],
      [0x90202000, files.get('ota-allocation.bin')!],
      [0x90220000, files.get('ota-manifest.bin')!],
      [0x90f00000, dashboard],
    ] as const;
    let upper = 0;
    let index = 0;
    let offset = 0;
    for (const line of hex.trim().split('\n')) {
      const bytes = Buffer.from(line.slice(1), 'hex');
      expect(bytes.reduce((sum, byte) => sum + byte, 0) & 255).toBe(0);
      if (bytes[3] === 4) upper = bytes.readUInt16BE(4) * 65536;
      if (bytes[3] !== 0) continue;
      const [base, data] = expected[index];
      expect(upper + bytes.readUInt16BE(1)).toBe(base + offset);
      const chunk = bytes.subarray(4, -1);
      expect(chunk).toEqual(data.subarray(offset, offset + chunk.length));
      offset += chunk.length;
      if (offset === data.length) {
        index++;
        offset = 0;
      }
    }
    expect(index).toBe(expected.length);
    expect(offset).toBe(0);
    expect(() =>
      bootstrapHex(files, dashboard, generateKeyPairSync('ed25519').publicKey),
    ).toThrow();
    files.set('ota-state.bin', Buffer.alloc(8192));
    expect(() => bootstrapHex(files, dashboard, publicKey)).toThrow('exact factory');
  });
  it('signs the exact domain digest and round trips the full image', () => {
    const image = buildOta(active(), dashboard, privateKey, 'fixture-a');
    expect(verifyOta(image, publicKey)).toMatchObject({
      buildId: 'fixture-a',
      total: image.length,
    });
    const header = Buffer.from(image.subarray(0, 256));
    resign(header);
    expect(header.equals(image.subarray(0, 256))).toBe(true);
    expect(header.subarray(168, 192).every((b) => b === 0)).toBe(true);
  });
  it('rejects every mutated header byte, wrong keys, truncation and trailing data', () => {
    const image = buildOta(active(), dashboard, privateKey, 'fixture-a');
    for (let i = 0; i < 256; i++) {
      const header = Buffer.from(image.subarray(0, 256));
      header[i] ^= 1;
      expect(() => validateOtaManifest(header, publicKey), `byte ${i}`).toThrow();
    }
    expect(() => verifyOta(image, generateKeyPairSync('ed25519').publicKey)).toThrow();
    expect(() => verifyOta(image, 'invalid key')).toThrow();
    expect(() =>
      verifyOta(image, generateKeyPairSync('rsa', { modulusLength: 1024 }).publicKey),
    ).toThrow('Ed25519');
    for (const bytes of [
      image.subarray(0, 255),
      image.subarray(0, -1),
      Buffer.concat([image, Buffer.alloc(1)]),
    ])
      expect(() => verifyOta(bytes, publicKey)).toThrow();
    for (const offset of [256, 256 + 0xe0000, image.length - 1]) {
      const bytes = Buffer.from(image);
      bytes[offset] ^= 1;
      expect(() => verifyOta(bytes, publicKey)).toThrow();
    }
  });
  it('checks actual vectors even when payload digests and signature match', () => {
    const image = buildOta(active(), dashboard, privateKey, 'fixture-a');
    image[256] ^= 1;
    createHash('sha256')
      .update(image.subarray(256, 256 + 0x1e0000))
      .digest()
      .copy(image, 40);
    resign(image.subarray(0, 256));
    expect(() => verifyOta(image, publicKey)).toThrow('actual vectors');
  });
  it('rejects dashboard CRC/reserved/padding, malformed IDs and vectors', () => {
    for (const offset of [0, 8, 12, 16, dashboard.length - 1]) {
      const bytes = Buffer.from(dashboard);
      bytes[offset] ^= 1;
      expect(() => buildOta(active(), bytes, privateKey, 'fixture-a')).toThrow();
    }
    for (const id of ['', 'a/b', 'a b', 'a\0b', 'é', 'a'.repeat(49)])
      expect(() => buildOta(active(), dashboard, privateKey, id)).toThrow();
    for (const offset of [0, 4, 0xe0000, 0xe0004]) {
      const bytes = active();
      bytes[offset] ^= 1;
      expect(() => buildOta(bytes, dashboard, privateKey, 'fixture-a')).toThrow();
    }
    expect(() => buildOta(active().subarray(1), dashboard, privateKey, 'fixture-a')).toThrow();
  });
  it('matches Rust factory state and allocation CRC records', () => {
    const seed = journalSeed();
    expect(seed.length).toBe(8192);
    expect(seed.subarray(0, 8).toString()).toBe('GIGAST01');
    expect(seed.readUInt32LE(8)).toBe(1);
    expect(seed.readUInt32LE(12)).toBe(0);
    expect(seed.subarray(16, 80).every((b) => b === 255)).toBe(true);
    expect(seed.subarray(80, 123).every((b) => b === 0)).toBe(true);
    expect(seed.readUInt32LE(123)).toBe(0x5453a227);
    expect(seed[127]).toBe(0);
    expect(seed.subarray(128).every((b) => b === 255)).toBe(true);
    const allocation = allocationRecord();
    expect(allocation.length).toBe(32);
    expect(allocation.subarray(0, 8).toString()).toBe('GIGAAL01');
    expect(allocation.readUInt32LE(8)).toBe(1);
    expect(allocation.subarray(12, 28).every((b) => b === 0)).toBe(true);
    expect(allocation.readUInt32LE(28)).toBe(crc32(allocation.subarray(0, 28)));
  });
  it('fills ELF holes with FF and rejects wrong core, overlap and physical bounds', () => {
    const bytes = buildActive(m7(), m4());
    expect(bytes.length).toBe(0x1e0000);
    expect(bytes.subarray(0x408, 0xe0000).every((b) => b === 255)).toBe(true);
    expect(() => buildActive(boot(), m4())).toThrow();
    const wrong = m7();
    wrong.writeUInt32LE(0x08000000, 64);
    expect(() => buildActive(wrong, m4())).toThrow();
    const entry = m7();
    entry.writeUInt32LE(0x08100001, 24);
    expect(() => buildActive(entry, m4())).toThrow();
    expect(() =>
      createBootstrapFiles(m7(), m7(), m4(), dashboard, privateKey, 'fixture-a'),
    ).toThrow();
  });
  it('emits fixed bootstrap files and exact public metadata', () => {
    const files = createBootstrapFiles(boot(), m7(), m4(), dashboard, privateKey, 'fixture-a');
    expect([...files.keys()].sort()).toEqual([
      'bootloader.elf',
      'm4.elf',
      'm7.elf',
      'ota-active.bin',
      'ota-allocation.bin',
      'ota-layout.json',
      'ota-manifest.bin',
      'ota-state.bin',
    ]);
    expect(JSON.parse(files.get('ota-layout.json')!.toString())).toEqual(
      otaLayout('fixture-a', publicKey),
    );
    expect(otaLayout('fixture-a', publicKey).publicKeyHex).toMatch(/^[a-f0-9]{64}$/);
    expect(
      verifyOta(
        Buffer.concat([files.get('ota-manifest.bin')!, files.get('ota-active.bin')!, dashboard]),
        publicKey,
      ).buildId,
    ).toBe('fixture-a');
  });
  it('has help and rejects invalid CLI calls without board access', () => {
    expect(() => main([])).toThrow('Node 24');
  });
  it('builds both CLI artifacts exclusively and verifies OTA files from disk', () => {
    const directory = mkdtempSync(join(tmpdir(), 'giga-ota-'));
    try {
      const inputs = new Map([
        ['boot.elf', boot()],
        ['m7.elf', m7()],
        ['m4.elf', m4()],
        ['dashboard.bin', dashboard],
        ['test-key.pem', Buffer.from(privateKey.export({ format: 'pem', type: 'pkcs8' }))],
      ]);
      for (const [name, bytes] of inputs)
        writeFileSync(join(directory, name), bytes, { mode: 0o600 });
      const appArgs = ['m7.elf', 'm4.elf', 'dashboard.bin', 'test-key.pem'].map((name) =>
        join(directory, name),
      );
      const output = join(directory, 'image.ota.bin');
      main(['ota', ...appArgs, 'fixture-a', output]);
      expect(verifyOtaFile(output, publicKey).buildId).toBe('fixture-a');
      const original = readFileSync(output);
      expect(() => main(['ota', ...appArgs, 'fixture-a', output])).toThrow();
      expect(readFileSync(output)).toEqual(original);
      const bootstrap = join(directory, 'bootstrap');
      main(['bootstrap', join(directory, 'boot.elf'), ...appArgs, 'fixture-a', bootstrap]);
      expect(readFileSync(join(bootstrap, 'ota-state.bin'))).toEqual(journalSeed());
      expect(() =>
        main(['bootstrap', join(directory, 'boot.elf'), ...appArgs, 'fixture-a', bootstrap]),
      ).toThrow();
      expect(() => verifyOtaFile(join(directory, 'missing'), publicKey)).toThrow();
    } finally {
      rmSync(directory, { recursive: true, force: true });
    }
  });
});
