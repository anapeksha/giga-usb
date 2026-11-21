import { PROTOCOL_VERSION } from './protocol';
import {
  validEventPage,
  validTracePage,
  type EventPage,
  type TracePage,
  type TraceFilters,
} from './http';

export class EventStreamProtocolError extends Error {}

/** Incremental SSE framing, bounded in encoded bytes even for unknown events. */
export function createSseParser(deliver: (event: string, data: string) => void) {
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let line = '';
  let event = '';
  let data: string[] = [];
  let bytes = 0;
  let cr = false;
  const endLine = () => {
    if (!line) {
      if (data.length) deliver(event || 'message', data.join('\n'));
      event = '';
      data = [];
      bytes = 0;
    } else if (!line.startsWith(':')) {
      const colon = line.indexOf(':');
      const field = colon < 0 ? line : line.slice(0, colon);
      let value = colon < 0 ? '' : line.slice(colon + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      if (field === 'event') event = value;
      if (field === 'data') data.push(value);
    }
    line = '';
  };
  return (chunk: Uint8Array) => {
    let start = 0;
    while (start < chunk.length) {
      if (cr) {
        cr = false;
        if (chunk[start] === 10) {
          if (++bytes > 8192) throw new EventStreamProtocolError('SSE frame exceeds 8192 bytes');
          start++;
        }
        endLine();
        if (start === chunk.length) break;
      }
      let end = start;
      while (end < chunk.length && chunk[end] !== 10 && chunk[end] !== 13) end++;
      const terminated = end < chunk.length;
      const stop = end + Number(terminated);
      bytes += stop - start;
      if (bytes > 8192) throw new EventStreamProtocolError('SSE frame exceeds 8192 bytes');
      try {
        const text = decoder.decode(chunk.subarray(start, stop), { stream: true });
        line += terminated ? text.slice(0, -1) : text;
      } catch {
        throw new EventStreamProtocolError('Invalid SSE UTF-8');
      }
      if (terminated) {
        if (chunk[end] === 13) cr = true;
        else endLine();
      }
      start = stop;
    }
  };
}

export interface EventStreamOptions {
  signal: AbortSignal;
  sessionSignal: AbortSignal;
  client: string;
  onReset: () => void;
  cursors: { events: number; trace: number; resume?: string };
  filters: TraceFilters;
  onEvents: (page: EventPage) => void;
  onTrace: (page: TracePage) => void;
  onInterruption: (error: unknown) => void;
  request?: typeof fetch;
  timeoutMs?: number;
  retryMs?: number;
}

/** One query owns this lifetime; only callbacks that return successfully commit cursors. */
export async function runEventStream(options: EventStreamOptions): Promise<never> {
  const { signal, sessionSignal, cursors, filters } = options;
  const lifetime = new AbortController();
  const abort = () => lifetime.abort(new DOMException('Cancelled', 'AbortError'));
  signal.addEventListener('abort', abort, { once: true });
  sessionSignal.addEventListener('abort', abort, { once: true });
  if (signal.aborted || sessionSignal.aborted) abort();
  let attempts = 0;
  try {
    for (;;) {
      lifetime.signal.throwIfAborted();
      const connection = new AbortController();
      const cancel = () => connection.abort(lifetime.signal.reason);
      lifetime.signal.addEventListener('abort', cancel, { once: true });
      let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
      const wait = async <T>(operation: Promise<T>, timeout: number): Promise<T> => {
        let timer: ReturnType<typeof setTimeout> | undefined;
        let rejectAbort!: () => void;
        const cancelled = new Promise<never>((_, reject) => {
          rejectAbort = () => reject(connection.signal.reason);
          connection.signal.addEventListener('abort', rejectAbort, { once: true });
          timer = setTimeout(() => connection.abort(new Error('Event stream timed out')), timeout);
          if (connection.signal.aborted) rejectAbort();
        });
        try {
          return await Promise.race([operation, cancelled]);
        } finally {
          clearTimeout(timer);
          connection.signal.removeEventListener('abort', rejectAbort);
        }
      };
      try {
        // Continuity proofs are not authentication. This API also works over HTTP.
        const attempt = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
          byte.toString(16).padStart(2, '0'),
        ).join('');
        const trace = filters.active && !filters.error;
        const params = new URLSearchParams({
          client: options.client,
          attempt,
          after: String(cursors.events),
          trace_after: String(cursors.trace),
          trace: String(trace),
        });
        if (cursors.resume !== undefined) params.set('resume', cursors.resume);
        if (trace) {
          if (filters.endpoint !== undefined) params.set('endpoint', String(filters.endpoint));
          if (filters.direction) params.set('direction', filters.direction);
          if (filters.kind) params.set('kind', filters.kind);
          if (filters.errors) params.set('errors', 'true');
          if (filters.q) params.set('q', filters.q);
        }
        const response = await wait(
          (options.request ?? fetch)(`/api/events?${params}`, {
            signal: connection.signal,
            cache: 'no-store',
            headers: { Accept: 'text/event-stream' },
          }),
          options.timeoutMs ?? 8000,
        );
        if (!response.ok) throw new Error(`Events failed: HTTP ${response.status}`);
        if (
          response.headers.get('content-type')?.split(';')[0].trim() !== 'text/event-stream' ||
          !response.body
        )
          throw new EventStreamProtocolError('Invalid event stream response');
        reader = response.body.getReader();
        const parse = createSseParser((event, data) => {
          lifetime.signal.throwIfAborted();
          if (event !== 'events' && event !== 'trace' && event !== 'reset') return;
          let page: unknown;
          try {
            page = JSON.parse(data);
          } catch {
            throw new EventStreamProtocolError('Invalid SSE JSON');
          }
          if (event === 'reset') {
            if (
              typeof page !== 'object' ||
              page === null ||
              Array.isArray(page) ||
              Object.keys(page).length !== 1 ||
              !('resume' in page) ||
              typeof page.resume !== 'string' ||
              !/^[0-9a-f]{32}$/.test(page.resume) ||
              page.resume !== attempt
            )
              throw new EventStreamProtocolError('Invalid reset response');
            options.onReset();
            lifetime.signal.throwIfAborted();
            cursors.resume = attempt;
            cursors.events = 0;
            cursors.trace = 0;
          } else if (event === 'events') {
            if (!validEventPage(page))
              throw new EventStreamProtocolError('Invalid events response');
            if (
              page.records.some(
                (record) =>
                  record.event.type === 'hello' &&
                  record.event.protocol_version !== PROTOCOL_VERSION,
              )
            )
              throw new EventStreamProtocolError(
                'Board software is incompatible with this app. Update the board and app together.',
              );
            options.onEvents(page);
            cursors.events = page.cursor;
            attempts = 0;
          } else {
            if (!trace || !validTracePage(page))
              throw new EventStreamProtocolError('Invalid trace response');
            options.onTrace(page);
            cursors.trace = page.cursor;
          }
        });
        for (;;) {
          const result = await wait(reader.read(), options.timeoutMs ?? 8000);
          lifetime.signal.throwIfAborted();
          if (result.done) throw new Error('Event stream ended');
          parse(result.value);
        }
      } catch (error) {
        lifetime.signal.throwIfAborted();
        options.onInterruption(error);
        if (error instanceof EventStreamProtocolError) throw error;
      } finally {
        connection.abort();
        void reader?.cancel().catch(() => {});
        lifetime.signal.removeEventListener('abort', cancel);
      }
      await new Promise<void>((resolve, reject) => {
        const cancelSleep = () => {
          clearTimeout(timer);
          reject(lifetime.signal.reason);
        };
        const timer = setTimeout(
          () => {
            lifetime.signal.removeEventListener('abort', cancelSleep);
            resolve();
          },
          Math.min(5000, (options.retryMs ?? 250) * 2 ** Math.min(attempts++, 5)),
        );
        lifetime.signal.addEventListener('abort', cancelSleep, { once: true });
        if (lifetime.signal.aborted) cancelSleep();
      });
    }
  } finally {
    signal.removeEventListener('abort', abort);
    sessionSignal.removeEventListener('abort', abort);
  }
}
