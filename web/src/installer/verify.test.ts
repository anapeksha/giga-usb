import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { gzipSync } from 'node:zlib';
import { describe, expect, it } from 'vitest';
import { parseElfSegments, verifyDistribution } from './verify';

const pair = generateKeyPairSync('ed25519');
const publicPem = pair.publicKey.export({ format: 'pem', type: 'spki' });
const digest = (b: Buffer) => createHash('sha256').update(b).digest();
function signed(payload: Buffer) {
  const header = Buffer.alloc(512, 255);
  header.fill(0, 0, 32);
  header.writeUInt32LE(0x96f3b83d);
  header.writeUInt16LE(512, 8);
  header.writeUInt32LE(payload.length, 12);
  header.writeUInt32LE(16, 16);
  const body = Buffer.concat([header, payload]);
  const hash = digest(body);
  const entries = [
    [0x10, hash],
    [1, digest(pair.publicKey.export({ format: 'der', type: 'spki' }))],
    [0x24, sign(null, hash, pair.privateKey)],
  ] as const;
  const tlvs = entries.map(([kind, value]) => {
    const h = Buffer.alloc(4);
    h[0] = kind;
    h.writeUInt16LE(value.length, 2);
    return Buffer.concat([h, value]);
  });
  const info = Buffer.alloc(4);
  info.writeUInt16LE(0x6907);
  info.writeUInt16LE(4 + tlvs.reduce((n, b) => n + b.length, 0), 2);
  return Buffer.concat([body, info, ...tlvs]);
}
function elf(core: 'm4' | 'm7' | 'loader') {
  const base = core === 'm4' ? 0x08100000 : core === 'm7' ? 0x08000000 : 0x24000000;
  const b = Buffer.alloc(264);
  Buffer.from([127, 69, 76, 70, 1, 1, 1]).copy(b);
  b.writeUInt16LE(2, 16);
  b.writeUInt16LE(40, 18);
  b.writeUInt32LE(1, 20);
  b.writeUInt32LE(base + 5, 24);
  b.writeUInt32LE(52, 28);
  b.writeUInt32LE(0x05000400, 36);
  b.writeUInt16LE(52, 40);
  b.writeUInt16LE(32, 42);
  b.writeUInt16LE(1, 44);
  [1, 256, base, base, 8, 8, 5, 4].forEach((v, i) => b.writeUInt32LE(v, 52 + i * 4));
  b.writeUInt32LE(core === 'm4' ? 0x30020000 : 0x24080000, 256);
  b.writeUInt32LE(base + 5, 260);
  return b;
}
function crc32(b: Buffer) {
  let crc = 0xffffffff;
  for (const byte of b) {
    crc ^= byte;
    for (let i = 0; i < 8; i++) crc = (crc >>> 1) ^ (crc & 1 ? 0xedb88320 : 0);
  }
  return ~crc >>> 0;
}
function fixture() {
  const gzip = gzipSync('<!doctype html><title>Offline</title>');
  const image = Buffer.alloc(4096, 255);
  image.fill(0, 0, 32);
  image.write('GIGAUI01');
  image.writeUInt32LE(gzip.length, 8);
  image.writeUInt32LE(crc32(gzip), 12);
  gzip.copy(image, 32);
  return new Map<string, Buffer>([
    ['m4.elf', elf('m4')],
    ['m7.elf', elf('m7')],
    ['index.html.gz', gzip],
    ['dashboard.qspi.bin', image],
    [
      'dashboard.qspi.bin.json',
      Buffer.from(
        JSON.stringify({
          format: 'GIGAUI01',
          offset: 0xf00000,
          length: 4096,
          gzipLength: gzip.length,
          crc32: crc32(gzip).toString(16).padStart(8, '0'),
          sha256: digest(image).toString('hex'),
          backupSha256: null,
          status: 'test only',
        }),
      ),
    ],
    ['BUILD.txt', Buffer.from(`Tag: release/候補\nCommit: ${'a'.repeat(40)}\n`)],
    ...['README.md', 'AGENTS.md', 'LICENSE', 'USB-IDS-LICENSE.txt', 'CHANGELOG.md'].map(
      (name): [string, Buffer] => [name, Buffer.from('test')],
    ),
  ]);
}
function checksum(h: Buffer) {
  h.fill(32, 148, 156);
  h.write(
    [...h]
      .reduce((a, b) => a + b, 0)
      .toString(8)
      .padStart(6, '0') + '\0 ',
    148,
  );
}
function member(name: string, data: Buffer, type = 48) {
  const h = Buffer.alloc(512);
  h.write(name);
  h.write('0000644\0', 100);
  h.write(data.length.toString(8).padStart(11, '0') + '\0', 124);
  h[156] = type;
  h.write('ustar\0' + '00', 257);
  checksum(h);
  return Buffer.concat([h, data, Buffer.alloc((512 - (data.length % 512)) % 512)]);
}
function archive(files = fixture(), mutate?: (tar: Buffer) => Buffer) {
  const all = new Map(files);
  if (!all.has('SHA256SUMS'))
    all.set(
      'SHA256SUMS',
      Buffer.from([...all].map(([n, b]) => `${digest(b).toString('hex')}  ${n}\n`).join('')),
    );
  const tar = Buffer.concat([...all].map(([n, b]) => member(n, b)).concat([Buffer.alloc(1024)]));
  return signed(gzipSync(mutate ? mutate(tar) : tar));
}
const verify = (b: Buffer) => verifyDistribution(b, publicPem);

describe('offline distribution verifier', () => {
  it('authenticates a complete bundle and preserves exact non-semver identity', () => {
    const result = verify(archive());
    expect(result.tag).toBe('release/候補');
    expect(result.commit).toBe('a'.repeat(40));
    expect(result.files.size).toBe(12);
    expect(parseElfSegments(result.files.get('m4.elf')!, 'm4')[0]).toEqual({
      address: 0x08100000,
      data: elf('m4').subarray(256),
    });
  });
  it('rejects a different trusted key', () => {
    const other = generateKeyPairSync('ed25519').publicKey.export({ type: 'spki', format: 'pem' });
    expect(() => verifyDistribution(archive(), other)).toThrow(/key hash/);
  });
  it.each([0, 4, 8, 10, 12, 16, 20, 32, 512])('rejects header/payload mutation at %i', (offset) => {
    const b = archive();
    b[offset] ^= 1;
    expect(() => verify(b)).toThrow();
  });
  it.each([
    'hash',
    'keyhash',
    'signature',
    'duplicate',
    'unsupported',
    'reserved',
    'length',
    'missing',
  ])('rejects %s TLV', (kind) => {
    let b = archive();
    const end = 512 + b.readUInt32LE(12);
    const offset = kind === 'keyhash' ? end + 40 : kind === 'signature' ? end + 76 : end + 4;
    if (kind === 'duplicate') b[end + 40] = 0x10;
    else if (kind === 'unsupported') b[offset] = 0x25;
    else if (kind === 'reserved') b[offset + 1] = 1;
    else if (kind === 'length') b.writeUInt16LE(65535, offset + 2);
    else if (kind === 'missing') {
      b = b.subarray(0, end + 76);
      b.writeUInt16LE(76, end + 2);
    } else b[offset + 4] ^= 1;
    expect(() => verify(b)).toThrow();
  });
  it('rejects truncation and trailing padding', () => {
    const b = archive();
    for (const value of [
      b.subarray(0, 20),
      b.subarray(0, -1),
      Buffer.concat([b, Buffer.alloc(1)]),
      Buffer.concat([b, Buffer.alloc(512, 255)]),
    ])
      expect(() => verify(value)).toThrow();
  });
  it('authenticates before attempting gzip extraction', () => {
    const b = signed(Buffer.from('not gzip'));
    b[b.length - 1] ^= 1;
    expect(() => verify(b)).toThrow(/signature/);
    expect(() => verify(signed(Buffer.from('not gzip')))).toThrow();
  });
  it.each(['../m4.elf', '/m4.elf', '././m4.elf', 'dir/m4.elf', 'unknown', 'm4.elf/'])(
    'rejects tar name %s',
    (name) => {
      expect(() =>
        verify(
          archive(fixture(), (t) => {
            t.fill(0, 0, 100);
            t.write(name);
            checksum(t.subarray(0, 512));
            return t;
          }),
        ),
      ).toThrow();
    },
  );
  it.each([49, 50, 53, 76, 75, 120, 103, 83])('rejects nonregular tar type %i', (type) => {
    expect(() =>
      verify(
        archive(fixture(), (t) => {
          t[156] = type;
          checksum(t.subarray(0, 512));
          return t;
        }),
      ),
    ).toThrow();
  });
  it.each(['checksum', 'octal', 'prefix', 'link', 'padding', 'terminator', 'duplicate'])(
    'rejects malformed tar %s',
    (kind) => {
      expect(() =>
        verify(
          archive(fixture(), (t) => {
            if (kind === 'checksum') t[0] ^= 1;
            if (kind === 'octal') {
              t[124] = 128;
              checksum(t.subarray(0, 512));
            }
            if (kind === 'prefix' || kind === 'link') {
              t[kind === 'prefix' ? 345 : 157] = 65;
              checksum(t.subarray(0, 512));
            }
            if (kind === 'padding') t[512 + 264] = 1;
            if (kind === 'terminator') return t.subarray(0, -512);
            if (kind === 'duplicate')
              return Buffer.concat([
                t.subarray(0, -1024),
                member('./m4.elf', elf('m4')),
                Buffer.alloc(1024),
              ]);
            return t;
          }),
        ),
      ).toThrow();
    },
  );
  it('accepts GNU root and ./ members', () => {
    expect(
      verify(
        archive(fixture(), (t) => {
          for (let o = 0; o < t.length - 1024;) {
            const h = t.subarray(o, o + 512),
              n = h.subarray(0, 100).toString().split('\0')[0];
            h.fill(0, 0, 100);
            h.write(`./${n}`);
            h.write('ustar  \0', 257);
            checksum(h);
            o += 512 + Math.ceil(Number.parseInt(h.subarray(124, 136).toString(), 8) / 512) * 512;
          }
          const root = member('./', Buffer.alloc(0), 53);
          return Buffer.concat([root, t]);
        }),
      ).files.size,
    ).toBe(12);
  });
  it.each(['missing', 'extra', 'duplicate', 'self', 'digest'])(
    'requires exact checksums: %s',
    (kind) => {
      const f = fixture();
      let lines = [...f].map(([n, b]) => `${digest(b).toString('hex')}  ${n}\n`);
      if (kind === 'missing') lines.pop();
      if (kind === 'extra') lines.push(`${'0'.repeat(64)}  unknown\n`);
      if (kind === 'duplicate') lines.push(lines[0]);
      if (kind === 'self') lines.push(`${'0'.repeat(64)}  SHA256SUMS\n`);
      if (kind === 'digest') lines = lines.map((l) => l.replace(/^[a-f0-9]{64}/, '0'.repeat(64)));
      f.set('SHA256SUMS', Buffer.from(lines.join('')));
      expect(() => verify(archive(f))).toThrow();
    },
  );
  it.each(['reserved', 'crc', 'padding', 'gzip', 'metadata', 'identity', 'optional'])(
    'rejects inconsistent asset %s',
    (kind) => {
      const f = fixture();
      if (kind === 'reserved') f.get('dashboard.qspi.bin')![16] = 1;
      if (kind === 'crc') f.get('dashboard.qspi.bin')![12] ^= 1;
      if (kind === 'padding') f.get('dashboard.qspi.bin')![4095] = 0;
      if (kind === 'gzip') f.get('index.html.gz')![0] = 0;
      if (kind === 'metadata') f.set('dashboard.qspi.bin.json', Buffer.from('{}'));
      if (kind === 'identity') f.set('BUILD.txt', Buffer.from('Tag: a\nTag: b\nCommit: bad\n'));
      if (kind === 'optional') f.set('installer-runtime.mjs', Buffer.from('throw 1'));
      expect(() => verify(archive(f))).toThrow();
    },
  );
  it('bounds archive decompression', () => {
    expect(() => verify(signed(gzipSync(Buffer.alloc(64 * 1024 * 1024 + 1))))).toThrow();
  });
});

describe('optional signed assets', () => {
  function optional() {
    const f = fixture();
    f.set('qspi-loader.elf', elf('loader'));
    f.set(
      'qspi-loader.json',
      Buffer.from(
        JSON.stringify({
          format: 'GIGAUSB-QSPI-LOADER-1',
          file: 'qspi-loader.elf',
          sha256: digest(f.get('qspi-loader.elf')!).toString('hex'),
          jedec: '1f8901',
          loadAddress: 0x24000000,
          limit: 0x24010000,
        }),
      ),
    );
    f.set('installer-runtime.mjs', Buffer.from('throw new Error("must never execute")'));
    f.set(
      'installer-manifest.json',
      Buffer.from(
        JSON.stringify({
          format: 'GIGAUSB-INSTALLER-1',
          file: 'installer-runtime.mjs',
          sha256: digest(f.get('installer-runtime.mjs')!).toString('hex'),
          tag: 'release/候補',
          commit: 'a'.repeat(40),
        }),
      ),
    );
    return f;
  }
  it('validates optional pairs without executing runtime code', () => {
    expect(verify(archive(optional())).files.size).toBe(16);
  });
  it.each(['qspi-loader.json', 'installer-manifest.json'])(
    'rejects execution instructions in %s',
    (name) => {
      const f = optional();
      const value = JSON.parse(f.get(name)!.toString());
      value.command = 'arbitrary';
      f.set(name, Buffer.from(JSON.stringify(value)));
      expect(() => verify(archive(f))).toThrow(/manifest fields/);
    },
  );
  it('rejects mismatched loader digest', () => {
    const f = optional();
    f.get('qspi-loader.elf')![263] ^= 1;
    expect(() => verify(archive(f))).toThrow(/Loader manifest/);
  });
});

describe('ELF programming segments', () => {
  it.each([
    ['machine', 18, 0],
    ['type', 16, 3],
    ['float', 36, 0],
    ['entry', 24, 0x08000000],
    ['physical', 64, 0x08100000],
    ['virtual', 60, 0x30000000],
    ['size', 68, 9],
    ['stack', 256, 0x24080001],
    ['reset', 260, 0x08100001],
  ])('rejects %s', (_, offset, value) => {
    const b = elf('m7');
    b.writeUInt32LE(value, offset);
    expect(() => parseElfSegments(b, 'm7')).toThrow();
  });
  it('allows RAM-only loader and rejects flash loader', () => {
    expect(parseElfSegments(elf('loader'), 'loader')[0].address).toBe(0x24000000);
    expect(() => parseElfSegments(elf('m7'), 'loader')).toThrow();
  });
  it('ignores valid zero-file BSS, rejects unknown BSS', () => {
    const b = elf('m7');
    b.writeUInt16LE(2, 44);
    [1, 264, 0x24000000, 0x24000000, 0, 128, 6, 1].forEach((v, i) =>
      b.writeUInt32LE(v, 84 + i * 4),
    );
    expect(parseElfSegments(b, 'm7')).toHaveLength(1);
    b.writeUInt32LE(0x40000000, 96);
    expect(() => parseElfSegments(b, 'm7')).toThrow();
  });
  it('rejects overlapping physical loads', () => {
    const b = elf('m7');
    b.writeUInt16LE(2, 44);
    b.copy(b, 84, 52, 84);
    expect(() => parseElfSegments(b, 'm7')).toThrow(/overlapping/);
  });
});
