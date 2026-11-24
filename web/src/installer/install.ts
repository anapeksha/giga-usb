import { spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFile, mkdir, mkdtemp, open, readFile, unlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { isAbsolute, join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';
import { assetImage, checkBackup, crc32, FLASH_SIZE, OFFSET } from '../asset-image.ts';
import { parseElfSegments, type verifyDistribution } from './verify.ts';

export type Distribution = ReturnType<typeof verifyDistribution>;
export interface CommandResult {
  code: number;
  stdout: string;
  stderr: string;
}
export type Runner = (program: string, args: string[], timeoutMs: number) => Promise<CommandResult>;
export interface InstallOptions {
  programmer: string;
  probe: string;
  backupDir: string;
  replaceDashboard: boolean;
  forceDashboardRewrite?: boolean;
  resumeFrom?: string;
  runner?: Runner;
}
export interface InstallResult {
  backupDir: string;
  tag: string;
  commit: string;
  dashboardWritten: boolean;
  verification: 'program-readback-only';
}
function need(ok: unknown, message: string): asserts ok {
  if (!ok) throw new Error(message);
}
const sha = (b: Buffer) => createHash('sha256').update(b).digest('hex');
const hex = (n: number) => `0x${n.toString(16)}`;
const RAM = 0x24000000;
const MAIL = 0x24010000;
const DATA = 0x24012000;
const BLOCK = 262144;

/** Bounded direct execution: no shell, no inherited stdin, no background child on timeout. */
export const runCommand: Runner = (program, args, timeoutMs) =>
  new Promise((resolve, reject) => {
    const child = spawn(program, args, { shell: false, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '',
      stderr = '',
      size = 0;
    let failure: Error | undefined;
    const timer = setTimeout(() => {
      failure = new Error(`Command timed out after ${timeoutMs} ms: ${program}`);
      child.kill('SIGKILL');
    }, timeoutMs);
    const collect = (chunk: Buffer, error: boolean) => {
      size += chunk.length;
      if (size > 8 * 1024 * 1024) {
        failure = new Error('Command output exceeded 8 MiB');
        child.kill('SIGKILL');
      } else if (error) stderr += chunk.toString();
      else stdout += chunk.toString();
    };
    child.stdout.on('data', (chunk: Buffer) => collect(chunk, false));
    child.stderr.on('data', (chunk: Buffer) => collect(chunk, true));
    child.on('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (failure) reject(new Error(`${failure.message}\nstdout:\n${stdout}\nstderr:\n${stderr}`));
      else resolve({ code: code ?? -1, stdout, stderr });
    });
  });

/** Only these fixed operation shapes can reach CubeProgrammer. */
export function cubeArguments(probe: string, mode: 'UR' | 'HOTPLUG', operation: string[]) {
  need(/^[A-Za-z0-9]{8,64}$/.test(probe), 'Explicit alphanumeric probe serial required');
  const valid =
    (operation.length === 2 && operation[0] === '-ob' && operation[1] === 'displ') ||
    (operation.length === 1 && ['-rst', '-halt'].includes(operation[0])) ||
    (operation.length === 4 &&
      operation[0] === '-u' &&
      /^0x[0-9a-f]+$/.test(operation[1]) &&
      /^\d+$/.test(operation[2]) &&
      isAbsolute(operation[3])) ||
    (operation[0] === '-w' &&
      isAbsolute(operation[1] ?? '') &&
      operation[1].endsWith('.elf') &&
      operation[2] === '-v' &&
      (operation.length === 3 ||
        (operation.length === 5 && operation[3] === '-g' && operation[4] === '0x24000000') ||
        (operation.length === 9 &&
          operation[3] === '-u' &&
          operation[4] === '0x24012000' &&
          operation[5] === '4096' &&
          isAbsolute(operation[6]) &&
          operation[6].endsWith('.bin') &&
          operation[7] === '-g' &&
          operation[8] === '0x24000000')));
  need(valid, 'Disallowed CubeProgrammer operation');
  need(mode === 'UR' || operation[0] === '-u', 'HOTPLUG is read-only');
  return [
    '-c',
    'port=SWD',
    `sn=${probe}`,
    'freq=1000',
    `mode=${mode}`,
    ...(mode === 'UR' ? ['reset=HWrst'] : []),
    ...operation,
  ];
}

function stripAnsi(output: string) {
  return output
    .split(String.fromCharCode(27))
    .map((part) => part.replace(/^\[[0-9;]*m/, ''))
    .join('');
}

/** Preserve every displayed field; unknown layouts fail closed rather than guessing protection. */
export function parseOptions(output: string) {
  const clean = stripAnsi(output);
  const sections = clean.split(/^\s*OPTION BYTES BANK:\s*0\s*$/m);
  need(sections.length === 2, 'Expected exactly one Cube option-byte bank display');
  const expected = [
    'RDP',
    'BOR_LEV',
    'IWDG1_SW',
    'IWDG2_SW',
    'NRST_STOP_D1',
    'NRST_STBY_D1',
    'FZ_IWDG_STOP',
    'FZ_IWDG_SDBY',
    'BCM4',
    'BCM7',
    'NRST_STOP_D2',
    'NRST_STBY_D2',
    'SWAP_BANK',
    'IO_HSLV',
    'BOOT_CM7_ADD0',
    'BOOT_CM7_ADD1',
    'BOOT_CM4_ADD0',
    'BOOT_CM4_ADD1',
    'PROT_AREA_START1',
    'PROT_AREA_END1',
    'DMEP1',
    'PROT_AREA_START2',
    'PROT_AREA_END2',
    'DMEP2',
    'ST_RAM_SIZE',
    ...Array.from({ length: 16 }, (_, i) => `NWRP${i}`),
  ];
  const fields = new Map<string, number>();
  for (const line of sections[1].split(/\r?\n/)) {
    if (!line.trim()) continue;
    const match =
      /^\s*([A-Za-z][A-Za-z0-9_]*)\s*:\s*(0x[\da-f]+|\d+)\s*(?:\([^\r\n]*\)\s*)?$/i.exec(line);
    if (!match) {
      need(/^\s*[A-Za-z][A-Za-z -]*:\s*$/.test(line), `Unrecognized option display line: ${line}`);
      continue;
    }
    const key = match[1].toUpperCase();
    need(expected.includes(key) && !fields.has(key), `Unknown/duplicate option field ${key}`);
    fields.set(key, Number(match[2]));
  }
  need(
    fields.size === expected.length,
    'Incomplete Cube option-byte display: all 41 fields required',
  );
  const get = (name: string) => fields.get(name)!;
  need(
    get('BCM7') === 1 && get('BCM4') === 0 && get('SWAP_BANK') === 0 && get('RDP') === 0xaa,
    'Unsafe boot/RDP options; inspect manually, no automatic repair',
  );
  need(
    [0x0800, 0x08000000].includes(get('BOOT_CM7_ADD0')) &&
      [0x0810, 0x08100000].includes(get('BOOT_CM4_ADD0')),
    'Unexpected boot vector options',
  );
  for (let sector = 0; sector < 16; sector++)
    need(get(`NWRP${sector}`) === 1, `Sector ${sector} must be unprotected`);
  for (const bank of [1, 2]) {
    need(
      get(`PROT_AREA_START${bank}`) <= 0xff &&
        get(`PROT_AREA_END${bank}`) <= 0xff &&
        get(`PROT_AREA_START${bank}`) > get(`PROT_AREA_END${bank}`),
      `Bank ${bank} PCROP must be disabled`,
    );
  }
  return JSON.stringify([...fields].sort(([a], [b]) => a.localeCompare(b)));
}

/** ELF32 with authenticated CODE, fresh mailbox/results, and optional operation DATA. */
export function invocationElf(
  template: Buffer,
  op: number,
  offset: number,
  length: number,
  data?: Buffer,
) {
  const read =
    op === 1 && length > 0 && length <= BLOCK && offset >= 0 && offset + length <= FLASH_SIZE;
  const sector =
    op === 2 && offset >= OFFSET && offset < FLASH_SIZE && offset % 4096 === 0 && length === 4096;
  const header = op === 3 && offset === OFFSET && length === 32;
  need(
    Number.isInteger(offset) && Number.isInteger(length) && (read || sector || header),
    'Loader operation bounds',
  );
  need(read ? data === undefined : data?.length === length, 'Loader DATA length');
  if (sector && offset === OFFSET)
    need(
      data!.subarray(0, 32).every((b) => b === 255),
      'First sector header must be erased',
    );
  const code = parseElfSegments(template, 'loader');
  need(
    code.length === 1 &&
      code[0].address === RAM &&
      code[0].data.length >= 8 &&
      code[0].data.readUInt32LE(0) === 0x20020000 &&
      code[0].data.readUInt32LE(4) === template.readUInt32LE(24),
    'Loader vector/segment ABI',
  );
  const ph = template.readUInt32LE(28);
  for (let i = 0; i < template.readUInt16LE(44); i++) {
    const p = ph + i * 32;
    if (template.readUInt32LE(p) !== 1) continue;
    need(
      template.readUInt32LE(p + 8) === template.readUInt32LE(p + 12) &&
        template.readUInt32LE(p + 16) === template.readUInt32LE(p + 20),
      'Loader requires identical RAM addresses and no BSS',
    );
  }
  const mailbox = Buffer.alloc(96);
  [0x31494c47, 1, op, offset, length, data ? crc32(data) : 0].forEach((v, i) =>
    mailbox.writeUInt32LE(v, i * 4),
  );
  // Cube short-section staging is under investigation. Keep the logical request/CRC exact,
  // but use the same bounded RAM load size as the exercised sector path.
  const staging = data ? Buffer.alloc(4096, 255) : undefined;
  if (data) data.copy(staging!);
  const segments = [
    ...code,
    { address: MAIL, data: mailbox },
    ...(staging ? [{ address: DATA, data: staging }] : []),
  ];
  const align4 = (n: number) => Math.ceil(n / 4) * 4;
  const names = ['.code', '.mailbox', ...(data ? ['.data'] : []), '.shstrtab'];
  const strings = Buffer.from('\0' + names.join('\0') + '\0', 'ascii');
  let cursor = 52 + segments.length * 32;
  const offsets = segments.map((s) => {
    const start = align4(cursor);
    cursor = start + s.data.length;
    return start;
  });
  const stringOffset = align4(cursor);
  const sectionOffset = align4(stringOffset + strings.length);
  const sectionCount = segments.length + 2;
  const elf = Buffer.alloc(sectionOffset + sectionCount * 40);
  template.copy(elf, 0, 0, 52);
  elf.writeUInt32LE(52, 28);
  elf.writeUInt32LE(sectionOffset, 32);
  elf.writeUInt16LE(segments.length, 44);
  elf.writeUInt16LE(40, 46);
  elf.writeUInt16LE(sectionCount, 48);
  elf.writeUInt16LE(sectionCount - 1, 50);
  segments.forEach((s, i) => {
    [1, offsets[i], s.address, s.address, s.data.length, s.data.length, i === 0 ? 5 : 6, 4].forEach(
      (v, j) => elf.writeUInt32LE(v, 52 + i * 32 + j * 4),
    );
    // Padding aligns the next file region; it is not part of any loaded segment.
    s.data.copy(elf, offsets[i]);
    [
      strings.indexOf(names[i] + '\0'),
      1,
      i === 0 ? 6 : 3,
      s.address,
      offsets[i],
      s.data.length,
      0,
      0,
      4,
      0,
    ].forEach((v, j) => elf.writeUInt32LE(v, sectionOffset + (i + 1) * 40 + j * 4));
  });
  strings.copy(elf, stringOffset);
  [strings.indexOf('.shstrtab\0'), 3, 0, 0, stringOffset, strings.length, 0, 0, 1, 0].forEach(
    (v, j) => elf.writeUInt32LE(v, sectionOffset + (sectionCount - 1) * 40 + j * 4),
  );
  return elf;
}

/** Pure guard: resume never repairs arbitrary damaged or partially staged contents. */
export function checkResume(
  original: Buffer,
  current: Buffer,
  image: Buffer,
): 'staged' | 'committed' {
  need(
    original.length === FLASH_SIZE && current.length === FLASH_SIZE,
    'Resume requires full-chip backups',
  );
  need(
    image.length >= 4096 && image.length <= FLASH_SIZE - OFFSET && image.length % 4096 === 0,
    'Resume image bounds',
  );
  need(
    current.subarray(0, OFFSET).equals(original.subarray(0, OFFSET)) &&
      current.subarray(OFFSET + image.length).equals(original.subarray(OFFSET + image.length)),
    'Resume rejected: bytes outside the approved image changed',
  );
  const range = current.subarray(OFFSET, OFFSET + image.length);
  if (range.equals(image)) return 'committed';
  const staged = Buffer.from(image);
  staged.fill(255, 0, 32);
  need(range.equals(staged), 'Resume rejected: chip is neither exact staged nor committed image');
  return 'staged';
}

async function resumeEvidence(
  directory: string,
  distribution: Distribution,
  probe: string,
  image: Buffer,
) {
  need(isAbsolute(directory), 'Absolute resume directory required');
  const read = (name: string) => readFile(join(directory, name));
  const first = await read('external-before-1.bin');
  const second = await read('external-before-2.bin');
  for (const [index, bytes] of [first, second].entries()) {
    const sidecar = (await read(`external-before-${index + 1}.sha256`)).toString('ascii');
    need(
      /^[a-f0-9]{64}\n?$/.test(sidecar) && sidecar.trim() === sha(bytes),
      'Resume backup SHA-256 mismatch',
    );
  }
  need(first.equals(second), 'Resume independent backups differ');
  checkBackup(first, sha(first), image, true);
  const identity = JSON.parse((await read('distribution.json')).toString('utf8')) as Record<
    string,
    unknown
  >;
  need(
    identity.tag === distribution.tag &&
      identity.commit === distribution.commit &&
      (identity.imageSha256 === undefined || identity.imageSha256 === sha(image)),
    'Resume distribution identity mismatch',
  );
  for (const core of ['m4', 'm7'])
    need(
      (await read(`${core}.elf`)).equals(distribution.files.get(`${core}.elf`)!),
      'Resume firmware differs from verified distribution',
    );
  const options = parseOptions((await read('options-before.txt')).toString('utf8'));
  const rows = (await read('install.jsonl')).toString('utf8');
  need(rows.endsWith('\n'), 'Truncated resume log');
  let commands = 0;
  for (const line of rows.trimEnd().split('\n')) {
    const row = JSON.parse(line) as { time?: unknown; message?: unknown };
    need(
      typeof row.time === 'string' && typeof row.message === 'string',
      'Malformed resume log row',
    );
    if (!row.message.startsWith('Cube ')) continue;
    const args: unknown = JSON.parse(row.message.slice(5));
    need(
      Array.isArray(args) && args.every((v) => typeof v === 'string') && args[2] === `sn=${probe}`,
      'Resume log probe mismatch',
    );
    const mode = args[4] === 'mode=UR' ? 'UR' : 'HOTPLUG';
    need(
      JSON.stringify(cubeArguments(probe, mode, args.slice(mode === 'UR' ? 6 : 5))) ===
        JSON.stringify(args),
      'Unrecognized resume command',
    );
    commands++;
  }
  need(commands > 0, 'Resume log has no probe evidence');
  return { original: first, options };
}

/** Input must be the authenticated result of verifyDistribution, never an unpacked archive. */
export async function installDistribution(
  distribution: Distribution,
  options: InstallOptions,
): Promise<InstallResult> {
  const files = new Map([...distribution.files].map(([k, v]) => [k, Buffer.from(v)]));
  const file = (name: string) => {
    const value = files.get(name);
    need(value?.length, `Missing verified installation asset: ${name}`);
    return value;
  };
  for (const name of [
    'qspi-loader.elf',
    'qspi-loader.json',
    'installer-runtime.mjs',
    'installer-manifest.json',
    'm4.elf',
    'm7.elf',
  ])
    file(name);
  const template = file('qspi-loader.elf');
  invocationElf(template, 1, 0, 1);
  const cores = (['m4', 'm7'] as const).map((core) => ({
    core,
    segments: parseElfSegments(file(`${core}.elf`), core),
  }));
  const image = file('dashboard.qspi.bin');
  need(assetImage(file('index.html.gz')).equals(image), 'Dashboard differs from verified gzip');
  need(
    isAbsolute(options.programmer) && isAbsolute(options.backupDir),
    'Absolute programmer and backup paths required',
  );
  cubeArguments(options.probe, 'UR', ['-ob', 'displ']);
  need(
    !options.forceDashboardRewrite || options.replaceDashboard,
    'Forced rewrite requires explicit replacement consent',
  );
  need(
    !options.resumeFrom || options.replaceDashboard,
    'Resume requires explicit dashboard replacement consent',
  );
  need(
    !options.resumeFrom || !options.forceDashboardRewrite,
    'Resume is header-only; do not combine with forced rewrite',
  );
  // Validate trusted local evidence before invoking even the programmer help command.
  const resume = options.resumeFrom
    ? await resumeEvidence(options.resumeFrom, distribution, options.probe, image)
    : undefined;
  const runner = options.runner ?? runCommand;
  await mkdir(options.backupDir, { recursive: true, mode: 0o700 });
  const directory = await mkdtemp(join(options.backupDir, 'giga-install-'));
  const log = (message: string) =>
    appendFile(
      join(directory, 'install.jsonl'),
      JSON.stringify({ time: new Date().toISOString(), message }) + '\n',
    );
  const save = async (name: string, bytes: Buffer | string) => {
    const path = join(directory, name);
    await writeFile(path, bytes, { flag: 'wx', mode: 0o600 });
    return path;
  };
  const lockPath = join(tmpdir(), `giga-usb-probe-${options.probe}.lock`);
  const lock = await open(lockPath, 'wx', 0o600).catch(() => {
    throw new Error(
      `Probe installer lock exists: ${lockPath}. Inspect owner before removing; evidence: ${directory}`,
    );
  });
  let touched = false,
    mutationStarted = false,
    internalStarted = false,
    loaderStarted = false,
    serial = 0;
  let lastStaging:
    { path: string; logicalLength: number; operation: number; offset: number } | undefined;
  const recoveryLog = async (message: string) => {
    try {
      await log(message);
    } catch {
      /* Evidence failure must not prevent halt or mask the original failure. */
    }
  };
  const cube = async (mode: 'UR' | 'HOTPLUG', args: string[], timeout = 60000) => {
    const command = cubeArguments(options.probe, mode, args);
    await log(`Cube ${JSON.stringify(command)}`);
    const result = await runner(options.programmer, command, timeout);
    await log(JSON.stringify(result));
    need(
      result.code === 0 && !/\bError\s*:/i.test(result.stdout + result.stderr),
      `Cube command failed: ${result.stderr || result.stdout}`,
    );
    return result.stdout;
  };
  const upload = async (address: number, length: number, name: string, timeout = 60000) => {
    const path = join(directory, `${++serial}-${name}.bin`);
    await cube('HOTPLUG', ['-u', hex(address), String(length), path], timeout);
    const bytes = await readFile(path);
    need(bytes.length === length, `Truncated upload ${name}`);
    return bytes;
  };
  let identitySaved = false;
  const operation = async (op: number, offset: number, length: number, data?: Buffer) => {
    const path = await save(
      `${++serial}-operation.elf`,
      invocationElf(template, op, offset, length, data),
    );
    await log(`Loader issue op=${op} offset=${hex(offset)} length=${length}`);
    // Mark before dispatch: a failed/timed-out command may already have issued WREN.
    if (op !== 1) mutationStarted = true;
    loaderStarted = true;
    const stagingPath = join(directory, `${serial}-staged-input.bin`);
    if (data) {
      lastStaging = { path: stagingPath, logicalLength: length, operation: op, offset };
      await save(
        `${serial}-staging.json`,
        JSON.stringify({
          operation: op,
          offset,
          logicalLength: length,
          stagingLength: 4096,
          inputCrc32: crc32(data),
          inputSha256: sha(data),
          elfSha256: sha(await readFile(path)),
          stagingPath,
        }),
      );
    }
    // Cube documents multiple sequential commands. This upload records RAM before -g in
    // the same connection; the host compares it after return, not as a pre-WREN interlock.
    await cube('UR', [
      '-w',
      path,
      '-v',
      ...(data ? ['-u', hex(DATA), '4096', stagingPath] : []),
      '-g',
      hex(RAM),
    ]);
    if (data) {
      const stagedInput = await readFile(stagingPath);
      const expected = Buffer.alloc(4096, 255);
      data.copy(expected);
      need(stagedInput.equals(expected), 'Pre-execution RAM staging differs; no automatic retry');
      await log(`Pre-execution RAM staging matched ${sha(stagedInput)}`);
    }
    const deadline = Date.now() + 180000;
    let result: Buffer;
    for (;;) {
      need(
        Date.now() < deadline,
        'Loader timeout: do not blindly retry a write; reconcile flash from retained evidence',
      );
      result = await upload(MAIL + 32, 64, 'result');
      const magic = result.readUInt32LE(0),
        state = result.readUInt32LE(4);
      await log(`Loader result ${result.toString('hex')}`);
      // Entry publishes magic before running; either zeroed initialization snapshot is fresh.
      if (
        (magic === 0 || magic === 0x31524c47) &&
        state === 0 &&
        result.subarray(4).every((b) => b === 0)
      ) {
        await sleep(5000);
        continue;
      }
      need(magic === 0x31524c47 && [1, 2, 65535].includes(state), 'Invalid loader result');
      need(
        result.readUInt32LE(12) <= 4 &&
          result.readUInt32LE(16) <= length &&
          result.subarray(44).every((b) => b === 0),
        'Invalid loader phase/progress/reserved fields',
      );
      need(
        state !== 65535 && result.readUInt32LE(8) === 0,
        `Loader failed${result.readUInt32LE(8) === 15 ? ' environment gate (error 15): reset clock/M4/cache/MPU/DWT preconditions rejected' : ''}: ${result.toString('hex')}`,
      );
      if (state === 2) break;
      await sleep(5000);
    }
    need(
      result.readUInt32LE(12) === 4 &&
        result.readUInt32LE(16) === length &&
        result.readUInt32LE(40) === length &&
        result.readUInt32LE(20) === 0x01891f &&
        [24, 28, 32].every((offset) => result.readUInt32LE(offset) <= 0xff) &&
        (result.readUInt32LE(24) & 0x7f) === 0 &&
        (result.readUInt32LE(28) & 0xc4) === 0,
      'Loader terminal count/identity/status mismatch',
    );
    if (!identitySaved) {
      const sfdp = await upload(MAIL + 256, 256, 'sfdp');
      need(
        sfdp.readUInt32LE(0) === 0x50444653 &&
          sfdp.readUInt32LE(0x34) === 0x07ffffff &&
          sfdp.readUInt32LE(0x4c) === 0x520f200c,
        'Unexpected SFDP',
      );
      await save(
        'identity.json',
        JSON.stringify({
          jedec: '1f8901',
          result: result.toString('hex'),
          sfdp: sfdp.toString('hex'),
        }),
      );
      identitySaved = true;
    }
    const bytes = await upload(DATA, length, 'data');
    need(crc32(bytes) === result.readUInt32LE(36), 'Fresh DATA CRC mismatch');
    if (data) need(bytes.equals(data), 'Fresh programmed bytes differ');
    return bytes;
  };
  const readRange = async (offset: number, length: number) => {
    const out = Buffer.alloc(length);
    for (let n = 0; n < length; n += BLOCK)
      (await operation(1, offset + n, Math.min(BLOCK, length - n))).copy(out, n);
    return out;
  };
  try {
    await lock.writeFile(JSON.stringify({ pid: process.pid, directory, probe: options.probe }));
    const processes = await runner('/bin/ps', ['-axo', 'pid=,comm='], 10000);
    need(processes.code === 0, 'Cannot inspect probe owners');
    const owners = processes.stdout.split('\n').filter((line) => {
      const match = /^\s*(\d+)\s+(.+)$/.exec(line);
      return (
        match &&
        Number(match[1]) !== process.pid &&
        /(?:probe-rs|cargo-embed|openocd|jlink|stm32.*programmer)/i.test(match[2])
      );
    });
    need(
      !owners.length,
      `Release existing probe owner(s), then retry; no processes were killed:\n${owners.join('\n')}`,
    );
    const help = await runner(options.programmer, ['--help'], 15000);
    await save('programmer-help.txt', help.stdout + help.stderr);
    need(
      help.code === 0 &&
        /(?:^|\n)\s*-halt\s+:\s*Halt core\b/i.test(stripAnsi(help.stdout + '\n' + help.stderr)),
      'CubeProgrammer help must confirm -halt support before board access',
    );
    for (const { core } of cores) await save(`${core}.elf`, file(`${core}.elf`));
    await save(
      'distribution.json',
      JSON.stringify({
        tag: distribution.tag,
        commit: distribution.commit,
        imageSha256: sha(image),
        probe: options.probe,
        resumeFrom: options.resumeFrom ?? null,
      }),
    );
    touched = true;
    const beforeText = await cube('UR', ['-ob', 'displ']);
    await save('options-before.txt', beforeText);
    const before = parseOptions(beforeText);
    if (resume) need(before === resume.options, 'Resume option fields differ from original backup');
    const internal = join(directory, 'internal-before.bin');
    await cube('UR', ['-u', '0x8000000', String(0x200000), internal]);
    const internalBackup = await readFile(internal);
    need(internalBackup.length === 0x200000, 'Incomplete internal backup');
    await save('internal-before.sha256', sha(internalBackup) + '\n');
    const backup = await readRange(0, FLASH_SIZE);
    await save('external-before-1.bin', backup);
    await save('external-before-1.sha256', sha(backup) + '\n');
    const second = await readRange(0, FLASH_SIZE);
    await save('external-before-2.bin', second);
    await save('external-before-2.sha256', sha(second) + '\n');
    need(backup.equals(second), 'Independent external backups differ');
    const resumeState = resume ? checkResume(resume.original, backup, image) : undefined;
    if (resume)
      await save(
        'resume.json',
        JSON.stringify({
          source: options.resumeFrom,
          state: resumeState,
          originalSha256: sha(resume.original),
        }),
      );
    else checkBackup(backup, sha(backup), image, options.replaceDashboard);
    const identical = backup.subarray(OFFSET, OFFSET + image.length).equals(image);
    if (options.forceDashboardRewrite)
      need(
        backup.subarray(OFFSET, OFFSET + 8).toString('ascii') === 'GIGAUI01',
        'Forced hardware rewrite requires an existing valid dashboard and explicit replacement consent',
      );
    const written = !identical || options.forceDashboardRewrite === true;
    if (written) {
      const staged = Buffer.from(image);
      staged.fill(255, 0, 32);
      for (let n = 0; !resume && n < staged.length; n += 4096) {
        const old = await operation(1, OFFSET + n, 4096);
        need(
          old.equals(backup.subarray(OFFSET + n, OFFSET + n + 4096)),
          'Sector changed since approved backup; refusing write',
        );
        await operation(2, OFFSET + n, 4096, staged.subarray(n, n + 4096));
      }
      const fresh = await readRange(OFFSET, staged.length);
      need(
        fresh.equals(staged) &&
          crc32(fresh.subarray(32, 32 + image.readUInt32LE(8))) === image.readUInt32LE(12),
        'Staged payload verification failed; header remains invalid',
      );
      await operation(3, OFFSET, 32, image.subarray(0, 32));
    }
    const final = await readRange(0, FLASH_SIZE);
    await save('external-after.bin', final);
    await save('external-after.sha256', sha(final) + '\n');
    const preservation = resume?.original ?? backup;
    need(
      final.subarray(OFFSET, OFFSET + image.length).equals(image) &&
        final.subarray(0, OFFSET).equals(preservation.subarray(0, OFFSET)) &&
        final.subarray(OFFSET + image.length).equals(preservation.subarray(OFFSET + image.length)),
      'Full external preservation comparison failed',
    );
    for (const { core } of cores) {
      const path = join(directory, `${core}.elf`);
      internalStarted = true;
      mutationStarted = true;
      await cube('UR', ['-w', path, '-v']);
    }
    const programmedPath = join(directory, 'internal-after.bin');
    await cube('UR', ['-u', '0x8000000', String(0x200000), programmedPath]);
    const programmed = await readFile(programmedPath);
    need(programmed.length === 0x200000, 'Incomplete internal readback');
    for (const { segments } of cores)
      for (const segment of segments) {
        const n = segment.address - 0x08000000;
        need(
          programmed.subarray(n, n + segment.data.length).equals(segment.data),
          'Internal PT_LOAD readback mismatch',
        );
      }
    const afterText = await cube('UR', ['-ob', 'displ']);
    await save('options-after.txt', afterText);
    need(parseOptions(afterText) === before, 'Option fields changed; manual recovery required');
    await cube('UR', ['-rst']);
    const result: InstallResult = {
      backupDir: directory,
      tag: distribution.tag,
      commit: distribution.commit,
      dashboardWritten: written,
      verification: 'program-readback-only',
    };
    await save('result.json', JSON.stringify(result, null, 2));
    await log('PASS: programming/readback only; hardware health and HTTP/USB are not validated');
    return result;
  } catch (error) {
    const guidance = mutationStarted
      ? `Possible ${internalStarted ? 'internal/external' : 'external'} partial programming. Do not run or power-cycle the board or blindly retry writes. Reconcile retained backups/readbacks; recover matching verified M4 then M7 images before permitting application execution.`
      : 'No persistent write was attempted. Inspect the failure before manually resuming.';
    await recoveryLog(`FAIL: ${String(error)}; ${guidance}`);
    if (lastStaging) {
      try {
        const bytes = await readFile(lastStaging.path);
        await save(
          'failure-staging.json',
          JSON.stringify({
            ...lastStaging,
            observedLength: bytes.length,
            sha256: sha(bytes),
            logicalCrc32: crc32(bytes.subarray(0, lastStaging.logicalLength)),
          }),
        );
        await recoveryLog(
          `Last staging upload retained: ${lastStaging.path} (${bytes.length} bytes)`,
        );
      } catch (stagingError) {
        await recoveryLog(
          `Last staging upload unavailable/incomplete: ${lastStaging.path}: ${String(stagingError)}`,
        );
      }
    }
    if (touched && loaderStarted) {
      try {
        const environment = await upload(MAIL + 512, 32, 'failure-environment', 15000);
        await recoveryLog(
          `Loader environment snapshot (may be incomplete): ${environment.toString('hex')}`,
        );
      } catch (snapshotError) {
        await recoveryLog(`Environment snapshot unavailable: ${String(snapshotError)}`);
      }
    }
    let halt = 'not attempted: no board access';
    if (touched) {
      try {
        // No -rst/-run/-g here: UR reconnects under hardware reset, then explicitly halts M7.
        // This is a best-effort command result, not proof the board remains halted after disconnect.
        const command = cubeArguments(options.probe, 'UR', ['-halt']);
        await recoveryLog(`Recovery halt ${JSON.stringify(command)}`);
        const result = await runner(options.programmer, command, 15000);
        await recoveryLog(JSON.stringify(result));
        need(
          result.code === 0 && !/\bError\s*:/i.test(result.stdout + result.stderr),
          'Cube halt failed',
        );
        halt = 'UR -halt command accepted; persistent halted state is hardware-unverified';
      } catch (haltError) {
        halt = `HALT FAILED; board execution state UNKNOWN: ${String(haltError)}`;
      }
    }
    await recoveryLog(`${halt}; ${guidance}`);
    try {
      await save(
        'recovery.json',
        JSON.stringify(
          {
            state: 'failed',
            error: String(error),
            mutationStarted,
            internalStarted,
            halt,
            guidance,
          },
          null,
          2,
        ),
      );
    } catch (evidenceError) {
      await recoveryLog(`Recovery evidence write failed: ${String(evidenceError)}`);
    }
    throw new Error(
      `Installation failed; evidence retained in ${directory}: ${String(error)}; ${halt}; ${guidance}`,
      {
        cause: error,
      },
    );
  } finally {
    try {
      await lock.close();
    } catch (error) {
      await recoveryLog(`Lock close failed: ${String(error)}`);
    }
    try {
      await unlink(lockPath);
    } catch (error) {
      await recoveryLog(`Lock removal failed; inspect before retry: ${String(error)}`);
    }
  }
}
