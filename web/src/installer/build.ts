import { build } from 'esbuild';
import { createHash } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { installerSource } from './cli.ts';
import { parseElfSegments, verifyDistribution } from './verify.ts';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const sha256 = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
function json(path: string, value: unknown) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
}

export async function buildInstallerRuntime(output: string) {
  await build({
    entryPoints: [join(root, 'web/src/installer/cli.ts')],
    bundle: true,
    platform: 'node',
    format: 'esm',
    target: 'node24',
    outfile: output,
    legalComments: 'none',
    define: { 'import.meta.main': 'false' },
  });
}

export function buildLoader(directory: string) {
  mkdirSync(directory, { recursive: true });
  const elf = join(directory, 'qspi-loader.elf');
  const result = spawnSync(
    'rustc',
    [
      '--edition',
      '2024',
      '--target',
      'thumbv7em-none-eabihf',
      '-D',
      'warnings',
      '-C',
      'panic=abort',
      '-C',
      'opt-level=s',
      '-C',
      'relocation-model=static',
      '-C',
      `link-arg=-T${join(root, 'firmware/m7/installer/link.x')}`,
      join(root, 'firmware/m7/installer/loader.rs'),
      '-o',
      elf,
    ],
    { shell: false, stdio: 'inherit', timeout: 120000 },
  );
  if (result.error || result.status !== 0) throw result.error ?? new Error('Loader build failed');
  const bytes = readFileSync(elf);
  parseElfSegments(bytes, 'loader');
  json(join(directory, 'qspi-loader.json'), {
    format: 'GIGAUSB-QSPI-LOADER-1',
    file: 'qspi-loader.elf',
    sha256: sha256(bytes),
    jedec: '1f8901',
    loadAddress: 0x24000000,
    limit: 0x24010000,
  });
}

export function writeRuntimeManifest(directory: string) {
  const identity = readFileSync(join(directory, 'BUILD.txt'), 'utf8');
  const tags = [...identity.matchAll(/^Tag: (.+)$/gm)];
  const commits = [...identity.matchAll(/^Commit: ([a-f0-9]{40})$/gm)];
  if (tags.length !== 1 || commits.length !== 1) throw new Error('Invalid build identity');
  json(join(directory, 'installer-manifest.json'), {
    format: 'GIGAUSB-INSTALLER-1',
    file: 'installer-runtime.mjs',
    sha256: sha256(readFileSync(join(directory, 'installer-runtime.mjs'))),
    tag: tags[0][1],
    commit: commits[0][1],
  });
}

export function packageInstaller(signed: string, runtime: string, key: string, output: string) {
  const image = readFileSync(signed);
  const distribution = verifyDistribution(image, readFileSync(key));
  const code = readFileSync(runtime, 'utf8');
  if (!distribution.files.get('installer-runtime.mjs')?.equals(Buffer.from(code)))
    throw new Error('Runtime is not the authenticated release runtime');
  if (!distribution.files.has('qspi-loader.elf'))
    throw new Error('Missing authenticated RAM loader');
  writeFileSync(output, installerSource(code, image), { flag: 'wx', mode: 0o755 });
}

if (import.meta.main) {
  const [command, ...args] = process.argv.slice(2);
  if (command === 'runtime' && args.length === 1) await buildInstallerRuntime(resolve(args[0]));
  else if (command === 'loader' && args.length === 1) buildLoader(resolve(args[0]));
  else if (command === 'manifest' && args.length === 1) writeRuntimeManifest(resolve(args[0]));
  else if (command === 'package' && args.length === 4)
    packageInstaller(...(args.map((path) => resolve(path)) as [string, string, string, string]));
  else
    throw new Error(
      'Use runtime <output>, loader <directory>, manifest <directory>, or package <signed.bin> <runtime.mjs> <trusted-public.pem> <output.mjs>',
    );
}
