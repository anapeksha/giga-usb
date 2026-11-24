import { generateKeyPairSync } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { afterEach, expect, test } from 'vitest';
import { authenticatedRuntime, installerSource, main, payloadMarker } from './cli';
import { buildInstallerRuntime } from './build';

const paths: string[] = [];
afterEach(() => paths.splice(0).forEach((p) => rmSync(p, { recursive: true, force: true })));
function temporary() {
  const path = mkdtempSync(join(tmpdir(), 'giga-installer-cli-'));
  paths.push(path);
  return path;
}

test('embedding round-trips exactly and rejects ambiguous runtime payloads', () => {
  const template = `const image = ${JSON.stringify(payloadMarker)};\n`;
  const image = Buffer.from('signed distribution');
  const source = installerSource(template, image);
  expect(authenticatedRuntime(source, image.toString('base64'))).toBe(template);
  expect(() => installerSource('', image)).toThrow('one installer payload marker');
  expect(() => installerSource(template + template, image)).toThrow('one installer payload marker');
  expect(() => authenticatedRuntime(source + source, image.toString('base64'))).toThrow(
    'placement',
  );
});

test('CLI requires a public trust root and rejects malformed data before board access', async () => {
  await expect(main([], '', payloadMarker)).rejects.toThrow('--key');
  const directory = temporary();
  const privatePath = join(directory, 'private.pem');
  writeFileSync(privatePath, '-----BEGIN PRIVATE KEY-----\nnot a public key');
  await expect(main(['--key', privatePath, '--install'], '', 'AAAA')).rejects.toThrow('public PEM');
  const { publicKey } = generateKeyPairSync('ed25519');
  const key = join(directory, 'public.pem');
  writeFileSync(key, publicKey.export({ type: 'spki', format: 'pem' }));
  for (const bad of [payloadMarker, 'AA!', 'A==='])
    await expect(main(['--key', key, '--install'], '', bad)).rejects.toThrow(
      'embedded signed image',
    );
  await expect(main(['--key', key, '--install'], '', 'AAAA')).rejects.toThrow(
    'Signed image bounds',
  );
  await expect(main(['--erase-all'], '', 'AAAA')).rejects.toThrow();
});

test('bundled installer is self-contained, starts one CLI, and keeps imported CLIs inactive', async () => {
  const output = join(temporary(), 'installer.mjs');
  await buildInstallerRuntime(output);
  const runtime = readFileSync(output, 'utf8');
  expect(installerSource(runtime, Buffer.from('test'))).toContain('dGVzdA==');
  const result = spawnSync(process.execPath, [output, '--help'], {
    shell: false,
    encoding: 'utf8',
    timeout: 10000,
  });
  expect(result.status).toBe(0);
  expect(result.stdout).toContain('GigaUSB verified installer');
  expect(result.stdout).not.toContain('Use build <gzip>');
  expect(result.stderr).toBe('');
});
