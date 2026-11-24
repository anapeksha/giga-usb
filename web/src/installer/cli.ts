import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { installDistribution } from './install.ts';
import { verifyDistribution } from './verify.ts';

const embeddedImage = 'GIGAUSB_SIGNED_PAYLOAD_V1';
export const payloadMarker = ['GIGAUSB', 'SIGNED', 'PAYLOAD', 'V1'].join('_');

export function installerSource(runtime: string, image: Buffer) {
  const marker = JSON.stringify(payloadMarker);
  if (runtime.split(marker).length !== 2) throw new Error('Expected one installer payload marker');
  return runtime.replace(marker, JSON.stringify(image.toString('base64')));
}

export function authenticatedRuntime(source: string, payload: string) {
  const value = JSON.stringify(payload);
  if (source.split(value).length !== 2) throw new Error('Invalid installer payload placement');
  return source.replace(value, JSON.stringify(payloadMarker));
}

export async function main(argv: string[], source: string, payload: string) {
  const { values } = parseArgs({
    args: argv,
    options: {
      key: { type: 'string' },
      install: { type: 'boolean', default: false },
      programmer: { type: 'string' },
      probe: { type: 'string' },
      'backup-dir': { type: 'string' },
      'resume-from': { type: 'string' },
      'replace-dashboard': { type: 'boolean', default: false },
      'force-dashboard-rewrite': { type: 'boolean', default: false },
      help: { type: 'boolean', short: 'h' },
    },
  });
  if (values.help) {
    console.log(
      `GigaUSB verified installer (Node.js 24+, macOS/Linux)\n\nVerify offline:\n  node installer.mjs --key trusted-public.pem\n\nInstall using an external SWD probe and STM32CubeProgrammer:\n  node installer.mjs --key trusted-public.pem --install \\\n    --programmer /absolute/path/to/STM32_Programmer_CLI --probe SERIAL \\\n    --backup-dir /absolute/path/to/backups --replace-dashboard\n\nThe installer itself must come from a trusted source. Supply the public key\nindependently; a key downloaded beside an untrusted installer is not a trust root.\nNo Internet access, imgtool, Rust compiler or extracted files are needed at runtime.\nThe signed container is verified and unpacked, never flashed directly.\nInstallation interrupts both cores, replaces their firmware, and overwrites the\nArduino bootloader. Only dashboard sectors may be erased in external flash.\nExisting unrelated data, option bytes, protection settings and OTP are preserved.\nTwo complete external reads and internal/option backups are retained.\nFailures may leave the board halted; retain the backups and recovery logs.\n--force-dashboard-rewrite exercises programming even for an identical dashboard\nand also requires --replace-dashboard. No mass-erase or option-write mode exists.\n--resume-from /absolute/prior-install-directory accepts only matching trusted\nbackups and an exact staged/committed image. It cannot recover arbitrary damage\nand cannot be combined with --force-dashboard-rewrite.`,
    );
    return;
  }
  if (!values.key) throw new Error('--key must name an independently trusted public PEM');
  const pem = readFileSync(values.key);
  if (pem.length > 16384 || !pem.toString('ascii').includes('-----BEGIN PUBLIC KEY-----'))
    throw new Error('Expected a public PEM, not a private signing key');
  if (
    payload === payloadMarker ||
    payload.length > 90 * 1024 * 1024 ||
    payload.length % 4 !== 0 ||
    !/^[A-Za-z0-9+/]+={0,2}$/.test(payload)
  )
    throw new Error('Missing or malformed embedded signed image');
  const image = Buffer.from(payload, 'base64');
  if (image.toString('base64') !== payload) throw new Error('Non-canonical installer payload');
  const distribution = verifyDistribution(image, pem);
  const runtime = distribution.files.get('installer-runtime.mjs');
  if (!runtime?.equals(Buffer.from(authenticatedRuntime(source, payload))))
    throw new Error('Installer runtime differs from the authenticated distribution');
  console.log(
    `Verified ${distribution.tag} (${distribution.commit}); signature and all assets match.`,
  );
  if (!values.install) {
    console.log('Verification only. No probe or board access performed. Add --install to program.');
    return;
  }
  if (!values.programmer || !values.probe || !values['backup-dir'])
    throw new Error('--install requires --programmer, --probe and --backup-dir');
  const result = await installDistribution(distribution, {
    programmer: values.programmer,
    probe: values.probe,
    backupDir: values['backup-dir'],
    replaceDashboard: values['replace-dashboard'],
    forceDashboardRewrite: values['force-dashboard-rewrite'],
    resumeFrom: values['resume-from'],
  });
  console.log(`Installation and exact readback verified. Evidence: ${result.backupDir}`);
  console.log('Board system-reset. USB/browser/HID operation requires separate runtime testing.');
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(
    process.argv.slice(2),
    readFileSync(fileURLToPath(import.meta.url), 'utf8'),
    embeddedImage,
  ).catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
