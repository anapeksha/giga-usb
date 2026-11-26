import { describe, expect, it, vi } from 'vitest';
import {
  bounded,
  createUpdateClient,
  inspectOta,
  parseUpdateStatus,
  pause,
  type UpdateStatus,
} from './ota-update';

function otaFile() {
  const bytes = new Uint8Array(256 + 0x1e0000 + 4096);
  bytes.set(new TextEncoder().encode('GIGAOTA1'));
  const view = new DataView(bytes.buffer);
  for (const [offset, value] of [
    [8, 1],
    [12, 256],
    [16, 1],
    [20, 10],
    [24, 0x08020000],
    [28, 0x1e0000],
    [32, 0x08100000],
    [36, 4096],
    [104, 0x24080000],
    [108, 0x08020101],
    [112, 0x30020000],
    [116, 0x08100101],
  ])
    view.setUint32(offset!, value!, true);
  bytes.set(new TextEncoder().encode('candidate'), 120);
  return new Blob([bytes]);
}

async function uploaded(commit: () => Promise<Response>, reads: () => UpdateStatus | Error) {
  const file = otaFile();
  let state: UpdateStatus = { ...status, phase: 'idle', confirmed: true };
  let committed = false;
  const request = vi.fn<typeof fetch>(async (url, init) => {
    const path = String(url).split('/').at(-1);
    if (init?.method !== 'POST') {
      const next = committed ? reads() : state;
      if (next instanceof Error) throw next;
      return Response.json(next);
    }
    if (path === 'authorize') state = { ...state, phase: 'authorized', token: 7 };
    if (path === 'begin') state = { ...state, phase: 'receiving', offset: 256, total: file.size };
    if (path === 'chunk') {
      const offset = Number((init.headers as Record<string, string>)['X-GigaUSB-Offset']);
      const end = offset + (init.body as Blob).size;
      state = { ...state, offset: end, phase: end === file.size ? 'ready' : 'receiving' };
    }
    if (path === 'commit') {
      committed = true;
      return commit();
    }
    return new Response(null, { status: 202 });
  });
  const client = createUpdateClient(vi.fn(), request);
  await client.upload(file, new AbortController().signal);
  return { client, request };
}

const status = {
  build_id: 'test-1',
  phase: 'checking',
  token: null,
  expires_ms: 0,
  offset: 0,
  total: 0,
  confirmed: false,
  maintenance: false,
  busy: false,
  cancel_requested: false,
  message: '',
};
describe('OTA client', () => {
  it('fresh cancellation binds only the explicit action token and accepts firmware null-token completion', async () => {
    let gets = 0;
    const request = vi.fn<typeof fetch>(async (_, init) => {
      if (init?.method === 'POST') {
        expect((init.headers as Record<string, string>)['X-GigaUSB-Token']).toBe('9');
        return new Response(null, { status: 202 });
      }
      gets++;
      return Response.json({
        ...status,
        phase: gets === 1 ? 'authorized' : gets === 2 ? 'receiving' : 'cancelled',
        token: gets === 1 ? 7 : gets === 2 ? 9 : null,
      });
    });
    const client = createUpdateClient(vi.fn(), request);
    await client.status(new AbortController().signal);
    expect(request.mock.calls.every(([, init]) => init?.method !== 'POST')).toBe(true);
    await expect(client.cancel(new AbortController().signal)).resolves.toMatchObject({
      phase: 'cancelled',
      token: null,
    });
    expect(request.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(1);
  });
  it('does not cancel when fresh status has become armed', async () => {
    const request = vi
      .fn<typeof fetch>()
      .mockResolvedValue(Response.json({ ...status, phase: 'armed', token: 7 }));
    await expect(
      createUpdateClient(vi.fn(), request).cancel(new AbortController().signal),
    ).rejects.toThrow('cannot be cancelled');
    expect(request).toHaveBeenCalledTimes(1);
  });
  it('reconciles lost commit acceptance without resending and tolerates old confirmed status after a hiccup', async () => {
    let reads = 0;
    const { client, request } = await uploaded(
      async () => {
        throw new TypeError('Connection lost');
      },
      () => {
        reads++;
        if (reads === 1) return new TypeError('Temporary status failure');
        return {
          ...status,
          phase: 'idle',
          confirmed: true,
          build_id: reads === 2 ? 'test-1' : 'candidate',
        };
      },
    );
    await expect(client.install(new AbortController().signal)).resolves.toMatchObject({
      build_id: 'candidate',
      confirmed: true,
    });
    expect(request.mock.calls.filter(([url]) => String(url).endsWith('/commit'))).toHaveLength(1);
  });
  it('reconciles a commit request deadline without retrying the POST', async () => {
    const { client, request } = await uploaded(
      () => new Promise(() => {}),
      () => ({ ...status, phase: 'idle', confirmed: true, build_id: 'candidate' }),
    );
    vi.useFakeTimers();
    try {
      const result = client.install(new AbortController().signal);
      await vi.advanceTimersByTimeAsync(8001);
      await expect(result).resolves.toMatchObject({ build_id: 'candidate', confirmed: true });
      expect(request.mock.calls.filter(([url]) => String(url).endsWith('/commit'))).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });
  it('stops on definite HTTP commit rejection without monitoring or retrying', async () => {
    const reads = vi.fn(() => ({ ...status }) as UpdateStatus);
    const { client, request } = await uploaded(
      async () => new Response(null, { status: 409 }),
      reads,
    );
    await expect(client.install(new AbortController().signal)).rejects.toThrow('HTTP 409');
    expect(reads).not.toHaveBeenCalled();
    expect(request.mock.calls.filter(([url]) => String(url).endsWith('/commit'))).toHaveLength(1);
  });
  it('keeps old confirmed builds inconclusive until the bounded confirmation deadline', async () => {
    const { client } = await uploaded(
      async () => new Response(null, { status: 202 }),
      () => ({ ...status, phase: 'idle', confirmed: true }),
    );
    vi.useFakeTimers();
    try {
      const result = expect(client.install(new AbortController().signal)).rejects.toThrow(
        'Candidate not confirmed. Reconnect',
      );
      await vi.advanceTimersByTimeAsync(300001);
      await result;
    } finally {
      vi.useRealTimers();
    }
  });
  it('honors external abort after an ambiguous commit rather than monitoring', async () => {
    const controller = new AbortController();
    const reads = vi.fn(() => ({ ...status }) as UpdateStatus);
    const { client } = await uploaded(async () => {
      controller.abort(new Error('User disconnected'));
      throw new TypeError('Lost');
    }, reads);
    await expect(client.install(controller.signal)).rejects.toThrow('User disconnected');
    expect(reads).not.toHaveBeenCalled();
  });
  it('accepts initial unconfirmed checking status', () => {
    expect(parseUpdateStatus(status).confirmed).toBe(false);
  });
  it.each([
    { phase: 'done' },
    { token: '1' },
    { offset: -1 },
    { confirmed: 1 },
    { build_id: '<script>' },
    { extra: true },
    { expires_ms: -1 },
  ])('rejects malformed status %j', (patch) => {
    expect(() => parseUpdateStatus({ ...status, ...patch })).toThrow('Invalid update status');
  });
  it('rejects oversized and truncated files before any request', async () => {
    await expect(inspectOta(new Blob([new Uint8Array(255)]))).rejects.toThrow('size');
    await expect(inspectOta(new Blob([new Uint8Array(3 * 1024 * 1024)]))).rejects.toThrow('size');
  });
  it('does not request anything until explicitly called and explains bootstrap', async () => {
    const request = vi.fn<typeof fetch>().mockResolvedValue(new Response('', { status: 404 }));
    const client = createUpdateClient(vi.fn(), request);
    expect(request).not.toHaveBeenCalled();
    await expect(client.status(new AbortController().signal)).rejects.toThrow('bootstrap');
    expect(request).toHaveBeenCalledTimes(1);
  });
  it('bounds status bytes', async () => {
    const client = createUpdateClient(
      vi.fn(),
      vi.fn<typeof fetch>().mockResolvedValue(new Response(' '.repeat(513))),
    );
    await expect(client.status(new AbortController().signal)).rejects.toThrow('512');
  });
  it('aborts a stalled body even when reader ignores fetch abort', async () => {
    const controller = new AbortController();
    const client = createUpdateClient(
      vi.fn(),
      vi.fn<typeof fetch>().mockResolvedValue(new Response(new ReadableStream({ start() {} }))),
    );
    const result = client.status(controller.signal);
    controller.abort(new Error('Disconnected'));
    await expect(result).rejects.toThrow('Disconnected');
  });
  it('enforces deadlines even for operations ignoring abort', async () => {
    await expect(
      bounded(new AbortController().signal, 1, () => new Promise(() => {})),
    ).rejects.toThrow('timed out');
  });
  it('cancels backoff immediately', async () => {
    const controller = new AbortController();
    const result = pause(controller.signal, 10000);
    controller.abort(new Error('Cancelled'));
    await expect(result).rejects.toThrow('Cancelled');
  });
});
