import { createPublicKey } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { parseElfSegments } from '../installer/verify.ts';
import { OTA_ACTIVE_BASE, OTA_ACTIVE_LENGTH, OTA_HEADER_LENGTH } from '../lib/ota-format.ts';
import { allocationRecord, buildOta, journalSeed, otaLayout, type OtaKey } from './format.ts';
import { bootstrapHex, HEX_WARNING } from './hex.ts';

export function buildActive(m7Elf: Buffer, m4Elf: Buffer): Buffer {
  const active = Buffer.alloc(OTA_ACTIVE_LENGTH, 0xff);
  for (const segment of [...parseElfSegments(m7Elf, 'm7-ota'), ...parseElfSegments(m4Elf, 'm4')]) {
    segment.data.copy(active, segment.address - OTA_ACTIVE_BASE);
  }
  return active;
}

export function createBootstrapFiles(
  bootloader: Buffer,
  m7: Buffer,
  m4: Buffer,
  dashboard: Buffer,
  privatePem: OtaKey,
  buildId: string,
): Map<string, Buffer> {
  parseElfSegments(bootloader, 'bootloader');
  const active = buildActive(m7, m4);
  const image = buildOta(active, dashboard, privatePem, buildId);
  return new Map([
    ['bootloader.elf', Buffer.from(bootloader)],
    ['m7.elf', Buffer.from(m7)],
    ['m4.elf', Buffer.from(m4)],
    ['ota-active.bin', active],
    ['ota-manifest.bin', image.subarray(0, OTA_HEADER_LENGTH)],
    ['ota-state.bin', journalSeed()],
    ['ota-allocation.bin', allocationRecord()],
    [
      'ota-layout.json',
      Buffer.from(JSON.stringify(otaLayout(buildId, createPublicKey(privatePem)), null, 2) + '\n'),
    ],
  ]);
}

export const USAGE = `Node 24 offline OTA artifact builder (no hardware access)
  node src/ota/build.ts ota <m7.elf> <m4.elf> <dashboard.qspi.bin> <private.pem> <build-id> <output.ota.bin>
  node src/ota/build.ts bootstrap <bootloader.elf> <m7.elf> <m4.elf> <dashboard.qspi.bin> <private.pem> <build-id> <output-directory>
The build ID must match the M7 compile-time build ID. This tool does not prove that provenance.
OTA files are GIGAOTA1 images, not legacy MCUboot containers. Outputs must not exist.
Bootstrap outputs include bootstrap.hex and BOOTSTRAP-WARNING.txt.
${HEX_WARNING}`;

export function main(args: string[]) {
  if (args.length === 1 && ['--help', '-h'].includes(args[0])) {
    console.log(USAGE);
    return;
  }
  const [command, ...paths] = args;
  if (command === 'ota' && paths.length === 6) {
    const [m7, m4, dashboard, key, buildId, output] = paths;
    const image = buildOta(
      buildActive(readFileSync(m7), readFileSync(m4)),
      readFileSync(dashboard),
      readFileSync(key),
      buildId,
    );
    writeFileSync(output, image, { flag: 'wx' });
  } else if (command === 'bootstrap' && paths.length === 7) {
    const [boot, m7, m4, dashboard, key, buildId, output] = paths;
    const dashboardBytes = readFileSync(dashboard);
    const privatePem = readFileSync(key);
    const files = createBootstrapFiles(
      readFileSync(boot),
      readFileSync(m7),
      readFileSync(m4),
      dashboardBytes,
      privatePem,
      buildId,
    );
    files.set(
      'bootstrap.hex',
      Buffer.from(bootstrapHex(files, dashboardBytes, createPublicKey(privatePem))),
    );
    files.set('BOOTSTRAP-WARNING.txt', Buffer.from(HEX_WARNING + '\n'));
    mkdirSync(output);
    for (const [name, bytes] of files) writeFileSync(join(output, name), bytes, { flag: 'wx' });
  } else throw new Error(USAGE);
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  }
}
