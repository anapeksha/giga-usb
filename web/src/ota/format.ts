import {
  createHash,
  createPrivateKey,
  createPublicKey,
  sign,
  verify,
  type KeyObject,
} from 'node:crypto';
import { readFileSync, statSync } from 'node:fs';
import { assetImage, crc32 } from '../asset-image.ts';
import {
  OTA_ACTIVE_LENGTH,
  OTA_HEADER_LENGTH,
  OTA_M4_OFFSET,
  parseOtaHeader,
} from '../lib/ota-format.ts';

export { parseOtaHeader } from '../lib/ota-format.ts';
export type OtaKey = Buffer | string | KeyObject;
const DOMAIN = Buffer.from('GigaUSB OTA manifest v1\0');
const hash = (bytes: Uint8Array) => createHash('sha256').update(bytes).digest();
function need(ok: unknown, message: string): asserts ok {
  if (!ok) throw new Error(message);
}
function publicKey(key: OtaKey) {
  const result =
    typeof key === 'object' && 'type' in key && key.type === 'public' ? key : createPublicKey(key);
  need(result.asymmetricKeyType === 'ed25519', 'Expected Ed25519 key');
  return result;
}
function signingDigest(header: Buffer) {
  return hash(Buffer.concat([DOMAIN, header.subarray(0, 192)]));
}
export function validateOtaManifest(header: Buffer, key: OtaKey) {
  need(header.length === OTA_HEADER_LENGTH, 'OTA manifest must be 256 bytes');
  const parsed = parseOtaHeader(header);
  need(
    verify(null, signingDigest(header), publicKey(key), header.subarray(192)),
    'Invalid OTA signature',
  );
  return parsed;
}
function validateDashboard(dashboard: Buffer) {
  need(
    dashboard.length >= 4096 && dashboard.length <= 0x100000 && dashboard.length % 4096 === 0,
    'Dashboard bounds',
  );
  need(dashboard.subarray(0, 8).equals(Buffer.from('GIGAUI01')), 'Dashboard magic');
  const length = dashboard.readUInt32LE(8);
  need(length >= 18 && length <= dashboard.length - 32, 'Dashboard payload bounds');
  need(
    assetImage(dashboard.subarray(32, 32 + length)).equals(dashboard),
    'Dashboard CRC, reserved bytes or padding',
  );
}
export function verifyOta(image: Buffer, key: OtaKey) {
  need(image.length >= OTA_HEADER_LENGTH, 'Truncated OTA image');
  const header = image.subarray(0, OTA_HEADER_LENGTH);
  const parsed = validateOtaManifest(header, key);
  need(image.length === parsed.total, 'OTA file length or trailing bytes');
  const active = image.subarray(OTA_HEADER_LENGTH, OTA_HEADER_LENGTH + OTA_ACTIVE_LENGTH);
  const dashboard = image.subarray(OTA_HEADER_LENGTH + OTA_ACTIVE_LENGTH);
  need(
    hash(active).equals(parsed.activeHash) && hash(dashboard).equals(parsed.dashboardHash),
    'OTA payload digest',
  );
  need(
    active.subarray(0, 8).equals(header.subarray(104, 112)) &&
      active.subarray(OTA_M4_OFFSET, OTA_M4_OFFSET + 8).equals(header.subarray(112, 120)),
    'OTA actual vectors',
  );
  validateDashboard(dashboard);
  return parsed;
}
export function verifyOtaFile(file: string, key: OtaKey) {
  const stat = statSync(file);
  need(
    stat.isFile() &&
      stat.size >= OTA_HEADER_LENGTH &&
      stat.size <= OTA_HEADER_LENGTH + OTA_ACTIVE_LENGTH + 0x100000,
    'OTA file bounds',
  );
  return verifyOta(readFileSync(file), key);
}

/** The caller must supply the build ID used to compile M7; binary signing cannot prove that provenance. */
export function buildOta(active: Buffer, dashboard: Buffer, privatePem: OtaKey, buildId: string) {
  need(active.length === OTA_ACTIVE_LENGTH, 'ACTIVE must be exactly 0x1e0000 bytes');
  need(/^[A-Za-z0-9._-]{1,48}$/.test(buildId), 'Invalid OTA build ID');
  validateDashboard(dashboard);
  const key =
    typeof privatePem === 'object' && 'type' in privatePem
      ? privatePem
      : createPrivateKey(privatePem);
  need(
    key.type === 'private' && key.asymmetricKeyType === 'ed25519',
    'Expected Ed25519 private key',
  );
  const header = Buffer.alloc(OTA_HEADER_LENGTH);
  header.write('GIGAOTA1');
  for (const [offset, value] of [
    [8, 1],
    [12, 256],
    [16, 1],
    [20, 10],
    [24, 0x08020000],
    [28, OTA_ACTIVE_LENGTH],
    [32, 0x08100000],
    [36, dashboard.length],
  ])
    header.writeUInt32LE(value, offset);
  hash(active).copy(header, 40);
  hash(dashboard).copy(header, 72);
  active.copy(header, 104, 0, 8);
  active.copy(header, 112, OTA_M4_OFFSET, OTA_M4_OFFSET + 8);
  header.write(buildId, 120, 'ascii');
  parseOtaHeader(header);
  sign(null, signingDigest(header), key).copy(header, 192);
  const image = Buffer.concat([header, active, dashboard]);
  verifyOta(image, createPublicKey(key));
  return image;
}
export function journalSeed(): Buffer {
  const bytes = Buffer.alloc(8192, 0xff);
  bytes.fill(0, 0, 128);
  bytes.write('GIGAST01');
  bytes.writeUInt32LE(1, 8);
  bytes.fill(0xff, 16, 80);
  bytes.writeUInt32LE(crc32(bytes.subarray(0, 123)), 123);
  return bytes;
}
export function allocationRecord(): Buffer {
  const bytes = Buffer.alloc(32);
  bytes.write('GIGAAL01');
  bytes.writeUInt32LE(1, 8);
  bytes.writeUInt32LE(crc32(bytes.subarray(0, 28)), 28);
  return bytes;
}
export function otaLayout(buildId: string, key: OtaKey) {
  need(/^[A-Za-z0-9._-]{1,48}$/.test(buildId), 'Invalid OTA build ID');
  const der = publicKey(key).export({ format: 'der', type: 'spki' });
  return {
    format: 'GIGAUSB-OTA-BOOTSTRAP-1',
    layout: 1,
    protocol: 10,
    buildId,
    publicKeyHex: der.subarray(-32).toString('hex'),
  };
}
