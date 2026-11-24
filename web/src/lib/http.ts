import {
  applyRecord,
  validRecord,
  isUsbTraceRecord,
  type ProtocolRecord,
  type ProtocolState,
} from './protocol';
export type Command =
  | { proxy_enable: { generation: number } }
  | 'proxy_disable'
  | 'start'
  | 'stop'
  | 'reset'
  | 'status'
  | 'storage_list'
  | { storage_read: { file: number; offset: number } };
export interface EventPage {
  records: ProtocolRecord[];
  cursor: number;
  dropped: number;
}

export function applyEventPage(
  state: ProtocolState & { transportDropped: number },
  page: EventPage,
): void {
  for (const record of page.records) applyRecord(state, record);
  state.transportDropped = page.dropped;
}

export function createLossRefreshGuard(now = () => performance.now()) {
  let previous = 0;
  let lastRefresh = -Infinity;
  return (dropped: number) => {
    const increased = dropped > previous;
    previous = dropped;
    const time = now();
    // Persistent assembly overflow must not create a status-refresh feedback loop.
    if (!increased || time - lastRefresh < 5000) return false;
    lastRefresh = time;
    return true;
  };
}

export interface TraceFilterInput {
  endpoint: string;
  direction: string;
  kind: string;
  errors: boolean;
  q: string;
}
export type TraceFilters = Readonly<{
  endpoint?: number;
  direction?: 'in' | 'out';
  kind?: 'control' | 'interrupt' | 'bulk';
  errors: boolean;
  q: string;
  active: boolean;
  error: string;
}>;
export function normalizeTraceFilters(input: TraceFilterInput): TraceFilters {
  const endpoint = input.endpoint.trim();
  const direction = input.direction.toLowerCase();
  const kind = input.kind.toLowerCase();
  const q = input.q.trim();
  let error = '';
  if (endpoint && (!/^\d+$/.test(endpoint) || Number(endpoint) > 255))
    error = 'Enter a whole endpoint address from 0 to 255.';
  else if (new TextEncoder().encode(q).length > 96)
    error = 'Search must be at most 96 UTF-8 bytes. Shorten your search.';
  else if (direction && !['in', 'out'].includes(direction)) error = 'Choose IN or OUT.';
  else if (kind && !['control', 'interrupt', 'bulk'].includes(kind))
    error = 'Choose a supported transfer type.';
  return Object.freeze({
    endpoint: endpoint ? Number(endpoint) : undefined,
    direction: (direction || undefined) as TraceFilters['direction'],
    kind: (kind || undefined) as TraceFilters['kind'],
    errors: input.errors,
    q,
    active: !!(endpoint || direction || kind || input.errors || q),
    error,
  });
}
export interface TracePage {
  records: { sequence: number; record: ProtocolRecord }[];
  cursor: number;
  dropped: number;
}
export function validTracePage(page: unknown): page is TracePage {
  return (
    typeof page === 'object' &&
    page !== null &&
    'cursor' in page &&
    u32(page.cursor) &&
    'dropped' in page &&
    u32(page.dropped) &&
    'records' in page &&
    Array.isArray(page.records) &&
    page.records.length <= 3 &&
    page.records.every(
      (item) =>
        typeof item === 'object' &&
        item !== null &&
        'sequence' in item &&
        u32(item.sequence) &&
        'record' in item &&
        validRecord(item.record) &&
        isUsbTraceRecord(item.record),
    )
  );
}
export async function getTracePage(
  cursor: number,
  filters: TraceFilters,
  signal: AbortSignal,
  request: typeof fetch = fetch,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<TracePage> {
  if (filters.error || !u32(cursor)) throw new Error(filters.error || 'Invalid trace cursor');
  const params = new URLSearchParams({ after: String(cursor) });
  if (filters.endpoint !== undefined) params.set('endpoint', String(filters.endpoint));
  if (filters.direction) params.set('direction', filters.direction);
  if (filters.kind) params.set('kind', filters.kind);
  if (filters.errors) params.set('errors', 'true');
  if (filters.q) params.set('q', filters.q);
  const page: unknown = await diagnosticRequest(signal, timeoutMs, async (requestSignal) => {
    const response = await request(`/api/trace?${params}`, {
      signal: requestSignal,
      cache: 'no-store',
    });
    if (!response.ok) throw new Error(`Trace failed: HTTP ${response.status}`);
    return response.json();
  });
  signal.throwIfAborted();
  if (!validTracePage(page)) throw new Error('Invalid trace response');
  return page;
}

const REQUEST_TIMEOUT_MS = 8000;

async function diagnosticRequest<T>(
  signal: AbortSignal,
  timeoutMs: number,
  operation: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const controller = new AbortController();
  let rejectAbort!: (reason: unknown) => void;
  const cancelled = new Promise<never>((_, reject) => {
    rejectAbort = reject;
  });
  const abort = () => {
    controller.abort(signal.reason);
    rejectAbort(signal.reason);
  };
  signal.addEventListener('abort', abort, { once: true });
  const timer = setTimeout(() => {
    const error = new Error(`Diagnostic request timed out after ${timeoutMs} ms. Try again.`);
    controller.abort(error);
    rejectAbort(error);
  }, timeoutMs);
  try {
    if (signal.aborted) abort();
    // Race as well as abort so injected requests/body readers cannot ignore the deadline.
    return await Promise.race([
      cancelled,
      Promise.resolve().then(() => {
        controller.signal.throwIfAborted();
        return operation(controller.signal);
      }),
    ]);
  } finally {
    clearTimeout(timer);
    signal.removeEventListener('abort', abort);
  }
}

const u32 = (value: unknown): value is number =>
  typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 0xffffffff;
function validCommandObject(command: unknown): boolean {
  if (typeof command !== 'object' || command === null || Array.isArray(command)) return false;
  const entries = Object.entries(command);
  if (entries.length !== 1) return false;
  const [kind, value] = entries[0];
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  if (kind === 'proxy_enable')
    return Object.keys(value).length === 1 && 'generation' in value && u32(value.generation);
  return (
    kind === 'storage_read' &&
    'offset' in value &&
    'file' in value &&
    u32(value.offset) &&
    value.offset <= 65536 &&
    u32(value.file) &&
    value.file <= 65535
  );
}

export async function sendCommand(
  command: unknown,
  signal: AbortSignal,
  request: typeof fetch = fetch,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<void> {
  if (
    typeof command === 'string'
      ? !['start', 'stop', 'reset', 'status', 'storage_list', 'proxy_disable'].includes(command)
      : !validCommandObject(command)
  )
    throw new Error('Unknown command');
  await diagnosticRequest(signal, timeoutMs, async (requestSignal) => {
    const response = await request('/api/command', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ command }),
      signal: requestSignal,
    });
    if (response.status === 503) throw new Error('Command queue full. Try again.');
    if (response.status !== 202) throw new Error(`Command failed: HTTP ${response.status}`);
  });
}

export function validEventPage(page: unknown): page is EventPage {
  if (typeof page !== 'object' || page === null) return false;
  return (
    'cursor' in page &&
    u32(page.cursor) &&
    'dropped' in page &&
    u32(page.dropped) &&
    'records' in page &&
    Array.isArray(page.records) &&
    page.records.length <= 3 &&
    page.records.every(validRecord)
  );
}
