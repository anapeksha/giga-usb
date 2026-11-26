export const OTA_HEADER_LENGTH = 256;
export const OTA_ACTIVE_LENGTH = 0x1e0000;
export const OTA_ACTIVE_BASE = 0x08020000;
export const OTA_M4_BASE = 0x08100000;
export const OTA_M4_OFFSET = 0xe0000;

export interface OtaHeader {
  buildId: string;
  total: number;
  totalLength: number;
  activeLen: number;
  dashboardLen: number;
  activeHash: Uint8Array;
  dashboardHash: Uint8Array;
  m7Sp: number;
  m7Reset: number;
  m4Sp: number;
  m4Reset: number;
}

/** Structural checks only: this does not authenticate a signature or payload. */
export function parseOtaHeader(bytes: Uint8Array): OtaHeader {
  if (bytes.length < OTA_HEADER_LENGTH) throw new Error('Truncated OTA header');
  const view = new DataView(bytes.buffer, bytes.byteOffset, OTA_HEADER_LENGTH);
  const word = (offset: number) => view.getUint32(offset, true);
  const equal = (offset: number, value: number) => word(offset) === value;
  if (
    !bytes.subarray(0, 8).every((b, i) => b === 'GIGAOTA1'.charCodeAt(i)) ||
    !equal(8, 1) ||
    !equal(12, 256) ||
    !equal(16, 1) ||
    !equal(20, 10) ||
    !equal(24, OTA_ACTIVE_BASE) ||
    !equal(28, OTA_ACTIVE_LENGTH) ||
    !equal(32, OTA_M4_BASE) ||
    bytes.subarray(168, 192).some((b) => b !== 0)
  )
    throw new Error('Unsupported OTA header or layout');
  const dashboardLen = word(36);
  if (dashboardLen < 4096 || dashboardLen > 0x100000 || dashboardLen % 4096 !== 0)
    throw new Error('Invalid OTA dashboard length');
  const field = bytes.subarray(120, 168);
  const nul = field.indexOf(0);
  const length = nul < 0 ? field.length : nul;
  const buildId = String.fromCharCode(...field.subarray(0, length));
  if (!/^[A-Za-z0-9._-]{1,48}$/.test(buildId) || field.subarray(length).some((b) => b !== 0))
    throw new Error('Invalid OTA build ID');
  const m7Sp = word(104),
    m7Reset = word(108),
    m4Sp = word(112),
    m4Reset = word(116);
  const reset = (value: number, start: number, end: number) =>
    value % 2 === 1 && value - 1 >= start && value - 1 < end;
  if (
    m7Sp !== 0x24080000 ||
    m4Sp !== 0x30020000 ||
    !reset(m7Reset, OTA_ACTIVE_BASE, OTA_M4_BASE) ||
    !reset(m4Reset, OTA_M4_BASE, 0x08200000)
  )
    throw new Error('Invalid OTA vectors');
  const total = OTA_HEADER_LENGTH + OTA_ACTIVE_LENGTH + dashboardLen;
  return {
    buildId,
    total,
    totalLength: total,
    activeLen: OTA_ACTIVE_LENGTH,
    dashboardLen,
    activeHash: bytes.slice(40, 72),
    dashboardHash: bytes.slice(72, 104),
    m7Sp,
    m7Reset,
    m4Sp,
    m4Reset,
  };
}

export const parseHeader = parseOtaHeader;
