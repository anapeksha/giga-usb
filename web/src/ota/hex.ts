import { parseElfSegments } from '../installer/verify.ts';
import { OTA_ACTIVE_BASE } from '../lib/ota-format.ts';
import { allocationRecord, journalSeed, verifyOta, type OtaKey } from './format.ts';

export interface HexRegion {
  address: number;
  data: Buffer;
}

/** Intel HEX with 16-byte data records and extended linear addresses. */
export function encodeHex(regions: readonly HexRegion[]): string {
  const sorted = [...regions].sort((a, b) => a.address - b.address);
  let end = 0;
  for (const { address, data } of sorted) {
    if (!Number.isSafeInteger(address) || address < 0 || address + data.length > 0x100000000)
      throw new Error('HEX address outside 32-bit range');
    if (!data.length) throw new Error('Empty HEX region');
    if (address < end) throw new Error('Overlapping HEX regions');
    end = address + data.length;
  }
  const lines: string[] = [];
  const record = (type: number, address: number, data: Buffer) => {
    const bytes = Buffer.from([data.length, address >>> 8, address & 255, type, ...data]);
    const checksum = -bytes.reduce((sum, byte) => sum + byte, 0) & 255;
    lines.push(
      ':' +
        bytes.toString('hex').toUpperCase() +
        checksum.toString(16).padStart(2, '0').toUpperCase(),
    );
  };
  let upper = -1;
  for (const { address, data } of sorted) {
    for (let offset = 0; offset < data.length;) {
      const absolute = address + offset;
      const high = Math.floor(absolute / 65536);
      if (high !== upper) {
        record(4, 0, Buffer.from([high >>> 8, high & 255]));
        upper = high;
      }
      const low = absolute % 65536;
      const count = Math.min(16, data.length - offset, 65536 - low);
      record(0, low, data.subarray(offset, offset + count));
      offset += count;
    }
  }
  record(1, 0, Buffer.alloc(0));
  return lines.join('\n') + '\n';
}

export const HEX_WARNING = `Bootstrap HEX is not an OTA upload or an authenticated distribution container.
External addresses use 0x90000000 + NOR offset and REQUIRE a separately validated
part- and board-routing-specific external loader. HEX does not initialize QSPI,
validate existing allocations, preserve backups, specify erase policy, or enforce
header-last programming. Unrepresented bytes are NOT implicitly erased.
Initial provisioning is NOT power-fail-safe. Retain SWD recovery; do not change
option bytes or mass erase. No hardware programming or validation is performed.`;

export function bootstrapHex(
  files: ReadonlyMap<string, Buffer>,
  dashboard: Buffer,
  key: OtaKey,
): string {
  const get = (name: string) => {
    const bytes = files.get(name);
    if (!bytes) throw new Error(`Missing bootstrap file: ${name}`);
    return bytes;
  };
  const active = get('ota-active.bin');
  const manifest = get('ota-manifest.bin');
  verifyOta(Buffer.concat([manifest, active, dashboard]), key);
  const state = get('ota-state.bin');
  const allocation = get('ota-allocation.bin');
  if (!state.equals(journalSeed()) || !allocation.equals(allocationRecord()))
    throw new Error('Bootstrap requires exact factory state and allocation record');
  const boot = Buffer.alloc(0x20000, 0xff);
  for (const segment of parseElfSegments(get('bootloader.elf'), 'bootloader'))
    segment.data.copy(boot, segment.address - 0x08000000);
  return encodeHex([
    { address: 0x08000000, data: boot },
    { address: OTA_ACTIVE_BASE, data: active },
    { address: 0x90200000, data: state },
    { address: 0x90202000, data: allocation },
    { address: 0x90220000, data: manifest },
    { address: 0x90f00000, data: dashboard },
  ]);
}
