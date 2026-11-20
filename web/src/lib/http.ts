import type { ProtocolRecord, ProtocolState } from './protocol';
export type Command =
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
import { applyRecord, validRecord } from './protocol';

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
    // Persistent assembly overflow must not create a status/poll feedback loop.
    if (!increased || time - lastRefresh < 5000) return false;
    lastRefresh = time;
    return true;
  };
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
export async function sendCommand(
  command: string | { storage_read: { file: number; offset: number } },
  signal: AbortSignal,
  request: typeof fetch = fetch,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<void> {
  if (
    typeof command === 'string'
      ? !['start', 'stop', 'reset', 'status', 'storage_list'].includes(command)
      : !command.storage_read ||
        !u32(command.storage_read.offset) ||
        command.storage_read.offset > 65536 ||
        !u32(command.storage_read.file) ||
        command.storage_read.file > 65535
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

export async function getEventPage(
  cursor: number,
  signal: AbortSignal,
  request: typeof fetch = fetch,
  timeoutMs = REQUEST_TIMEOUT_MS,
): Promise<EventPage> {
  const page: unknown = await diagnosticRequest(signal, timeoutMs, async (requestSignal) => {
    const response = await request(`/api/events?after=${cursor}`, {
      signal: requestSignal,
      cache: 'no-store',
    });
    if (!response.ok) throw new Error(`Events failed: HTTP ${response.status}`);
    return response.json();
  });
  signal.throwIfAborted();
  if (!validEventPage(page)) throw new Error('Invalid events response');
  return page;
}

function validEventPage(page: unknown): page is EventPage {
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
