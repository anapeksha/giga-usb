import { createHash, createPublicKey, verify } from 'node:crypto';
import { gunzipSync } from 'node:zlib';

const LIMIT = 64 * 1024 * 1024;
const REQUIRED = [
  'm4.elf',
  'm7.elf',
  'index.html.gz',
  'dashboard.qspi.bin',
  'dashboard.qspi.bin.json',
  'README.md',
  'AGENTS.md',
  'LICENSE',
  'USB-IDS-LICENSE.txt',
  'CHANGELOG.md',
  'BUILD.txt',
  'SHA256SUMS',
];
const OPTIONAL = [
  'qspi-loader.elf',
  'qspi-loader.json',
  'installer-runtime.mjs',
  'installer-manifest.json',
];
const hash = (data: Buffer) => createHash('sha256').update(data).digest();
function need(ok: unknown, message: string): asserts ok {
  if (!ok) throw new Error(message);
}
function zero(data: Buffer) {
  return data.every((b) => b === 0);
}
function text(data: Buffer) {
  return new TextDecoder('utf-8', { fatal: true }).decode(data);
}

function authenticatedPayload(image: Buffer, pem: Buffer | string) {
  need(image.length >= 516 && image.length <= LIMIT, 'Signed image bounds');
  need(
    image.readUInt32LE(0) === 0x96f3b83d &&
      image.readUInt32LE(4) === 0 &&
      image.readUInt16LE(8) === 512 &&
      image.readUInt16LE(10) === 0 &&
      image.readUInt32LE(16) === 0x10 &&
      zero(image.subarray(20, 32)) &&
      image.subarray(32, 512).every((b) => b === 255),
    'Unsupported MCUboot header',
  );
  const end = 512 + image.readUInt32LE(12);
  need(end > 512 && end + 4 <= image.length, 'Payload bounds');
  need(
    image.readUInt16LE(end) === 0x6907 && end + image.readUInt16LE(end + 2) === image.length,
    'TLV bounds or trailing data',
  );
  const tlvs = new Map<number, Buffer>();
  for (let offset = end + 4; offset < image.length;) {
    need(offset + 4 <= image.length, 'Truncated TLV');
    const kind = image[offset];
    const length = image.readUInt16LE(offset + 2);
    need(
      image[offset + 1] === 0 && [1, 0x10, 0x24].includes(kind) && !tlvs.has(kind),
      'Unsupported or duplicate TLV',
    );
    offset += 4;
    need(offset + length <= image.length, 'Truncated TLV value');
    tlvs.set(kind, image.subarray(offset, offset + length));
    offset += length;
  }
  const digest = hash(image.subarray(0, end));
  const key = createPublicKey(pem);
  need(key.asymmetricKeyType === 'ed25519', 'Expected Ed25519 public key');
  const keyHash = hash(key.export({ format: 'der', type: 'spki' }));
  need(
    tlvs.size === 3 &&
      tlvs.get(0x10)?.equals(digest) &&
      tlvs.get(1)?.equals(keyHash) &&
      tlvs.get(0x24)?.length === 64,
    'Missing or mismatched hash, key hash or signature',
  );
  need(verify(null, digest, key, tlvs.get(0x24)!), 'Invalid distribution signature');
  return image.subarray(512, end);
}

function octal(field: Buffer) {
  const value = field.toString('ascii');
  need(field.every((b) => b < 128) && /^[ 0-7]*[\0 ]*$/.test(value), 'Invalid tar octal');
  const digits = value.replace(/[\0 ]+$/g, '').trim();
  const number = digits ? Number.parseInt(digits, 8) : 0;
  need(Number.isSafeInteger(number) && number >= 0, 'Tar number overflow');
  return number;
}
function tarString(field: Buffer) {
  const end = field.indexOf(0);
  need(end < 0 || zero(field.subarray(end)), 'Tar string padding');
  const bytes = end < 0 ? field : field.subarray(0, end);
  need(
    bytes.every((b) => b >= 32 && b < 127),
    'Non-ASCII tar name',
  );
  return bytes.toString('ascii');
}
function unpack(payload: Buffer) {
  const tar = gunzipSync(payload, { maxOutputLength: LIMIT });
  need(tar.length % 512 === 0, 'Tar block alignment');
  const files = new Map<string, Buffer>();
  let root = false;
  let offset = 0;
  for (; offset + 512 <= tar.length;) {
    const header = tar.subarray(offset, offset + 512);
    if (zero(header)) {
      need(offset + 1024 <= tar.length && zero(tar.subarray(offset)), 'Tar terminators');
      break;
    }
    const checksum = octal(header.subarray(148, 156));
    let sum = 0;
    for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 32 : header[i];
    need(sum === checksum, 'Tar checksum');
    need(
      (header.subarray(257, 265).equals(Buffer.from('ustar\0' + '00')) ||
        header.subarray(257, 265).equals(Buffer.from('ustar  \0'))) &&
        zero(header.subarray(345)),
      'Unsupported tar format',
    );
    for (const [start, end] of [
      [100, 108],
      [108, 116],
      [116, 124],
      [136, 148],
      [329, 337],
      [337, 345],
    ])
      octal(header.subarray(start, end));
    need(zero(header.subarray(157, 257)), 'Tar link target');
    const name = tarString(header.subarray(0, 100));
    const size = octal(header.subarray(124, 136));
    offset += 512;
    if (name === './' && header[156] === 53 && size === 0 && !root && files.size === 0) {
      root = true;
      continue;
    }
    const normalized = name.startsWith('./') ? name.slice(2) : name;
    need(
      (header[156] === 0 || header[156] === 48) &&
        [...REQUIRED, ...OPTIONAL].includes(normalized) &&
        !files.has(normalized),
      'Unknown, duplicate or non-regular tar member',
    );
    const padded = Math.ceil(size / 512) * 512;
    need(
      size <= LIMIT &&
        offset + padded <= tar.length &&
        zero(tar.subarray(offset + size, offset + padded)),
      'Tar member bounds or padding',
    );
    files.set(normalized, Buffer.from(tar.subarray(offset, offset + size)));
    offset += padded;
  }
  need(offset + 1024 <= tar.length, 'Missing tar terminators');
  for (const name of REQUIRED) need(files.has(name), `Missing ${name}`);
  return files;
}

export interface ElfSegment {
  address: number;
  data: Buffer;
}
/** Only file-backed physical bytes are programming operations; BSS is never flashed. */
export function parseElfSegments(elf: Buffer, core: 'm4' | 'm7' | 'loader'): ElfSegment[] {
  need(['m4', 'm7', 'loader'].includes(core), 'Unknown ELF core');
  const loader = core === 'loader';
  const base = loader ? 0x24000000 : core === 'm4' ? 0x08100000 : 0x08000000;
  const top = base + (loader ? 0x10000 : 0x100000);
  const ram = core === 'm4' ? 0x30000000 : 0x24000000;
  const ramTop = ram + (core === 'm4' ? 0x20000 : 0x80000);
  need(
    elf.length >= 52 &&
      elf.subarray(0, 7).equals(Buffer.from([127, 69, 76, 70, 1, 1, 1])) &&
      [0, 3].includes(elf[7]) &&
      zero(elf.subarray(8, 16)) &&
      elf.readUInt16LE(16) === 2 &&
      elf.readUInt16LE(18) === 40 &&
      elf.readUInt32LE(20) === 1 &&
      elf.readUInt16LE(40) === 52 &&
      (elf.readUInt32LE(36) & 0xff000600) === 0x05000400,
    'Expected ELF32 ARM hard-float executable',
  );
  const entry = elf.readUInt32LE(24);
  need(entry % 2 === 1 && entry >= base && entry < top, 'ELF entry bounds');
  const ph = elf.readUInt32LE(28),
    count = elf.readUInt16LE(44);
  need(
    elf.readUInt16LE(42) === 32 &&
      count > 0 &&
      count <= 128 &&
      ph >= 52 &&
      ph + count * 32 <= elf.length,
    'ELF program headers',
  );
  const segments: ElfSegment[] = [];
  for (let i = 0; i < count; i++) {
    const p = ph + i * 32;
    if (elf.readUInt32LE(p) !== 1) continue;
    const offset = elf.readUInt32LE(p + 4),
      virtual = elf.readUInt32LE(p + 8);
    const address = elf.readUInt32LE(p + 12),
      size = elf.readUInt32LE(p + 16),
      memory = elf.readUInt32LE(p + 20);
    const alignment = elf.readUInt32LE(p + 28);
    need(
      size <= memory &&
        offset + size <= elf.length &&
        (alignment <= 1 ||
          (Number.isInteger(Math.log2(alignment)) && virtual % alignment === offset % alignment)),
      'ELF segment layout',
    );
    const privateRam = virtual >= ram && virtual + memory <= ramTop;
    const sharedRam = virtual >= 0x38000000 && virtual + memory <= 0x38000800;
    need(
      (virtual >= base && virtual + memory <= top) ||
        (!loader && privateRam) ||
        (!loader && sharedRam && size === 0),
      'ELF virtual bounds',
    );
    if (!size) {
      need(
        (address >= ram && address + memory <= (loader ? top : ramTop)) ||
          (!loader && address >= 0x38000000 && address + memory <= 0x38000800),
        'ELF BSS bounds',
      );
      continue;
    }
    need(
      address >= base && address + size <= top && (!loader || address + memory <= top),
      'ELF physical bounds',
    );
    segments.push({ address, data: Buffer.from(elf.subarray(offset, offset + size)) });
  }
  segments.sort((a, b) => a.address - b.address);
  need(
    segments.length > 0 &&
      segments.every(
        (s, i) => !i || segments[i - 1].address + segments[i - 1].data.length <= s.address,
      ),
    'ELF overlapping or missing loads',
  );
  need(
    segments.some((s) => entry - 1 >= s.address && entry - 1 < s.address + s.data.length),
    'ELF entry not loaded',
  );
  if (!loader) {
    const vector = segments[0];
    need(vector.address === base && vector.data.length >= 8, 'Missing vector table');
    const sp = vector.data.readUInt32LE(0),
      reset = vector.data.readUInt32LE(4);
    need(
      sp > ram &&
        sp <= ramTop &&
        sp % 8 === 0 &&
        reset % 2 === 1 &&
        reset >= base &&
        reset < top &&
        segments.some((s) => reset - 1 >= s.address && reset - 1 < s.address + s.data.length),
      'Invalid vectors',
    );
  }
  return segments;
}

function json(data: Buffer): Record<string, unknown> {
  const value: unknown = JSON.parse(text(data));
  need(value && typeof value === 'object' && !Array.isArray(value), 'Expected JSON object');
  return value as Record<string, unknown>;
}
function keys(value: Record<string, unknown>, names: string[]) {
  need(
    Object.keys(value).sort().join(',') === names.sort().join(','),
    'Unexpected manifest fields',
  );
}
function crc32(bytes: Buffer) {
  let crc = 0xffffffff;
  for (const byte of bytes) {
    crc ^= byte;
    for (let bit = 0; bit < 8; bit++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return ~crc >>> 0;
}
function dashboard(files: Map<string, Buffer>) {
  const image = files.get('dashboard.qspi.bin')!,
    gzip = files.get('index.html.gz')!;
  need(
    image.length >= 4096 &&
      image.length <= 0x100000 &&
      image.length % 4096 === 0 &&
      image.subarray(0, 8).toString() === 'GIGAUI01' &&
      zero(image.subarray(16, 32)),
    'Dashboard header',
  );
  const length = image.readUInt32LE(8),
    crc = image.readUInt32LE(12);
  need(
    length >= 18 &&
      length <= 0x100000 - 32 &&
      image.length === Math.ceil((32 + length) / 4096) * 4096 &&
      gzip.length === length &&
      image.subarray(32, 32 + length).equals(gzip) &&
      image.subarray(32 + length).every((b) => b === 255) &&
      crc32(gzip) === crc &&
      gzip[0] === 31 &&
      gzip[1] === 139 &&
      gzip[2] === 8,
    'Dashboard payload',
  );
  need(
    /<!doctype html>/i.test(text(gunzipSync(gzip, { maxOutputLength: 8 * 1024 * 1024 }))),
    'Expected dashboard HTML',
  );
  const metadata = json(files.get('dashboard.qspi.bin.json')!);
  keys(metadata, [
    'format',
    'offset',
    'length',
    'gzipLength',
    'crc32',
    'sha256',
    'backupSha256',
    'status',
  ]);
  need(
    metadata.format === 'GIGAUI01' &&
      metadata.offset === 0xf00000 &&
      metadata.length === image.length &&
      metadata.gzipLength === length &&
      metadata.crc32 === crc.toString(16).padStart(8, '0') &&
      metadata.sha256 === hash(image).toString('hex') &&
      (metadata.backupSha256 === null ||
        (typeof metadata.backupSha256 === 'string' &&
          /^[a-f0-9]{64}$/.test(metadata.backupSha256))) &&
      typeof metadata.status === 'string' &&
      metadata.status.length > 0 &&
      metadata.status.length <= 256,
    'Dashboard metadata',
  );
}

/** Offline authentication and validation only. No files are executed or written. */
export function verifyDistribution(
  signed: Buffer,
  trustedPublicPem: Buffer | string,
): {
  files: Map<string, Buffer>;
  tag: string;
  commit: string;
} {
  const files = unpack(authenticatedPayload(signed, trustedPublicPem));
  const checksums = text(files.get('SHA256SUMS')!);
  need(checksums.endsWith('\n'), 'Checksum terminator');
  const seen = new Set<string>();
  for (const line of checksums.slice(0, -1).split('\n')) {
    const match = /^([a-f0-9]{64}) {2}([A-Za-z0-9_.-]+)$/.exec(line);
    need(
      match && match[2] !== 'SHA256SUMS' && files.has(match[2]) && !seen.has(match[2]),
      'Invalid checksum member',
    );
    need(hash(files.get(match[2])!).toString('hex') === match[1], 'Checksum mismatch');
    seen.add(match[2]);
  }
  need(seen.size === files.size - 1, 'Incomplete checksums');
  const build = text(files.get('BUILD.txt')!);
  const tags = [...build.matchAll(/^Tag: (.+)$/gm)],
    commits = [...build.matchAll(/^Commit: (.+)$/gm)];
  need(
    tags.length === 1 &&
      commits.length === 1 &&
      Array.from(tags[0][1]).every((c) => c.codePointAt(0)! >= 32 && c.codePointAt(0) !== 127) &&
      /^[a-f0-9]{40}$/.test(commits[0][1]),
    'Invalid build identity',
  );
  const tag = tags[0][1],
    commit = commits[0][1];
  parseElfSegments(files.get('m4.elf')!, 'm4');
  parseElfSegments(files.get('m7.elf')!, 'm7');
  dashboard(files);
  for (const [binary, manifest] of [
    ['qspi-loader.elf', 'qspi-loader.json'],
    ['installer-runtime.mjs', 'installer-manifest.json'],
  ])
    need(files.has(binary) === files.has(manifest), 'Incomplete optional asset pair');
  if (files.has('qspi-loader.elf')) {
    parseElfSegments(files.get('qspi-loader.elf')!, 'loader');
    const value = json(files.get('qspi-loader.json')!);
    keys(value, ['format', 'file', 'sha256', 'jedec', 'loadAddress', 'limit']);
    need(
      value.format === 'GIGAUSB-QSPI-LOADER-1' &&
        value.file === 'qspi-loader.elf' &&
        value.sha256 === hash(files.get('qspi-loader.elf')!).toString('hex') &&
        value.jedec === '1f8901' &&
        value.loadAddress === 0x24000000 &&
        value.limit === 0x24010000,
      'Loader manifest',
    );
  }
  if (files.has('installer-runtime.mjs')) {
    const value = json(files.get('installer-manifest.json')!);
    keys(value, ['format', 'file', 'sha256', 'tag', 'commit']);
    need(
      value.format === 'GIGAUSB-INSTALLER-1' &&
        value.file === 'installer-runtime.mjs' &&
        value.sha256 === hash(files.get('installer-runtime.mjs')!).toString('hex') &&
        value.tag === tag &&
        value.commit === commit,
      'Installer manifest',
    );
  }
  return { files, tag, commit };
}
