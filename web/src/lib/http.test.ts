import type { ProtocolRecord } from './protocol';
import test from 'node:test';
import assert from 'node:assert/strict';
import { applyEventPage, createLossRefreshGuard, getEventPage, sendCommand } from './http';
import { createState } from './protocol';
const record: ProtocolRecord = { timestamp_ms: 0, event: { type: 'hello', protocol_version: 5 } };
const response = (page: unknown) => Response.json(page);

test('page fetch uses supplied cursor and leaves reduction to the session', async () => {
  const state = { ...createState(), transportDropped: 0 };
  const page = await getEventPage(0xffffffff, new AbortController().signal, async (url) => {
    assert.equal(url, '/api/events?after=4294967295');
    return response({ records: [record], cursor: 1, dropped: 8 });
  });
  assert.equal(state.history.length, 0);
  applyEventPage(state, page);
  assert.equal(state.history.length, 1);
  assert.equal(state.transportDropped, 8);
});

test('transport snapshots never overwrite authoritative firmware drops', () => {
  const state = { ...createState(), transportDropped: 0 };
  const status: ProtocolRecord = {
    timestamp_ms: 1,
    event: {
      type: 'host',
      event: {
        status: {
          enabled: true,
          attached: false,
          address: 0,
          transfers: 10,
          errors: 2,
          dropped: 12,
        },
      },
    },
  };
  applyEventPage(state, { records: [status], cursor: 1, dropped: 7 });
  assert.equal(state.dropped, 12);
  applyEventPage(state, {
    records: [{ timestamp_ms: 2, event: { type: 'dropped', count: 3 } }],
    cursor: 2,
    dropped: 7,
  });
  assert.equal(state.dropped, 12);
  assert.equal(state.queueDropped, 3);
  applyEventPage(state, { records: [], cursor: 2, dropped: 2 });
  assert.equal(state.transportDropped, 2);
  assert.equal(state.dropped, 12);
  applyEventPage(state, { records: [status], cursor: 3, dropped: 0 });
  assert.equal(state.dropped, 12);
  assert.equal(state.transportDropped, 0);
});

test('loss refresh guard detects increases, ignores persistent snapshots and rate limits feedback', () => {
  let time = 0;
  const refresh = createLossRefreshGuard(() => time);
  assert.equal(refresh(0), false);
  assert.equal(refresh(7), true);
  time = 6000;
  assert.equal(refresh(7), false);
  assert.equal(refresh(2), false);
  assert.equal(refresh(3), true);
  assert.equal(refresh(4), false);
  time = 12000;
  assert.equal(refresh(4), false); // No delayed retry of a suppressed increase.
  assert.equal(refresh(5), true);
  assert.equal(refresh(0), false);
  time = 18000;
  assert.equal(refresh(1), true);
});

test('rejects invalid envelopes and HTTP errors without delivering a page', async () => {
  for (const page of [
    null,
    { records: [record], cursor: -1, dropped: 0 },
    { records: [null], cursor: 1, dropped: 0 },
    { records: Array(4).fill(record), cursor: 1, dropped: 0 },
  ]) {
    await assert.rejects(
      getEventPage(0, new AbortController().signal, async () => response(page)),
      /Invalid events response/,
    );
  }
  await assert.rejects(
    getEventPage(0, new AbortController().signal, async () => new Response(null, { status: 500 })),
    /HTTP 500/,
  );
});

test('command and event fetch deadlines abort stalled injected requests', async () => {
  for (const kind of ['command', 'events']) {
    const controller = new AbortController();
    let requestSignal: AbortSignal | undefined;
    const stalled: typeof fetch = async (_, options) => {
      requestSignal = options?.signal ?? undefined;
      return new Promise<Response>(() => {});
    };
    const pending =
      kind === 'command'
        ? sendCommand('status', controller.signal, stalled, 10)
        : getEventPage(0, controller.signal, stalled, 10);
    await assert.rejects(pending, /Diagnostic request timed out/);
    assert.equal(requestSignal?.aborted, true);
    assert.equal(controller.signal.aborted, false);
  }
});

test('event deadline includes JSON body and never delivers a late page', async () => {
  let finish!: (page: unknown) => void;
  let delivered = 0;
  let requestSignal: AbortSignal | undefined;
  await assert.rejects(
    getEventPage(
      0,
      new AbortController().signal,
      async (_, options) => {
        requestSignal = options?.signal ?? undefined;
        const result = response({});
        result.json = () => new Promise((resolve) => (finish = resolve));
        return result;
      },
      10,
    ).then((page) => {
      delivered += page.records.length + 1;
    }),
    /Diagnostic request timed out/,
  );
  assert.equal(requestSignal?.aborted, true);
  finish({ records: [], cursor: 0, dropped: 0 });
  await new Promise<void>((resolve) => setImmediate(resolve));
  assert.equal(delivered, 0);
});

test('external cancellation settles stalled requests and preserves abort reason', async () => {
  for (const kind of ['command', 'events']) {
    const controller = new AbortController();
    const reason = new Error('Disconnected by user');
    let requestSignal: AbortSignal | undefined;
    let finish!: (response: Response) => void;
    const request: typeof fetch = async (_, options) => {
      requestSignal = options?.signal ?? undefined;
      return new Promise((resolve) => (finish = resolve));
    };
    const pending =
      kind === 'command'
        ? sendCommand('status', controller.signal, request, 1000)
        : getEventPage(0, controller.signal, request, 1000);
    await Promise.resolve();
    controller.abort(reason);
    await assert.rejects(pending, (error) => error === reason);
    assert.equal(requestSignal?.aborted, true);
    assert.equal(requestSignal?.reason, reason);
    finish(
      kind === 'command'
        ? new Response(null, { status: 202 })
        : response({ records: [], cursor: 0, dropped: 0 }),
    );
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
});

test('pre-aborted commands do not start requests', async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(
    sendCommand('status', controller.signal, async () => assert.fail('request started'), 10),
    { name: 'AbortError' },
  );
});

test('commands use JSON POST and require 202; queue full is actionable', async () => {
  const signal = new AbortController().signal;
  for (const command of [
    'start',
    'stop',
    'reset',
    'status',
    'storage_list',
    { storage_read: { file: 31, offset: 65536 } },
  ]) {
    await sendCommand(command, signal, async (url, options) => {
      assert.equal(url, '/api/command');
      assert.equal(options?.method, 'POST');
      assert.equal(new Headers(options?.headers).get('Content-Type'), 'application/json');
      assert.equal(options?.body, JSON.stringify({ command }));
      assert.ok(options?.signal instanceof AbortSignal);
      assert.equal(options.signal.aborted, false);
      return new Response(null, { status: 202 });
    });
  }
  await assert.rejects(sendCommand('erase', signal), /Unknown command/);
  await assert.rejects(
    sendCommand('start', signal, async () => new Response(null, { status: 503 })),
    /queue full/,
  );
  await assert.rejects(
    sendCommand('start', signal, async () => new Response(null, { status: 200 })),
    /HTTP 200/,
  );
});
