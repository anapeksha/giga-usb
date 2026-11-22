import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { gunzipSync } from 'node:zlib';

export const FLASH_SIZE = 16 * 1024 * 1024;
export const OFFSET = 15 * 1024 * 1024;
export const CAPACITY = 1024 * 1024;
export const HEADER_SIZE = 32;

export function crc32(bytes: Uint8Array) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return ~crc >>> 0;
}

export function assetImage(gzip: Buffer) {
  if (gzip.length < 18 || gzip.length > CAPACITY - HEADER_SIZE)
    throw new Error('Asset exceeds partition bounds');
  const html = gunzipSync(gzip, { maxOutputLength: 8 * 1024 * 1024 }).toString('utf8');
  if (!/<!doctype html>/i.test(html)) throw new Error('Expected gzip HTML');
  const image = Buffer.alloc(Math.ceil((HEADER_SIZE + gzip.length) / 4096) * 4096, 0xff);
  image.fill(0, 0, HEADER_SIZE);
  image.write('GIGAUI01');
  image.writeUInt32LE(gzip.length, 8);
  image.writeUInt32LE(crc32(gzip), 12);
  gzip.copy(image, HEADER_SIZE);
  return image;
}

/** Offline guard only: never talks to a probe or writes a device. */
export function checkBackup(backup: Buffer, expectedHash: string, image: Buffer, replace: boolean) {
  if (
    backup.length !== FLASH_SIZE ||
    createHash('sha256').update(backup).digest('hex') !== expectedHash
  ) {
    throw new Error('Full 16 MiB backup and independently recorded SHA-256 required');
  }
  if (!image.length || image.length > CAPACITY || image.length % 4096 !== 0) {
    throw new Error('Invalid sector image bounds');
  }
  const previous = backup.subarray(OFFSET, OFFSET + image.length);
  if (previous.every((byte) => byte === 0xff)) return;
  const length = previous.readUInt32LE(8);
  const known =
    previous.subarray(0, 8).toString() === 'GIGAUI01' &&
    length >= 18 &&
    length <= CAPACITY - HEADER_SIZE &&
    previous.subarray(16, HEADER_SIZE).every((byte) => byte === 0) &&
    previous.subarray(HEADER_SIZE + length).every((byte) => byte === 0xff) &&
    crc32(backup.subarray(OFFSET + HEADER_SIZE, OFFSET + HEADER_SIZE + length)) ===
      previous.readUInt32LE(12);
  if (known && replace) {
    assetImage(backup.subarray(OFFSET + HEADER_SIZE, OFFSET + HEADER_SIZE + length));
  } else {
    throw new Error(
      'Refusing occupied range: only explicitly approved valid dashboard replacement is supported',
    );
  }
}

if (import.meta.main) {
  const [command, input, output, backupPath, backupHash, approval] = process.argv.slice(2);
  if (command !== 'build' && command !== 'prepare')
    throw new Error(
      'Use build <gzip> <image>, or prepare <gzip> <image> <full-backup> <sha256> [replace-dashboard]',
    );
  const gzip = readFileSync(input);
  const image = assetImage(gzip);
  if (command === 'prepare')
    checkBackup(readFileSync(backupPath), backupHash, image, approval === 'replace-dashboard');
  writeFileSync(output, image, { flag: 'wx' });
  writeFileSync(
    `${output}.json`,
    JSON.stringify(
      {
        format: 'GIGAUI01',
        offset: OFFSET,
        length: image.length,
        gzipLength: gzip.length,
        crc32: crc32(gzip).toString(16).padStart(8, '0'),
        sha256: createHash('sha256').update(image).digest('hex'),
        backupSha256: command === 'prepare' ? backupHash : null,
        status:
          command === 'prepare'
            ? 'backup-checked; programming requires approved loader'
            : 'build artifact; NOT approved for programming',
      },
      null,
      2,
    ) + '\n',
    { flag: 'wx' },
  );
}
