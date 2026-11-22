import { test, expect } from 'vitest';
import { gzipSync } from 'node:zlib';
import { createHash } from 'node:crypto';
import {
  assetImage,
  checkBackup,
  crc32,
  FLASH_SIZE,
  HEADER_SIZE,
  OFFSET,
  CAPACITY,
} from './asset-image';

const gzip = gzipSync('<!doctype html><html>offline dashboard</html>');
const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');

test('container is bounded, sector aligned and matches firmware contract', () => {
  const image = assetImage(gzip);
  expect(image.length % 4096).toBe(0);
  expect(image.subarray(0, 8).toString()).toBe('GIGAUI01');
  expect(image.readUInt32LE(8)).toBe(gzip.length);
  expect(image.readUInt32LE(12)).toBe(crc32(gzip));
  expect(crc32(Buffer.from('123456789'))).toBe(0xcbf43926);
  expect(image.subarray(16, HEADER_SIZE).every((byte) => byte === 0)).toBe(true);
  expect(image.subarray(HEADER_SIZE, HEADER_SIZE + gzip.length)).toEqual(gzip);
  expect(() => assetImage(Buffer.alloc(CAPACITY))).toThrow();
  expect(() => assetImage(Buffer.alloc(20))).toThrow();
});

test('preparation requires full hashed backup, protects occupied ranges and explicit replacement', () => {
  const backup = Buffer.alloc(FLASH_SIZE, 255);
  backup[0] = 42;
  const image = assetImage(gzip);
  expect(() => checkBackup(backup, hash(backup), image, false)).not.toThrow();
  expect(() => checkBackup(backup.subarray(1), hash(backup), image, false)).toThrow();
  expect(() => checkBackup(backup, 'wrong', image, false)).toThrow();
  backup[OFFSET] = 0;
  expect(() => checkBackup(backup, hash(backup), image, true)).toThrow();
  image.copy(backup, OFFSET);
  expect(() => checkBackup(backup, hash(backup), image, false)).toThrow();
  expect(() => checkBackup(backup, hash(backup), image, true)).not.toThrow();
  backup[OFFSET + image.length - 1] = 0;
  expect(() => checkBackup(backup, hash(backup), image, true)).toThrow();
  backup[OFFSET + image.length - 1] = 255;
  backup[OFFSET + 16] = 1;
  expect(() => checkBackup(backup, hash(backup), image, true)).toThrow();
  backup[OFFSET + 16] = 0;
  backup[OFFSET + HEADER_SIZE] ^= 1;
  expect(() => checkBackup(backup, hash(backup), image, true)).toThrow();
  expect(backup[0]).toBe(42);
});
