import { createHash } from 'node:crypto';
import { mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { gzipSync } from 'node:zlib';
import { afterEach, describe, expect, it } from 'vitest';
import { assetImage, crc32, FLASH_SIZE, OFFSET } from '../asset-image';
import {
  cubeArguments,
  checkResume,
  installDistribution,
  invocationElf,
  parseOptions,
  runCommand,
  type Distribution,
  type Runner,
} from './install';

const directories: string[] = [];
afterEach(async () => {
  await Promise.all(directories.splice(0).map((p) => rm(p, { recursive: true, force: true })));
});
async function temporary() {
  const dir = await mkdtemp(join(tmpdir(), 'giga-installer-test-'));
  directories.push(dir);
  return dir;
}
function elf(core: 'm4' | 'm7' | 'loader') {
  const base = core === 'm4' ? 0x08100000 : core === 'm7' ? 0x08000000 : 0x24000000;
  const b = Buffer.alloc(272);
  Buffer.from([127, 69, 76, 70, 1, 1, 1]).copy(b);
  b.writeUInt16LE(2, 16);
  b.writeUInt16LE(40, 18);
  b.writeUInt32LE(1, 20);
  b.writeUInt32LE(base + 9, 24);
  b.writeUInt32LE(52, 28);
  b.writeUInt32LE(0x05000400, 36);
  b.writeUInt16LE(52, 40);
  b.writeUInt16LE(32, 42);
  b.writeUInt16LE(1, 44);
  [1, 256, base, base, 16, 16, 5, 4].forEach((v, i) => b.writeUInt32LE(v, 52 + 4 * i));
  b.writeUInt32LE(core === 'loader' ? 0x20020000 : core === 'm4' ? 0x30020000 : 0x24080000, 256);
  b.writeUInt32LE(base + 9, 260);
  return b;
}
function fixture(): Distribution {
  const gzip = gzipSync('<!doctype html><title>Installer fixture</title>');
  return {
    tag: 'test',
    commit: 'a'.repeat(40),
    files: new Map([
      ['m4.elf', elf('m4')],
      ['m7.elf', elf('m7')],
      ['qspi-loader.elf', elf('loader')],
      ['qspi-loader.json', Buffer.from('{}')],
      ['installer-runtime.mjs', Buffer.from('// fixture')],
      ['installer-manifest.json', Buffer.from('{}')],
      ['index.html.gz', gzip],
      ['dashboard.qspi.bin', assetImage(gzip)],
    ]),
  };
}
// Sanitized option fields from CubeProgrammer 2.24.0; no probe identity or hardware log.
const optionText = `OPTION BYTES BANK: 0
Read Out Protection:
RDP: 0xAA (Level 0, no protection)
BOR Level:
BOR_LEV: 0x0
User Configuration:
IWDG1_SW: 0x1
IWDG2_SW: 0x1
NRST_STOP_D1: 0x1
NRST_STBY_D1: 0x1
FZ_IWDG_STOP: 0x1
FZ_IWDG_SDBY: 0x1
BCM4: 0x0
BCM7: 0x1
NRST_STOP_D2: 0x1
NRST_STBY_D2: 0x1
SWAP_BANK: 0x0
IO_HSLV: 0x0
Boot address Option Bytes:
BOOT_CM7_ADD0: 0x800 (0x8000000)
BOOT_CM7_ADD1: 0x1FF0 (0x1FF00000)
BOOT_CM4_ADD0: 0x810 (0x8100000)
BOOT_CM4_ADD1: 0x1000 (0x10000000)
PCROP Protection:
PROT_AREA_START1: 0xFF (0x800FF00)
PROT_AREA_END1: 0x0 (0x80000FF)
DMEP1: 0x0
PROT_AREA_START2: 0xFF (0x810FF00)
PROT_AREA_END2: 0x0 (0x81000FF)
DMEP2: 0x0
DTCM RAM Protection:
ST_RAM_SIZE: 0x0 (2 KB)
Write Protection:
nWRP0: 0x1
nWRP1: 0x1
nWRP2: 0x1
nWRP3: 0x1
nWRP4: 0x1
nWRP5: 0x1
nWRP6: 0x1
nWRP7: 0x1
nWRP8: 0x1
nWRP9: 0x1
nWRP10: 0x1
nWRP11: 0x1
nWRP12: 0x1
nWRP13: 0x1
nWRP14: 0x1
nWRP15: 0x1
`;

function mockBoard(
  config: {
    badOptions?: boolean;
    unsupportedHelp?: boolean;
    helpResult?: { code: number; stdout: string; stderr: string };
    failInternal?: boolean;
    corruptInternal?: boolean;
    changedOptions?: boolean;
    failHalt?: boolean;
    environmentError?: boolean;
    badResult?: 'magic' | 'count' | 'crc' | 'phase' | 'error' | 'state' | 'sr1' | 'sr2' | 'sr3';
    owner?: boolean;
    failProgram?: boolean;
    corruptStaging?: boolean;
    corruptOutside?: boolean;
    occupied?: boolean;
    staleSector?: boolean;
    identical?: boolean;
  } = {},
) {
  const distribution = fixture();
  const external = Buffer.alloc(FLASH_SIZE, 255);
  external[42] = 17;
  if (config.occupied) external[OFFSET] = 1;
  if (config.identical) distribution.files.get('dashboard.qspi.bin')!.copy(external, OFFSET);
  const internal = Buffer.alloc(0x200000, 255);
  let result = Buffer.alloc(64),
    data = Buffer.alloc(0),
    invocation = 0;
  const writes: string[] = [],
    operations: number[] = [];
  let reset = 0,
    halt = 0,
    environmentReads = 0,
    optionReads = 0,
    sectorReads = 0;
  const failure = new Error('simulated uncertain write timeout');
  const runner: Runner = async (program, args, timeout) => {
    expect(timeout).toBeGreaterThan(0);
    if (program === '/bin/ps')
      return { code: 0, stdout: config.owner ? '123 openocd\n' : '', stderr: '' };
    if (args[0] === '--help')
      return (
        config.helpResult ?? {
          code: 0,
          stdout: config.unsupportedHelp ? 'unsupported' : '\n -halt : Halt core\n',
          stderr: '',
        }
      );
    expect(args.slice(0, 5)).toEqual([
      '-c',
      'port=SWD',
      expect.stringMatching(/^sn=/),
      'freq=1000',
      expect.stringMatching(/^mode=/),
    ]);
    expect(args.some((v) => ['-e', '-rdu'].includes(v))).toBe(false);
    if (args.includes('-ob')) {
      optionReads++;
      return {
        code: 0,
        stdout: config.badOptions
          ? optionText.replace('BCM4: 0x0', 'BCM4: 0x1')
          : config.changedOptions && optionReads > 1
            ? optionText.replace('BOR_LEV: 0x0', 'BOR_LEV: 0x1')
            : optionText,
        stderr: '',
      };
    }
    if (args.includes('-halt')) {
      halt++;
      if (config.failHalt) throw new Error('simulated halt failure');
    }
    if (args.includes('-rst')) reset++;
    const w = args.indexOf('-w');
    if (w >= 0) {
      const path = args[w + 1];
      const bytes = await readFile(path);
      if (!args.includes('-g')) {
        const core = path.endsWith('m4.elf') ? 'm4' : 'm7';
        writes.push(core);
        if (config.failInternal) throw failure;
        bytes.subarray(256).copy(internal, core === 'm4' ? 0x100000 : 0);
        if (config.corruptInternal) internal[core === 'm4' ? 0x100000 : 0] ^= 1;
      } else {
        invocation++;
        const count = bytes.readUInt16LE(44);
        const segments = Array.from({ length: count }, (_, i) => {
          const p = 52 + i * 32;
          return {
            address: bytes.readUInt32LE(p + 12),
            data: bytes.subarray(
              bytes.readUInt32LE(p + 4),
              bytes.readUInt32LE(p + 4) + bytes.readUInt32LE(p + 16),
            ),
          };
        });
        const mailbox = segments.find((s) => s.address === 0x24010000)!.data;
        expect(mailbox.length).toBe(96);
        expect(mailbox.subarray(32).every((b) => b === 0)).toBe(true);
        const op = mailbox.readUInt32LE(8),
          offset = mailbox.readUInt32LE(12),
          length = mailbox.readUInt32LE(16);
        operations.push(op);
        if (op !== 1) {
          const staging = segments.find((s) => s.address === 0x24012000)!.data;
          expect(staging.length).toBe(4096);
          const u = args.indexOf('-u');
          expect(args.slice(u, u + 3)).toEqual(['-u', '0x24012000', '4096']);
          expect(u).toBeLessThan(args.indexOf('-g'));
          const snapshot = Buffer.from(staging);
          if (config.corruptStaging) snapshot[0] ^= 1;
          await writeFile(args[u + 3], snapshot, { flag: 'wx' });
          if (config.failProgram) throw failure;
          const input = staging.subarray(0, length);
          expect(crc32(input)).toBe(mailbox.readUInt32LE(20));
          if (op === 2 && offset === OFFSET)
            expect(input.subarray(0, 32).every((b) => b === 255)).toBe(true);
          input.copy(external, offset);
          if (config.corruptOutside) external[0] = 0;
        } else if (
          length === 4096 &&
          offset === OFFSET &&
          config.staleSector &&
          ++sectorReads === 1
        )
          external[OFFSET + 100] = 0;
        data = Buffer.from(external.subarray(offset, offset + length));
        result = Buffer.alloc(64);
        [0x31524c47, 2, 0, 4, length, 0x01891f, 0x80, 0x3b, 0xff, crc32(data), length].forEach(
          (v, i) => result.writeUInt32LE(v, 4 * i),
        );
      }
    }
    const u = args.indexOf('-u');
    if (u >= 0 && w < 0) {
      const address = Number(args[u + 1]),
        length = Number(args[u + 2]);
      let bytes: Buffer;
      if (address === 0x08000000) bytes = internal;
      else if (address === 0x24010020) {
        bytes = Buffer.from(result);
        if (config.environmentError) {
          bytes.fill(0);
          bytes.writeUInt32LE(0x31524c47, 0);
          bytes.writeUInt32LE(65535, 4);
          bytes.writeUInt32LE(15, 8);
        }
        if (config.badResult) {
          const field = {
            magic: 0,
            count: 40,
            crc: 36,
            phase: 12,
            error: 8,
            state: 4,
            sr1: 24,
            sr2: 28,
            sr3: 32,
          }[config.badResult];
          bytes.writeUInt32LE(
            config.badResult === 'state' ? 65535 : config.badResult === 'sr3' ? 256 : 99,
            field,
          );
        }
      } else if (address === 0x24010200) {
        environmentReads++;
        bytes = Buffer.alloc(32);
        bytes.writeUInt32LE(5, 0);
      } else if (address === 0x24010100) {
        bytes = Buffer.alloc(256);
        bytes.writeUInt32LE(0x50444653);
        bytes.writeUInt32LE(0x07ffffff, 0x34);
        bytes.writeUInt32LE(0x520f200c, 0x4c);
      } else {
        expect(address).toBe(0x24012000);
        bytes = data;
      }
      expect(bytes.length).toBe(length);
      await writeFile(args[u + 3], bytes, { flag: 'wx' });
    }
    return { code: 0, stdout: '', stderr: '' };
  };
  return {
    distribution,
    runner,
    writes,
    operations,
    external,
    invocations: () => invocation,
    resets: () => reset,
    halts: () => halt,
    environmentReads: () => environmentReads,
    failure,
  };
}
const digest = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
async function priorEvidence(board: ReturnType<typeof mockBoard>, committed = false) {
  const resumeFrom = await temporary();
  const probe = `RESUMEPROBE${process.pid}`;
  const image = board.distribution.files.get('dashboard.qspi.bin')!;
  const original = Buffer.from(board.external);
  image.copy(original, OFFSET);
  const files: [string, Buffer | string][] = [
    ['external-before-1.bin', original],
    ['external-before-2.bin', original],
    ['external-before-1.sha256', digest(original) + '\n'],
    ['external-before-2.sha256', digest(original) + '\n'],
    [
      'distribution.json',
      JSON.stringify({ tag: board.distribution.tag, commit: board.distribution.commit }),
    ],
    ['m4.elf', board.distribution.files.get('m4.elf')!],
    ['m7.elf', board.distribution.files.get('m7.elf')!],
    ['options-before.txt', optionText],
    [
      'install.jsonl',
      JSON.stringify({
        time: new Date().toISOString(),
        message: `Cube ${JSON.stringify(cubeArguments(probe, 'UR', ['-ob', 'displ']))}`,
      }) + '\n',
    ],
  ];
  await Promise.all(
    files.map(([name, data]) => writeFile(join(resumeFrom, name), data, { flag: 'wx' })),
  );
  image.copy(board.external, OFFSET);
  if (!committed) board.external.fill(255, OFFSET, OFFSET + 32);
  return { resumeFrom, probe, original };
}
let probeCounter = 0;
async function install(board: ReturnType<typeof mockBoard>, extra = {}) {
  return installDistribution(board.distribution, {
    programmer: '/test/CubeProgrammer',
    probe: `TESTPROBE${process.pid}${++probeCounter}`,
    backupDir: await temporary(),
    replaceDashboard: false,
    runner: board.runner,
    ...extra,
  });
}

describe('installer safety boundaries', () => {
  it('accepts only fixed Cube operation shapes', () => {
    expect(cubeArguments('12345678', 'UR', ['-ob', 'displ'])).toContain('reset=HWrst');
    for (const args of [
      ['-e', 'all'],
      ['-ob', 'RDP=0xAA'],
      ['-rdu'],
      ['-w', '/tmp/x.bin', '-v'],
      ['-rst', '-e'],
    ])
      expect(() => cubeArguments('12345678', 'UR', args)).toThrow();
    expect(() => cubeArguments('123; bad', 'UR', ['-rst'])).toThrow();
    expect(() => cubeArguments('12345678', 'HOTPLUG', ['-rst'])).toThrow();
  });
  it('parses all 41 actual Cube fields, strips ANSI, and excludes upload metadata', () => {
    const parsed: unknown[] = JSON.parse(parseOptions(optionText));
    expect(parsed).toHaveLength(41);
    const escape = String.fromCharCode(27);
    const colored = optionText
      .split('\n')
      .map((line) => `${escape}[39;49m${escape}[0m${line}`)
      .join('\n');
    expect(parseOptions(`Bank: 0x00\nAddress: 0x5200201c\nSize: 308 Bytes\n${colored}`)).toBe(
      parseOptions(optionText),
    );
    expect(parseOptions(`Bank: 0x01\nAddress: 0x12345678\nSize: 999 Bytes\n${optionText}`)).toBe(
      parseOptions(optionText),
    );
  });
  it('requires the complete known layout and rejects ambiguous or malformed fields', () => {
    for (const text of [
      optionText.replace('RDP: 0xAA', 'RDP: 0xCC'),
      optionText.replace('PROT_AREA_END1: 0x0', 'PROT_AREA_END1: 0xFF'),
      optionText.replace('PROT_AREA_START2: 0xFF', 'PROT_AREA_START2: 0x100'),
      optionText.replace('BCM7: 0x1', ''),
      optionText.replace('BOR_LEV: 0x0', 'BOR_LEV: unknown'),
      optionText.replace('nWRP0: 0x1', 'WRP1: 0xFF'),
      optionText + '\nOTHER: 3',
      optionText + '\nnWRP0: 0x1',
      optionText + optionText,
    ])
      expect(() => parseOptions(text)).toThrow();
    for (let i = 0; i < 16; i++) {
      expect(() => parseOptions(optionText.replace(`nWRP${i}: 0x1`, `nWRP${i}: 0x0`))).toThrow(
        'unprotected',
      );
      expect(() => parseOptions(optionText.replace(`nWRP${i}: 0x1`, ''))).toThrow('41 fields');
    }
  });
  it('compares every displayed option field, including fields not changed by installation', () => {
    const original: [string, number][] = JSON.parse(parseOptions(optionText));
    for (const [key, value] of original) {
      const name = key.startsWith('NWRP') ? key.replace('NWRP', 'nWRP') : key;
      const changed = optionText.replace(
        new RegExp(`(${name}: )0x[\\da-f]+`, 'i'),
        `$1${value === 0 ? '0x1' : '0x0'}`,
      );
      expect(changed).not.toBe(optionText);
      let parsed: string;
      try {
        parsed = parseOptions(changed);
      } catch {
        continue;
      }
      expect(parsed).not.toBe(parseOptions(optionText));
    }
  });
  it('constructs disjoint RAM segments and keeps code/entry intact', () => {
    const template = elf('loader');
    const out = invocationElf(template, 2, OFFSET, 4096, Buffer.alloc(4096, 255));
    expect(out.readUInt16LE(44)).toBe(3);
    expect(out.readUInt32LE(24)).toBe(template.readUInt32LE(24));
    const p = out.readUInt32LE(56);
    expect(out.subarray(p, p + 16)).toEqual(template.subarray(256));
    for (const [op, offset, length] of [
      [1, 0, 262145],
      [2, OFFSET - 4096, 4096],
      [3, OFFSET + 1, 32],
      [1, FLASH_SIZE, 1],
    ])
      expect(() => invocationElf(template, op, offset, length)).toThrow();
    expect(() => invocationElf(template, 2, OFFSET, 4096, Buffer.alloc(4096))).toThrow();
    const bss = Buffer.from(template);
    bss.writeUInt32LE(20, 72);
    expect(() => invocationElf(bss, 1, 0, 1)).toThrow();
  });
  it.each([1, 2, 3])(
    'emits consistent ELF32 sections and aligned exact RAM loads for operation %i',
    (op) => {
      const template = Buffer.concat([elf('loader'), Buffer.from([0x71, 0x82])]);
      template.writeUInt32LE(18, 68);
      template.writeUInt32LE(18, 72);
      const data = op === 1 ? undefined : Buffer.alloc(op === 2 ? 4096 : 32, 255);
      const length = data?.length ?? 262144;
      const out = invocationElf(template, op, OFFSET, length, data);
      const count = data ? 3 : 2;
      const sh = out.readUInt32LE(32);
      expect(out.readUInt32LE(28)).toBe(52);
      expect(out.readUInt16LE(42)).toBe(32);
      expect(out.readUInt16LE(44)).toBe(count);
      expect(out.readUInt16LE(46)).toBe(40);
      expect(out.readUInt16LE(48)).toBe(count + 2);
      expect(out.readUInt16LE(50)).toBe(count + 1);
      expect(sh % 4).toBe(0);
      expect(sh + (count + 2) * 40).toBe(out.length);
      expect(out.subarray(sh, sh + 40)).toEqual(Buffer.alloc(40));
      const stringsHeader = sh + (count + 1) * 40;
      expect(out.readUInt32LE(stringsHeader + 4)).toBe(3);
      expect(out.readUInt32LE(stringsHeader + 8)).toBe(0);
      const stringsOffset = out.readUInt32LE(stringsHeader + 16);
      const strings = out.subarray(
        stringsOffset,
        stringsOffset + out.readUInt32LE(stringsHeader + 20),
      );
      expect(stringsOffset % 4).toBe(0);
      expect(stringsOffset + strings.length).toBeLessThanOrEqual(sh);
      const sectionName = (header: number) => {
        const offset = out.readUInt32LE(header);
        return strings.subarray(offset, strings.indexOf(0, offset)).toString('ascii');
      };
      expect(sectionName(stringsHeader)).toBe('.shstrtab');
      const expected = [
        { address: 0x24000000, data: template.subarray(256), name: '.code' },
        { address: 0x24010000, data: Buffer.alloc(96), name: '.mailbox' },
        ...(data
          ? [
              {
                address: 0x24012000,
                data: Buffer.concat([data, Buffer.alloc(4096 - data.length, 255)]),
                name: '.data',
              },
            ]
          : []),
      ];
      let previousEnd = 52 + count * 32;
      expected.forEach((segment, i) => {
        const p = 52 + i * 32,
          s = sh + (i + 1) * 40;
        const offset = out.readUInt32LE(p + 4),
          size = out.readUInt32LE(p + 16);
        expect(out.readUInt32LE(p)).toBe(1);
        expect(out.readUInt32LE(p + 8)).toBe(segment.address);
        expect(out.readUInt32LE(p + 12)).toBe(segment.address);
        expect(out.readUInt32LE(p + 20)).toBe(segment.data.length);
        expect(size).toBe(segment.data.length);
        expect(out.readUInt32LE(p + 24)).toBe(i === 0 ? 5 : 6);
        expect(out.readUInt32LE(p + 28)).toBe(4);
        expect(offset % 4).toBe(segment.address % 4);
        expect(offset).toBeGreaterThanOrEqual(previousEnd);
        expect(out.subarray(previousEnd, offset).every((b) => b === 0)).toBe(true);
        expect(offset + size).toBeLessThanOrEqual(stringsOffset);
        expect(sectionName(s)).toBe(segment.name);
        expect(out.readUInt32LE(s + 4)).toBe(1);
        expect(out.readUInt32LE(s + 8)).toBe(i === 0 ? 6 : 3);
        expect(out.readUInt32LE(s + 12)).toBe(segment.address);
        expect(out.readUInt32LE(s + 16)).toBe(offset);
        expect(out.readUInt32LE(s + 20)).toBe(size);
        expect(out.readUInt32LE(s + 32)).toBe(4);
        const bytes = out.subarray(offset, offset + size);
        if (i !== 1) expect(bytes).toEqual(segment.data);
        else {
          expect(bytes.subarray(32)).toEqual(Buffer.alloc(64));
          expect(bytes.readUInt32LE(0)).toBe(0x31494c47);
          expect(bytes.readUInt32LE(8)).toBe(op);
          expect(bytes.readUInt32LE(12)).toBe(OFFSET);
          expect(bytes.readUInt32LE(16)).toBe(length);
        }
        previousEnd = offset + size;
      });
      expect(out.readUInt32LE(24)).toBe(template.readUInt32LE(24));
    },
  );
  it('rejects absent runtime before invoking any command', async () => {
    const board = mockBoard();
    board.distribution.files.delete('installer-runtime.mjs');
    let calls = 0;
    board.runner = async () => {
      calls++;
      throw new Error('unexpected IO');
    };
    await expect(install(board)).rejects.toThrow('Missing verified');
    expect(calls).toBe(0);
  });
  it('detects an existing probe owner without touching the board', async () => {
    const board = mockBoard({ owner: true });
    await expect(install(board)).rejects.toThrow('Release existing probe');
    expect(board.resets()).toBe(0);
    expect(board.halts()).toBe(0);
    expect(board.environmentReads()).toBe(0);
    expect(board.invocations()).toBe(0);
  });
  it('refuses incorrect options and attempts bounded halt without reset', async () => {
    const board = mockBoard({ badOptions: true });
    await expect(install(board)).rejects.toThrow('Unsafe boot');
    expect(board.invocations()).toBe(0);
    expect(board.resets()).toBe(0);
    expect(board.halts()).toBe(1);
  });
  it('backs up twice, writes header last, preserves the entire chip and flashes M4 then M7', async () => {
    const board = mockBoard();
    const result = await install(board);
    expect(result.verification).toBe('program-readback-only');
    expect(result.dashboardWritten).toBe(true);
    expect(board.writes).toEqual(['m4', 'm7']);
    expect(board.operations.slice(0, 128).every((op) => op === 1)).toBe(true);
    expect(board.operations.filter((op) => op !== 1)).toEqual([2, 3]);
    expect(board.external[42]).toBe(17);
    expect(await readFile(join(result.backupDir, 'external-before-1.bin'))).toEqual(
      await readFile(join(result.backupDir, 'external-before-2.bin')),
    );
    expect(board.resets()).toBe(1);
  }, 30000);
  it.each([
    [{ occupied: true }, 'occupied range'],
    [{ staleSector: true }, 'Sector changed'],
    [{ failProgram: true }, 'uncertain write'],
    [{ corruptOutside: true }, 'preservation comparison'],
  ])(
    'never flashes applications after external failure %j',
    async (config, message) => {
      const board = mockBoard(config);
      await expect(install(board)).rejects.toThrow(message);
      expect(board.writes).toEqual([]);
      expect(board.resets()).toBe(0);
      expect(board.halts()).toBe(1);
      if ('failProgram' in config || 'staleSector' in config)
        expect(board.operations).not.toContain(3);
    },
    30000,
  );
  it('skips an identical dashboard unless replacement and forced rewrite are explicit', async () => {
    const board = mockBoard({ identical: true });
    const result = await install(board, { replaceDashboard: true });
    expect(result.dashboardWritten).toBe(false);
    expect(board.operations.every((op) => op === 1)).toBe(true);
    const forced = mockBoard({ identical: true });
    expect(
      (await install(forced, { replaceDashboard: true, forceDashboardRewrite: true }))
        .dashboardWritten,
    ).toBe(true);
    expect(forced.operations.filter((op) => op !== 1)).toEqual([2, 3]);
  }, 30000);
  it.each(['magic', 'count', 'crc', 'phase', 'error', 'state', 'sr1', 'sr2', 'sr3'] as const)(
    'rejects malformed fresh loader %s without writing',
    async (badResult) => {
      const board = mockBoard({ badResult });
      await expect(install(board)).rejects.toThrow('Installation failed');
      expect(board.writes).toEqual([]);
      expect(board.operations).toEqual([1]);
      expect(board.resets()).toBe(0);
      expect(board.halts()).toBe(1);
    },
  );
  it.each(['stdout', 'stderr'] as const)(
    'recognizes actual Cube ANSI help on %s',
    async (stream) => {
      const escape = String.fromCharCode(27);
      const help = `${escape}[39;49m${escape}[0m${escape}[90m -halt${escape}[39;49m${escape}[0m                  : ${escape}[90mHalt core${escape}[39;49m${escape}[0m\n`;
      const board = mockBoard({
        badOptions: true,
        helpResult: {
          code: 0,
          stdout: stream === 'stdout' ? help : '',
          stderr: stream === 'stderr' ? help : '',
        },
      });
      // Reaching option validation proves preflight passed without running a loader.
      await expect(install(board)).rejects.toThrow('Unsafe boot');
      expect(board.invocations()).toBe(0);
      expect(board.resets()).toBe(0);
      expect(board.halts()).toBe(1);
    },
  );
  it.each([
    { code: 1, stdout: '\n -halt : Halt core\n', stderr: '' },
    { code: 0, stdout: '\n -halt : Unknown command\n', stderr: '' },
    { code: 0, stdout: '', stderr: '\n -run : Run core\n' },
  ])('rejects failed or unrecognized help before board access %j', async (helpResult) => {
    const board = mockBoard({ helpResult });
    await expect(install(board)).rejects.toThrow('confirm -halt');
    expect(board.invocations()).toBe(0);
    expect(board.resets()).toBe(0);
    expect(board.halts()).toBe(0);
    expect(board.environmentReads()).toBe(0);
  });
  it('requires documented halt support before touching the board', async () => {
    const board = mockBoard({ unsupportedHelp: true });
    await expect(install(board)).rejects.toThrow('confirm -halt');
    expect(board.invocations()).toBe(0);
    expect(board.resets()).toBe(0);
    expect(board.halts()).toBe(0);
  });
  it.each([
    [{ failInternal: true }, 'uncertain write timeout'],
    [{ corruptInternal: true }, 'PT_LOAD readback mismatch'],
    [{ changedOptions: true }, 'Option fields changed'],
  ])(
    'halts without reset after possible internal mutation %j',
    async (config, message) => {
      const board = mockBoard({ ...config, identical: true });
      await expect(install(board, { replaceDashboard: true })).rejects.toThrow(message);
      expect(board.writes.length).toBeGreaterThan(0);
      expect(board.resets()).toBe(0);
      expect(board.halts()).toBe(1);
      expect(board.environmentReads()).toBe(1);
    },
    30000,
  );
  it('preserves the original mutating timeout even when recovery halt fails', async () => {
    const board = mockBoard({ failProgram: true, failHalt: true });
    let failure: unknown;
    try {
      await install(board);
    } catch (error) {
      failure = error;
    }
    expect(failure).toBeInstanceOf(Error);
    expect((failure as Error).cause).toBe(board.failure);
    expect((failure as Error).message).toContain('HALT FAILED');
    expect((failure as Error).message).toContain('Do not run or power-cycle');
    expect(board.operations.filter((op) => op !== 1)).toEqual([2]);
    expect(board.resets()).toBe(0);
    expect(board.halts()).toBe(1);
  }, 30000);
  it('retains environment-gate error 15 diagnostics before halt', async () => {
    const board = mockBoard({ environmentError: true });
    await expect(install(board)).rejects.toThrow('environment gate (error 15)');
    expect(board.operations).toEqual([1]);
    expect(board.environmentReads()).toBe(1);
    expect(board.halts()).toBe(1);
    expect(board.resets()).toBe(0);
  });
  it('limits combined staging uploads to the exact RAM address, size and command ordering', () => {
    const valid = [
      '-w',
      '/tmp/operation.elf',
      '-v',
      '-u',
      '0x24012000',
      '4096',
      '/tmp/staged.bin',
      '-g',
      '0x24000000',
    ];
    expect(cubeArguments('12345678', 'UR', valid).slice(-9)).toEqual(valid);
    for (const [index, value] of [
      [4, '0x08000000'],
      [5, '32'],
      [5, '262144'],
      [8, '0x08000000'],
      [6, 'relative.bin'],
    ] as const) {
      const bad = [...valid];
      bad[index] = value;
      expect(() => cubeArguments('12345678', 'UR', bad)).toThrow();
    }
    expect(() => cubeArguments('12345678', 'HOTPLUG', valid)).toThrow();
  });
  it.each([false, true])(
    'resumes only the header or skips an already committed image (committed=%s)',
    async (committed) => {
      const board = mockBoard();
      const prior = await priorEvidence(board, committed);
      const result = await install(board, { ...prior, replaceDashboard: true });
      expect(board.operations.slice(0, 128).every((op) => op === 1)).toBe(true);
      expect(board.operations.filter((op) => op !== 1)).toEqual(committed ? [] : [3]);
      expect(board.writes).toEqual(['m4', 'm7']);
      expect(result.dashboardWritten).toBe(!committed);
      expect(board.resets()).toBe(1);
      if (!committed) {
        const names = await readdir(result.backupDir);
        const metaName = names.find((name) => name.endsWith('-staging.json'))!;
        const metadata = JSON.parse(await readFile(join(result.backupDir, metaName), 'utf8')) as {
          stagingPath: string;
          logicalLength: number;
          stagingLength: number;
          inputCrc32: number;
        };
        expect(metadata.logicalLength).toBe(32);
        expect(metadata.stagingLength).toBe(4096);
        const snapshot = await readFile(metadata.stagingPath);
        expect(snapshot.subarray(0, 32)).toEqual(
          board.distribution.files.get('dashboard.qspi.bin')!.subarray(0, 32),
        );
        expect(snapshot.subarray(32).every((b) => b === 255)).toBe(true);
        expect(crc32(snapshot.subarray(0, 32))).toBe(metadata.inputCrc32);
      }
    },
    30000,
  );
  it.each([
    'hash',
    'backup',
    'firmware',
    'identity',
    'imageHash',
    'occupied',
    'log',
    'probe',
    'missing',
  ] as const)('rejects invalid resume %s before any command', async (kind) => {
    const board = mockBoard();
    const prior = await priorEvidence(board);
    if (kind === 'hash')
      await writeFile(join(prior.resumeFrom, 'external-before-1.sha256'), '0'.repeat(64));
    if (kind === 'backup') {
      const changed = Buffer.from(prior.original);
      changed[7] ^= 1;
      await writeFile(join(prior.resumeFrom, 'external-before-2.bin'), changed);
      await writeFile(join(prior.resumeFrom, 'external-before-2.sha256'), digest(changed) + '\n');
    }
    if (kind === 'occupied') {
      const changed = Buffer.from(prior.original);
      changed[OFFSET] = 0;
      for (const index of [1, 2]) {
        await writeFile(join(prior.resumeFrom, `external-before-${index}.bin`), changed);
        await writeFile(
          join(prior.resumeFrom, `external-before-${index}.sha256`),
          digest(changed) + '\n',
        );
      }
    }
    if (kind === 'imageHash')
      await writeFile(
        join(prior.resumeFrom, 'distribution.json'),
        JSON.stringify({
          tag: board.distribution.tag,
          commit: board.distribution.commit,
          imageSha256: '0'.repeat(64),
        }),
      );
    if (kind === 'firmware')
      await writeFile(join(prior.resumeFrom, 'm4.elf'), Buffer.from('different'));
    if (kind === 'identity')
      await writeFile(
        join(prior.resumeFrom, 'distribution.json'),
        JSON.stringify({ tag: 'other', commit: board.distribution.commit }),
      );
    if (kind === 'log')
      await writeFile(join(prior.resumeFrom, 'install.jsonl'), '{"unknown":"row"}\n');
    if (kind === 'missing') await rm(join(prior.resumeFrom, 'external-before-2.bin'));
    let calls = 0;
    board.runner = async () => {
      calls++;
      throw new Error('unexpected board access');
    };
    await expect(
      install(board, {
        ...prior,
        probe: kind === 'probe' ? 'WRONGPROBE' : prior.probe,
        replaceDashboard: true,
      }),
    ).rejects.toThrow();
    expect(calls).toBe(0);
  });
  it.each(['inside', 'outside', 'options'] as const)(
    'rejects fresh resume %s mismatch without persistent writes',
    async (kind) => {
      const board = mockBoard();
      const prior = await priorEvidence(board);
      if (kind === 'inside') board.external[OFFSET + 100] ^= 1;
      if (kind === 'outside') board.external[7] ^= 1;
      if (kind === 'options')
        await writeFile(
          join(prior.resumeFrom, 'options-before.txt'),
          optionText.replace('BOR_LEV: 0x0', 'BOR_LEV: 0x1'),
        );
      await expect(install(board, { ...prior, replaceDashboard: true })).rejects.toThrow(/Resume/);
      expect(board.operations.every((op) => op === 1)).toBe(true);
      expect(board.writes).toEqual([]);
      expect(board.resets()).toBe(0);
    },
    30000,
  );
  it('refuses missing replacement consent or forced rewrite on resume', async () => {
    const board = mockBoard();
    await expect(install(board, { resumeFrom: '/not-read' })).rejects.toThrow(
      'replacement consent',
    );
    await expect(
      install(board, {
        resumeFrom: '/not-read',
        replaceDashboard: true,
        forceDashboardRewrite: true,
      }),
    ).rejects.toThrow('header-only');
    expect(board.invocations()).toBe(0);
  });
  it('checks staged and committed images byte-for-byte, not by header CRC alone', () => {
    const image = fixture().files.get('dashboard.qspi.bin')!;
    const original = Buffer.alloc(FLASH_SIZE, 255);
    const current = Buffer.from(original);
    image.copy(current, OFFSET);
    expect(checkResume(original, current, image)).toBe('committed');
    current.fill(255, OFFSET, OFFSET + 32);
    expect(checkResume(original, current, image)).toBe('staged');
    current[OFFSET + 100] ^= 1;
    expect(() => checkResume(original, current, image)).toThrow('neither exact');
    expect(() => checkResume(original, Buffer.alloc(1), image)).toThrow('full-chip');
  });
  it('retains bad staging evidence and never retries or flashes applications', async () => {
    const board = mockBoard({ corruptStaging: true });
    const prior = await priorEvidence(board);
    await expect(install(board, { ...prior, replaceDashboard: true })).rejects.toThrow(
      'RAM staging differs',
    );
    expect(board.operations.filter((op) => op !== 1)).toEqual([3]);
    expect(board.writes).toEqual([]);
    expect(board.resets()).toBe(0);
    expect(board.halts()).toBe(1);
  }, 30000);
  it('terminates a timed-out direct child', async () => {
    await expect(
      runCommand(process.execPath, ['-e', 'setTimeout(() => {}, 10000)'], 30),
    ).rejects.toThrow('timed out');
  });
});
