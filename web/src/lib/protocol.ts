export interface DeviceDescriptor {
  usb_version: number;
  vendor_id: number;
  product_id: number;
  device_version: number;
  class: number;
  subclass: number;
  protocol: number;
  max_packet_size: number;
  configurations: number;
}
export interface InterfaceDescriptor {
  number: number;
  alternate: number;
  class: number;
  subclass: number;
  protocol: number;
  endpoints: number;
}
export interface EndpointDescriptor {
  interface: number;
  alternate: number;
  address: number;
  attributes: number;
  max_packet_size: number;
  interval: number;
}
export interface Keyboard {
  modifiers: number;
  keys: number[];
  pressed: number[];
  released: number[];
}
export interface Mouse {
  buttons: number;
  dx: number;
  dy: number;
  wheel: number;
}
export type HostError =
  | 'timeout'
  | 'stall'
  | 'nak'
  | 'disconnected'
  | 'babble'
  | 'transaction'
  | 'overflow'
  | 'invalid_descriptor'
  | 'unsupported'
  | 'controller';
export type Lifecycle = 'stopped' | 'waiting' | 'attached' | 'enumerating' | 'ready' | 'error';
export interface NetworkInfo {
  mode: 'setup' | 'associating' | 'link_wait' | 'dhcp' | 'connected' | 'error';
  mac: number[];
  ip: string | null;
  error: string | null;
}
export interface HostPayloads {
  network: { info: NetworkInfo };
  lifecycle: { state: Lifecycle; session_id: number };
  transfer_context: {
    session_id: number;
    transfer_id: number;
    endpoint: number;
    kind: 'control' | 'interrupt' | 'bulk';
    stage: 'setup' | 'data' | 'status';
    elapsed_us: number;
    retries: number;
  };
  diagnostic: {
    code: HostError;
    setup: number[];
    session_id: number;
    transfer_id: number;
    stage: 'setup' | 'data' | 'status';
    endpoint: number;
    elapsed_us: number;
    retries: number;
    hcint: number;
    hcchar: number;
    hctsiz: number;
    hprt: number;
  };
  storage: { blocks: number; block_size: number; vendor: string; product: string; fat32: boolean };
  file: { index: number; name: string; size: number; directory: boolean };
  file_data: { index: number; offset: number; total_size: number; data: number[]; eof: boolean };
  storage_error: { message: string };
  attached: { speed: 'low' | 'full' };
  configured: { address: number; configuration: number };
  // Completed own-host packet payload, not passive capture or USB wire framing.
  // PID uses STM32 DPID encoding: 0=DATA0, 2=DATA1, 3=SETUP.
  transfer: {
    endpoint: number;
    bytes: number;
    direction: 'in' | 'out';
    pid: number;
    data: number[];
  };
  hid_report: { endpoint: number; data: number[] };
  status: {
    enabled: boolean;
    attached: boolean;
    address: number;
    transfers: number;
    errors: number;
    dropped: number;
  };
  error: { code: HostError };
  descriptor: {
    descriptor_type: number;
    index: number;
    offset: number;
    total_length: number;
    data: number[];
  };
}
export type HostEvent =
  | 'ready'
  | 'detached'
  | 'port_reset'
  | { [K in keyof HostPayloads]: { [P in K]: HostPayloads[K] } }[keyof HostPayloads];
export type ParsedHostEvent =
  | ['ready' | 'detached' | 'port_reset', Record<string, never>]
  | { [K in keyof HostPayloads]: [K, HostPayloads[K]] }[keyof HostPayloads];
export type RecordEvent =
  | { type: 'hello'; protocol_version: number }
  | { type: 'host'; event: HostEvent }
  | { type: 'device'; descriptor: DeviceDescriptor }
  | { type: 'interface'; descriptor: InterfaceDescriptor }
  | { type: 'endpoint'; descriptor: EndpointDescriptor }
  | ({ type: 'keyboard' } & Keyboard)
  | ({ type: 'mouse' } & Mouse)
  | { type: 'error'; message: string }
  | { type: 'dropped'; count: number };
export interface ProtocolRecord {
  /** Browser arrival order, including events omitted from USB history. */
  sequence?: number;
  timestamp_ms: number;
  event: RecordEvent;
}
export interface ProtocolState {
  history: ProtocolRecord[];
  recordSequence: number;
  network?: NetworkInfo;
  lifecycle: Lifecycle | null;
  session: number | null;
  diagnostic: HostPayloads['diagnostic'] | null;
  storage: HostPayloads['storage'] | null;
  files: HostPayloads['file'][];
  storageError: string | null;
  queueDropped: number;
  device: DeviceDescriptor | null;
  interfaces: InterfaceDescriptor[];
  endpoints: EndpointDescriptor[];
  attached: boolean;
  speed: 'low' | 'full' | null;
  address: number | null;
  enabled: boolean | null;
  protocol: number | null;
  transfers: number;
  errors: number;
  dropped: number;
  warnings: number;
  keyboard: Keyboard | null;
  mouse: Mouse | null;
}
const object = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value);
export const HISTORY_LIMIT = 500;
export const PROTOCOL_VERSION = 9;
const uint = (v: unknown, max = Number.MAX_SAFE_INTEGER): v is number =>
  typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 && v <= max;
const fields = (v: unknown, names: string[]) =>
  object(v) &&
  names.every((k) =>
    uint(
      v[k],
      ['usb_version', 'vendor_id', 'product_id', 'device_version', 'max_packet_size'].includes(k)
        ? 65535
        : 255,
    ),
  );
const bytes = (v: unknown): v is number[] => Array.isArray(v) && v.every((n) => uint(n, 255));
const errors = new Set([
  'timeout',
  'stall',
  'nak',
  'disconnected',
  'babble',
  'transaction',
  'overflow',
  'invalid_descriptor',
  'unsupported',
  'controller',
]);
const isHostError = (value: unknown): value is HostError =>
  typeof value === 'string' && errors.has(value);
export function hostEvent(event: unknown): ParsedHostEvent | null {
  if (event === 'ready' || event === 'detached' || event === 'port_reset') return [event, {}];
  if (!object(event) || Object.keys(event).length !== 1) return null;
  const [type, v] = Object.entries(event)[0];
  if (!object(v)) return null;
  const u32 = (n: unknown) => uint(n, 0xffffffff);
  const stage = (n: unknown) => typeof n === 'string' && ['setup', 'data', 'status'].includes(n);
  const text = (n: unknown, max: number) =>
    typeof n === 'string' && new TextEncoder().encode(n).length <= max;
  const context = () =>
    u32(v.session_id) && u32(v.transfer_id) && uint(v.endpoint, 255) && stage(v.stage);
  let valid: boolean | undefined;
  switch (type) {
    case 'network': {
      const info = v.info;
      valid =
        object(info) &&
        text(info.mode, 12) &&
        ['setup', 'associating', 'link_wait', 'dhcp', 'connected', 'error'].includes(
          info.mode as string,
        ) &&
        bytes(info.mac) &&
        info.mac.length === 6 &&
        (info.ip === null || text(info.ip, 15)) &&
        (info.error === null || text(info.error, 64));
      break;
    }
    case 'lifecycle':
      valid =
        u32(v.session_id) &&
        typeof v.state === 'string' &&
        ['stopped', 'waiting', 'attached', 'enumerating', 'ready', 'error'].includes(v.state);
      break;
    case 'transfer_context':
      valid =
        context() &&
        typeof v.kind === 'string' &&
        ['control', 'interrupt', 'bulk'].includes(v.kind) &&
        uint(v.elapsed_us) &&
        u32(v.retries);
      break;
    case 'diagnostic':
      valid =
        context() &&
        isHostError(v.code) &&
        bytes(v.setup) &&
        v.setup.length === 8 &&
        uint(v.elapsed_us) &&
        u32(v.retries) &&
        ['hcint', 'hcchar', 'hctsiz', 'hprt'].every((k) => u32(v[k]));
      break;
    case 'storage':
      valid =
        u32(v.blocks) &&
        u32(v.block_size) &&
        text(v.vendor, 8) &&
        text(v.product, 16) &&
        typeof v.fat32 === 'boolean';
      break;
    case 'file':
      valid =
        uint(v.index, 65535) && text(v.name, 13) && u32(v.size) && typeof v.directory === 'boolean';
      break;
    case 'file_data':
      valid =
        uint(v.index, 65535) &&
        u32(v.offset) &&
        u32(v.total_size) &&
        bytes(v.data) &&
        v.data.length <= 128 &&
        typeof v.eof === 'boolean';
      break;
    case 'storage_error':
      valid = text(v.message, 96);
      break;
  }
  if (valid !== undefined) return valid ? ([type, v] as ParsedHostEvent) : null;
  switch (type) {
    case 'attached':
      return v.speed === 'low' || v.speed === 'full' ? [type, { speed: v.speed }] : null;
    case 'configured':
      return uint(v.address, 255) && uint(v.configuration, 255)
        ? [type, { address: v.address, configuration: v.configuration }]
        : null;
    case 'transfer':
      return uint(v.endpoint, 255) &&
        (v.endpoint & 0x70) === 0 &&
        uint(v.bytes, 64) &&
        bytes(v.data) &&
        v.data.length === v.bytes &&
        (v.pid === 0 || v.pid === 2 || v.pid === 3) &&
        (v.direction === 'in' || v.direction === 'out') &&
        (v.direction === 'in') === ((v.endpoint & 0x80) !== 0) &&
        (v.pid !== 3 || (v.endpoint === 0 && v.direction === 'out' && v.bytes === 8))
        ? [
            type,
            {
              endpoint: v.endpoint,
              bytes: v.bytes,
              direction: v.direction,
              pid: v.pid,
              data: v.data,
            },
          ]
        : null;
    case 'hid_report':
      return uint(v.endpoint, 255) && bytes(v.data) && v.data.length <= 64
        ? [type, { endpoint: v.endpoint, data: v.data }]
        : null;
    case 'status':
      return typeof v.enabled === 'boolean' &&
        typeof v.attached === 'boolean' &&
        uint(v.address, 255) &&
        u32(v.transfers) &&
        u32(v.errors) &&
        u32(v.dropped)
        ? [
            type,
            {
              enabled: v.enabled,
              attached: v.attached,
              address: v.address,
              transfers: v.transfers,
              errors: v.errors,
              dropped: v.dropped,
            },
          ]
        : null;
    case 'error':
      return isHostError(v.code) ? [type, { code: v.code }] : null;
    case 'descriptor':
      return uint(v.descriptor_type, 255) &&
        uint(v.index, 255) &&
        uint(v.offset, 65535) &&
        uint(v.total_length, 65535) &&
        bytes(v.data) &&
        v.data.length <= 128
        ? [
            type,
            {
              descriptor_type: v.descriptor_type,
              index: v.index,
              offset: v.offset,
              total_length: v.total_length,
              data: v.data,
            },
          ]
        : null;
    default:
      return null;
  }
}
export function validRecord(r: unknown): r is ProtocolRecord {
  if (!object(r) || !uint(r.timestamp_ms) || !object(r.event)) return false;
  const e = r.event;
  switch (e.type) {
    case 'hello':
      return uint(e.protocol_version, 0xffffffff);
    case 'host':
      return !!hostEvent(e.event);
    case 'device':
      return (
        object(e.descriptor) &&
        uint(e.descriptor.max_packet_size, 255) &&
        fields(e.descriptor, [
          'usb_version',
          'vendor_id',
          'product_id',
          'device_version',
          'class',
          'subclass',
          'protocol',
          'max_packet_size',
          'configurations',
        ])
      );
    case 'interface':
      return fields(e.descriptor, [
        'number',
        'alternate',
        'class',
        'subclass',
        'protocol',
        'endpoints',
      ]);
    case 'endpoint':
      return (
        fields(e.descriptor, [
          'interface',
          'alternate',
          'address',
          'attributes',
          'max_packet_size',
          'interval',
        ]) &&
        object(e.descriptor) &&
        Object.entries(e.descriptor).every(([k, v]) =>
          uint(v, k === 'max_packet_size' ? 65535 : 255),
        )
      );
    case 'keyboard':
      return (
        uint(e.modifiers, 255) &&
        [e.keys, e.pressed, e.released].every((v) => bytes(v) && v.length <= 6)
      );
    case 'mouse':
      return (
        uint(e.buttons, 255) &&
        [e.dx, e.dy, e.wheel].every(
          (v) => typeof v === 'number' && Number.isInteger(v) && v >= -128 && v <= 127,
        )
      );
    case 'error':
      return typeof e.message === 'string' && new TextEncoder().encode(e.message).length <= 96;
    case 'dropped':
      return uint(e.count, 0xffffffff);
    default:
      return false;
  }
}
export function createState(): ProtocolState {
  return {
    history: [],
    recordSequence: 0,
    lifecycle: null,
    session: null,
    diagnostic: null,
    storage: null,
    files: [],
    storageError: null,
    queueDropped: 0,
    device: null,
    interfaces: [],
    endpoints: [],
    attached: false,
    speed: null,
    address: null,
    enabled: null,
    protocol: null,
    transfers: 0,
    errors: 0,
    dropped: 0,
    warnings: 0,
    keyboard: null,
    mouse: null,
  };
}
function clearDevice(s: ProtocolState) {
  s.storage = null;
  s.files = [];
  s.storageError = null;
  s.diagnostic = null;
  s.device = null;
  s.interfaces = [];
  s.endpoints = [];
  s.keyboard = null;
  s.mouse = null;
  s.address = null;
  s.speed = null;
}
// Generic processor/network errors have no reliable USB scope. Typed host errors do.
export function isUsbTraceRecord(r: ProtocolRecord): boolean {
  const p = r.event.type === 'host' ? hostEvent(r.event.event) : null;
  return !!p && ['transfer', 'transfer_context', 'diagnostic', 'error'].includes(p[0]);
}
export function applyRecord(s: ProtocolState, r: unknown): boolean {
  if (!validRecord(r)) return false;
  s.recordSequence++;
  if (isUsbTraceRecord(r)) {
    s.history.push({ ...r, sequence: s.recordSequence });
    if (s.history.length > HISTORY_LIMIT) s.history.shift();
  }
  const e = r.event;
  switch (e.type) {
    case 'hello':
      s.protocol = e.protocol_version;
      break;
    case 'device':
      s.device = { ...e.descriptor };
      break;
    case 'interface':
      s.interfaces = [
        ...s.interfaces.filter(
          (d) => d.number !== e.descriptor.number || d.alternate !== e.descriptor.alternate,
        ),
        { ...e.descriptor },
      ].slice(-256);
      break;
    case 'endpoint':
      s.endpoints = [
        ...s.endpoints.filter(
          (d) =>
            d.address !== e.descriptor.address ||
            d.interface !== e.descriptor.interface ||
            d.alternate !== e.descriptor.alternate,
        ),
        { ...e.descriptor },
      ].slice(-256);
      break;
    case 'keyboard':
      s.keyboard = e;
      break;
    case 'mouse':
      s.mouse = e;
      break;
    case 'error':
      s.warnings++;
      break;
    case 'dropped':
      s.queueDropped += e.count;
      break;
    case 'host': {
      const parsed = hostEvent(e.event);
      if (!parsed) return false;
      const [type, v] = parsed;
      if (type === 'lifecycle') {
        if (s.session !== v.session_id) clearDevice(s);
        s.session = v.session_id;
        s.lifecycle = v.state;
        s.enabled = v.state !== 'stopped';
        s.attached =
          ['attached', 'enumerating', 'ready'].includes(v.state) ||
          (v.state === 'error' && s.attached);
      }
      if (type === 'network') s.network = v.info;
      if (type === 'diagnostic') s.diagnostic = v;
      if (type === 'storage') s.storage = v;
      if (type === 'storage_error') s.storageError = v.message;
      if (type === 'file') s.files = [...s.files.filter((f) => f.index !== v.index), v].slice(-32);
      if (type === 'attached') {
        if (!s.attached && s.lifecycle !== 'attached') clearDevice(s);
        s.attached = true;
        s.speed = v.speed;
      }
      if (type === 'detached') {
        s.attached = false;
      }

      if (type === 'configured') {
        s.attached = true;
        s.address = v.address;
      }
      if (type === 'transfer') s.transfers++;
      if (type === 'error') s.errors++;
      if (type === 'status') {
        Object.assign(s, {
          attached: v.attached,
          enabled: v.enabled,
          address: v.address,
          transfers: v.transfers,
          errors: v.errors,
          dropped: v.dropped,
        });
      }
      break;
    }
  }
  return true;
}
export const hex = (n: number | null | undefined, width = 2) =>
  n == null ? '—' : '0x' + n.toString(16).toUpperCase().padStart(width, '0');
export function summary(r: ProtocolRecord): string {
  const e = r.event;
  if (e.type === 'host') {
    const parsed = hostEvent(e.event);
    if (!parsed) return 'host · invalid';
    const [type, value] = parsed;
    switch (type) {
      case 'network':
        return `Network · ${value.info.mode.replaceAll('_', ' ')}${value.info.ip === null ? '' : ` · ${value.info.ip}`}`;
      case 'transfer':
        return `${value.direction === 'in' ? 'Received' : 'Sent'} · endpoint ${value.endpoint}`;
      case 'lifecycle':
        return value.state.charAt(0).toUpperCase() + value.state.slice(1);
      case 'diagnostic':
        return `${value.code.replaceAll('_', ' ')} · ${value.stage}`;
      case 'error':
        return value.code.replaceAll('_', ' ');
      case 'storage_error':
        return value.message;
      case 'file':
        return `${value.name} · ${value.size} bytes`;
      case 'file_data':
        return 'File data';
      case 'storage':
        return `Storage · ${value.vendor} ${value.product}`;
      case 'attached':
        return `Attached · ${value.speed} speed`;
      case 'configured':
        return `Configured · address ${value.address}`;
      case 'hid_report':
        return 'Input report';
      case 'descriptor':
        return 'Device details';
      default:
        return type.replaceAll('_', ' ');
    }
  }
  if (e.type === 'error') return e.message;
  if (e.type === 'dropped') return `Gap · ${e.count} records dropped`;
  return {
    hello: 'Connected',
    device: 'Device details',
    interface: 'Interface details',
    endpoint: 'Endpoint details',
    keyboard: 'Keyboard input',
    mouse: 'Mouse input',
  }[e.type];
}
