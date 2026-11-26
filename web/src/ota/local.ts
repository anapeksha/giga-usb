import { spawnSync } from 'node:child_process';
import { createHash, generateKeyPairSync, sign, type KeyObject } from 'node:crypto';
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { bootstrapHex, HEX_WARNING } from './hex.ts';
import { parseElfSegments } from '../installer/verify.ts';
import { buildActive, createBootstrapFiles } from './build.ts';
import { buildOta, verifyOta } from './format.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const hash = (bytes: Buffer) => createHash('sha256').update(bytes).digest();
export const USAGE = `TEST ONLY — NOT PRODUCTION. Node 24 local artifact builder; no hardware access.
  node web/src/ota/local.ts --output /absolute/path/to/target/new-directory [--defmt]
Output must not exist and must be inside this worktree's target directory.
Generates an ephemeral TEST-ONLY private key (0600): never publish it.
Builds bootstrap-test-a and candidate-test-b from the current worktree.
Initial SWD migration is NOT power-fail-safe and remains hardware-unvalidated.
${HEX_WARNING}`;

export function parseOptions(args: string[]) {
  const defmt = args.includes('--defmt');
  const rest = args.filter((arg) => arg !== '--defmt');
  if (
    args.filter((arg) => arg === '--defmt').length > 1 ||
    rest.length !== 2 ||
    rest[0] !== '--output' ||
    !isAbsolute(rest[1])
  )
    throw new Error(USAGE);
  const output = resolve(rest[1]);
  if (!output.startsWith(join(root, 'target') + '/'))
    throw new Error('Output must be a new directory inside the worktree target directory');
  return { output, defmt };
}

/** Non-bootable distribution envelope, matching the installer's strict MCUboot profile. */
export function signDistribution(payload: Buffer, privateKey: KeyObject, publicKey: KeyObject) {
  const header = Buffer.alloc(512, 0xff);
  header.fill(0, 0, 32);
  header.writeUInt32LE(0x96f3b83d);
  header.writeUInt16LE(512, 8);
  header.writeUInt32LE(payload.length, 12);
  header.writeUInt32LE(0x10, 16);
  const body = Buffer.concat([header, payload]);
  const digest = hash(body);
  const tlvs = [
    [1, hash(publicKey.export({ format: 'der', type: 'spki' }))],
    [0x10, digest],
    [0x24, sign(null, digest, privateKey)],
  ] as const;
  const info = Buffer.alloc(4);
  info.writeUInt16LE(0x6907);
  info.writeUInt16LE(144, 2);
  return Buffer.concat([
    body,
    info,
    ...tlvs.flatMap(([kind, bytes]) => {
      const entry = Buffer.alloc(4);
      entry[0] = kind;
      entry.writeUInt16LE(bytes.length, 2);
      return [entry, bytes];
    }),
  ]);
}

function run(command: string, args: string[], cwd = root, env = process.env) {
  const result = spawnSync(command, args, {
    cwd,
    env,
    shell: false,
    encoding: 'utf8',
    timeout: 600000,
    maxBuffer: 16 * 1024 * 1024,
  });
  if (result.stdout) process.stdout.write(result.stdout);
  if (result.stderr) process.stderr.write(result.stderr);
  if (result.error || result.status !== 0)
    throw new Error(
      `${command} ${args.join(' ')} failed: ${result.error?.message ?? result.status}`,
    );
  return result.stdout.trim();
}

export async function main(args: string[]) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) {
    console.log(USAGE);
    return;
  }
  const { output, defmt } = parseOptions(args);
  mkdirSync(output, { mode: 0o700 });
  console.warn('NOT PRODUCTION: generating a new ephemeral TEST-ONLY signing trust anchor.');
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const privatePem = privateKey.export({ format: 'pem', type: 'pkcs8' });
  const publicPem = publicKey.export({ format: 'pem', type: 'spki' });
  const raw = publicKey.export({ format: 'der', type: 'spki' }).subarray(-32);
  const put = (directory: string, name: string, bytes: string | Buffer) =>
    writeFileSync(join(directory, name), bytes, { flag: 'wx', mode: 0o600 });
  put(output, 'TEST-ONLY-private.pem', privatePem);
  put(output, 'TEST-ONLY-public.pem', publicPem);
  const commit = run('git', ['--no-pager', 'rev-parse', 'HEAD']);
  const dirty = run('git', ['--no-optional-locks', 'status', '--porcelain']).length > 0;
  run('npm', ['run', 'build'], join(root, 'web'));
  const pkg = join(output, 'bootstrap-package');
  mkdirSync(pkg);
  const cargo = (core: string, id: string) =>
    run(
      'cargo',
      [
        'build',
        '-p',
        core,
        '--release',
        '--no-default-features',
        '--locked',
        ...(defmt ? ['--features', 'defmt'] : []),
      ],
      root,
      {
        ...process.env,
        CARGO_TARGET_DIR: join(root, 'target'),
        GIGAUSB_OTA_PUBLIC_KEY_HEX: raw.toString('hex'),
        GIGAUSB_OTA_BUILD_ID: id,
      },
    );
  const elf = (name: string) =>
    readFileSync(join(root, 'target/thumbv7em-none-eabihf/release', name));
  cargo('m4', 'bootstrap-test-a');
  const m4 = elf('m4');
  cargo('m7', 'bootstrap-test-a');
  const boot = elf('bootloader');
  const m7 = elf('m7');
  const loaded = (bytes: Buffer, core: 'm7-ota' | 'bootloader') =>
    Buffer.concat(parseElfSegments(bytes, core).map((segment) => segment.data));
  if (!loaded(boot, 'bootloader').includes(raw) || !loaded(m7, 'm7-ota').includes(raw))
    throw new Error('Compiled bootloader/application does not retain the TEST-ONLY public key');
  if (!loaded(m7, 'm7-ota').includes(Buffer.from('bootstrap-test-a')))
    throw new Error('Bootstrap compiled build ID missing from file-backed LOAD bytes');
  const dashboard = readFileSync(join(root, 'dist/dashboard.qspi.bin'));
  const files = createBootstrapFiles(boot, m7, m4, dashboard, privatePem, 'bootstrap-test-a');
  for (const [name, bytes] of files) put(pkg, name, bytes);
  for (const name of ['index.html.gz', 'dashboard.qspi.bin', 'dashboard.qspi.bin.json'])
    put(pkg, name, readFileSync(join(root, 'dist', name)));
  for (const name of ['README.md', 'AGENTS.md', 'LICENSE', 'USB-IDS-LICENSE.txt', 'CHANGELOG.md'])
    put(pkg, name, readFileSync(join(root, name)));
  const identity = `NOT PRODUCTION: ephemeral TEST-ONLY key; never publish private PEM.\nTag: local-ota-bootstrap-test-a\nCommit: ${commit}\nWorktree: ${dirty ? 'dirty' : 'clean'}\nProfile: release\nFeatures: ${defmt ? 'defmt' : 'none'}\nLayout: OTA-v1\nGIGAUSB_OTA_PUBLIC_KEY_HEX: ${raw.toString('hex')}\nGIGAUSB_OTA_BUILD_ID: bootstrap-test-a\nCandidate build ID: candidate-test-b\nTrust fingerprint (SHA256 SPKI DER): ${hash(publicKey.export({ format: 'der', type: 'spki' })).toString('hex')}\n`;
  put(pkg, 'BUILD.txt', identity);
  put(output, 'BUILD.txt', identity);
  cargo('m7', 'candidate-test-b');
  const candidateM7 = elf('m7');
  const candidateBoot = elf('bootloader');
  if (
    !loaded(candidateM7, 'm7-ota').includes(Buffer.from('candidate-test-b')) ||
    !loaded(candidateM7, 'm7-ota').includes(raw) ||
    !loaded(candidateBoot, 'bootloader').includes(raw)
  )
    throw new Error('Candidate compiled key/build provenance mismatch');
  const candidateDir = join(output, 'candidate-test-b');
  mkdirSync(candidateDir);
  for (const [name, bytes] of [
    ['m7.elf', candidateM7],
    ['m4.elf', m4],
    ['bootloader.elf', candidateBoot],
  ] as const)
    put(candidateDir, name, bytes);
  const candidate = buildOta(
    buildActive(candidateM7, m4),
    dashboard,
    privatePem,
    'candidate-test-b',
  );
  const bootstrap = verifyOta(
    Buffer.concat([files.get('ota-manifest.bin')!, files.get('ota-active.bin')!, dashboard]),
    publicPem,
  );
  const verified = verifyOta(candidate, publicPem);
  if (Buffer.from(bootstrap.activeHash).equals(verified.activeHash))
    throw new Error('A/B ACTIVE hashes must differ');
  put(output, 'giga-usb-candidate-test-b.ota.bin', candidate);
  const hex = join(output, 'giga-usb-ota-bootstrap-test.hex');
  put(output, 'giga-usb-ota-bootstrap-test.hex', bootstrapHex(files, dashboard, publicPem));
  put(output, 'BOOTSTRAP-WARNING.txt', HEX_WARNING + '\n');
  const names = readdirSync(pkg).sort();
  put(
    pkg,
    'SHA256SUMS',
    names
      .map((name) => `${hash(readFileSync(join(pkg, name))).toString('hex')}  ${name}\n`)
      .join(''),
  );
  const archive = join(output, 'giga-usb-ota-bootstrap-test.tar.gz');
  run('/usr/bin/tar', ['--format=ustar', '-czf', archive, ...names, 'SHA256SUMS'], pkg, {
    ...process.env,
    COPYFILE_DISABLE: '1',
  });

  put(
    output,
    'giga-usb-ota-bootstrap-test.signed.bin',
    signDistribution(readFileSync(archive), privateKey, publicKey),
  );

  console.log(
    `TEST ONLY artifacts verified offline: ${output}\nBootstrap: ${hex}\nCandidate: ${join(output, 'giga-usb-candidate-test-b.ota.bin')}\nNo hardware accessed. Do not flash ELFs or the signed container directly.\nInitial migration is NOT power-fail-safe; retain SWD recovery. Hardware OTA behavior is NOT VALIDATED.\n${HEX_WARNING}\nNever publish TEST-ONLY-private.pem. Retained on failure for diagnosis; existing outputs are never removed.`,
  );
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2)).catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
