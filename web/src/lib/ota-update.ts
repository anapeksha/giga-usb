import { parseOtaHeader, type OtaHeader } from './ota-format';

const phases = [
  'checking',
  'idle',
  'authorized',
  'quiescing',
  'receiving',
  'verifying',
  'ready',
  'committing',
  'armed',
  'cancelled',
  'fault',
] as const;
export interface UpdateStatus {
  build_id: string;
  phase: (typeof phases)[number];
  token: number | null;
  expires_ms: number;
  offset: number;
  total: number;
  confirmed: boolean;
  maintenance: boolean;
  busy: boolean;
  cancel_requested: boolean;
  message: string;
}
const uint = (v: unknown, max: number): v is number =>
  typeof v === 'number' && Number.isInteger(v) && v >= 0 && v <= max;
export function parseUpdateStatus(value: unknown): UpdateStatus {
  if (!value || typeof value !== 'object') throw new Error('Invalid update status');
  const s = value as UpdateStatus;
  if (
    Object.keys(s).sort().join() !==
      'build_id,busy,cancel_requested,confirmed,expires_ms,maintenance,message,offset,phase,token,total' ||
    typeof s.build_id !== 'string' ||
    !/^[A-Za-z0-9._-]{1,48}$/.test(s.build_id) ||
    !phases.includes(s.phase) ||
    !(s.token === null || uint(s.token, 0xffffffff)) ||
    !uint(s.expires_ms, 2 ** 64 - 1) ||
    !uint(s.offset, 0xffffffff) ||
    !uint(s.total, 0xffffffff) ||
    !['confirmed', 'maintenance', 'busy', 'cancel_requested'].every(
      (k) => typeof s[k as keyof UpdateStatus] === 'boolean',
    ) ||
    typeof s.message !== 'string'
  )
    throw new Error('Invalid update status');
  return s;
}

export async function bounded<T>(
  signal: AbortSignal,
  ms: number,
  work: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const controller = new AbortController();
  let reject!: (error: unknown) => void;
  const stopped = new Promise<never>((_, fail) => {
    reject = fail;
  });
  const abort = () => {
    controller.abort(signal.reason);
    reject(signal.reason);
  };
  signal.addEventListener('abort', abort, { once: true });
  const timer = setTimeout(() => {
    const error = new Error('Update timed out. Reconnect to GigaUSB and check status.');
    controller.abort(error);
    reject(error);
  }, ms);
  try {
    signal.throwIfAborted();
    return await Promise.race([stopped, work(controller.signal)]);
  } finally {
    clearTimeout(timer);
    signal.removeEventListener('abort', abort);
  }
}
export function pause(signal: AbortSignal, ms: number): Promise<void> {
  return new Promise((resolve, reject) => {
    signal.throwIfAborted();
    const abort = () => {
      clearTimeout(timer);
      reject(signal.reason);
    };
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', abort);
      resolve();
    }, ms);
    signal.addEventListener('abort', abort, { once: true });
  });
}
export async function inspectOta(file: Blob): Promise<OtaHeader> {
  if (file.size > 256 + 0x1e0000 + 0x100000 || file.size < 256)
    throw new Error('Invalid OTA file size');
  const header = parseOtaHeader(new Uint8Array(await file.slice(0, 256).arrayBuffer()));
  if (header.totalLength !== file.size) throw new Error('OTA file length does not match manifest');
  return header;
}
class UpdateRejected extends Error {}

export function createUpdateClient(
  onStatus: (status: UpdateStatus) => void,
  request: typeof fetch = fetch,
) {
  let token: number | null = null;
  let candidate: OtaHeader | undefined;

  const status = (signal: AbortSignal) =>
    bounded(signal, 8000, async (signal) => {
      const response = await request('/api/update/status', { signal, cache: 'no-store' });
      if (response.status === 404)
        throw new Error('OTA requires bootstrap firmware. Install it over SWD first.');
      if (!response.ok) throw new Error(`Update status: HTTP ${response.status}`);
      const reader = response.body?.getReader();
      if (!reader) throw new Error('Missing update status');
      const bytes: number[] = [];
      try {
        for (;;) {
          const { done, value } = await reader.read();
          if (done) break;
          if (bytes.length + value.length > 512) throw new Error('Update status exceeds 512 bytes');
          bytes.push(...value);
        }
      } finally {
        void reader.cancel().catch(() => {});
      }
      signal.throwIfAborted();
      const s = parseUpdateStatus(
        JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes))),
      );
      onStatus(s);
      return s;
    });
  const post = (
    path: string,
    signal: AbortSignal,
    body: Blob = new Blob(),
    offset?: number,
    actionToken = token,
  ) =>
    bounded(signal, 8000, async (signal) => {
      const headers: Record<string, string> = {
        'X-GigaUSB-Update': '1',
        'Content-Type': 'application/octet-stream',
      };
      if (actionToken !== null) headers['X-GigaUSB-Token'] = String(actionToken);
      if (offset !== undefined) headers['X-GigaUSB-Offset'] = String(offset);
      const response = await request(`/api/update/${path}`, {
        method: 'POST',
        headers,
        body,
        signal,
      });
      if (response.status !== 202)
        throw new UpdateRejected(
          `Update ${path}: HTTP ${response.status}. Check status before trying again.`,
        );
    });
  const wait = (signal: AbortSignal, accept: (s: UpdateStatus) => boolean, sameToken = true) =>
    bounded(signal, 120000, async (signal) => {
      for (;;) {
        const s = await status(signal);
        if (sameToken && s.token !== token)
          throw new Error('Update authorization changed. Check status.');
        if (s.phase === 'fault') throw new Error(s.message || 'Board rejected update');
        if (accept(s)) return s;
        await pause(signal, 500);
      }
    });
  return {
    status,
    async upload(file: Blob, signal: AbortSignal) {
      candidate = await inspectOta(file);
      return bounded(signal, 600000, async (signal) => {
        token = null;
        await post('authorize', signal);
        const authorized = await wait(
          signal,
          (s) => s.phase === 'authorized' && !s.busy && s.token !== null,
          false,
        );
        token = authorized.token;

        await post('begin', signal, file.slice(0, 256));
        await wait(
          signal,
          (s) => s.phase === 'receiving' && !s.busy && s.offset === 256 && s.total === file.size,
        );
        for (let offset = 256; offset < file.size;) {
          const end = Math.min(offset + 4096, file.size);
          // Never retry a POST: a lost response may still have staged bytes.
          await post('chunk', signal, file.slice(offset, end), offset);
          await wait(
            signal,
            (s) =>
              !s.busy &&
              s.offset === end &&
              s.total === file.size &&
              (s.phase === 'receiving' || (end === file.size && s.phase === 'ready')),
          );
          offset = end;
        }
        return wait(signal, (s) => s.phase === 'ready' && !s.busy && s.offset === file.size);
      });
    },
    async cancel(signal: AbortSignal) {
      const current = await status(signal);
      if (
        current.token === null ||
        !['authorized', 'quiescing', 'receiving', 'verifying', 'ready', 'fault'].includes(
          current.phase,
        )
      )
        throw new Error('Update cannot be cancelled in its current state. Check status.');
      const cancelToken = current.token;
      await post('cancel', signal, new Blob(), undefined, cancelToken);
      return wait(
        signal,
        (s) => {
          if (s.phase === 'cancelled' && s.token === null && !s.busy && !s.cancel_requested)
            return true;
          if (s.token !== cancelToken)
            throw new Error('Update authorization changed. Check status.');
          return false;
        },
        false,
      );
    },
    async install(signal: AbortSignal) {
      if (!candidate) throw new Error('Upload an OTA file first');
      const ready = await status(signal);
      if (ready.token !== token || ready.phase !== 'ready' || ready.busy)
        throw new Error('Update is not ready');
      try {
        await post('commit', signal);
      } catch (error) {
        signal.throwIfAborted();
        if (error instanceof UpdateRejected) throw error;
        // Acceptance is uncertain: reconcile with GETs only, never resend commit.
      }
      try {
        return await bounded(signal, 300000, async (signal) => {
          let delay = 250;
          for (;;) {
            let s: UpdateStatus;
            try {
              s = await status(signal);
            } catch (error) {
              signal.throwIfAborted();
              if (error instanceof Error && /Invalid|512|bootstrap/.test(error.message))
                throw error;

              await pause(signal, delay);
              delay = Math.min(2000, delay * 2);
              continue;
            }
            if (s.phase === 'fault') throw new Error(s.message || 'Installation failed');

            if (
              s.build_id === candidate!.buildId &&
              s.confirmed &&
              !['ready', 'committing', 'armed'].includes(s.phase)
            )
              return s;

            await pause(signal, 500);
          }
        });
      } catch (error) {
        signal.throwIfAborted();
        if (error instanceof Error && error.message.startsWith('Update timed out.'))
          throw new Error('Candidate not confirmed. Reconnect to GigaUSB and check status.', {
            cause: error,
          });
        throw error;
      }
    },
  };
}
