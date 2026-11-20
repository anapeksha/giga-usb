import { test } from 'vitest';
import assert from 'node:assert/strict';
import { getTracePage, normalizeTraceFilters, validTracePage } from './http';
import type { ProtocolRecord } from './protocol';
import { fixtureTracePage } from '../e2e/trace-query-fixture';

const input = { endpoint: '', direction: '', kind: '', errors: false, q: '' };
const record: ProtocolRecord = {
  timestamp_ms: 1,
  event: {
    type: 'host',
    event: {
      transfer: {
        endpoint: 0,
        direction: 'out',
        pid: 3,
        bytes: 8,
        data: [128, 6, 0, 1, 0, 0, 18, 0],
      },
    },
  },
};
const context: ProtocolRecord = {
  timestamp_ms: 0,
  event: {
    type: 'host',
    event: {
      transfer_context: {
        session_id: 2,
        transfer_id: 1,
        endpoint: 0,
        kind: 'control',
        stage: 'setup',
        elapsed_us: 1,
        retries: 0,
      },
    },
  },
};

test('trace normalization preserves exact addresses and enforces 96 UTF-8 bytes', () => {
  assert.equal(normalizeTraceFilters(input).active, false);
  for (const endpoint of ['0', '128', '255'])
    assert.equal(normalizeTraceFilters({ ...input, endpoint }).endpoint, Number(endpoint));
  for (const endpoint of ['-1', '256', '1.5', '0x80', 'NaN'])
    assert.match(normalizeTraceFilters({ ...input, endpoint }).error, /endpoint/);
  assert.equal(normalizeTraceFilters({ ...input, q: ' é'.trim().repeat(48) }).error, '');
  assert.match(normalizeTraceFilters({ ...input, q: 'é'.repeat(49) }).error, /96 UTF-8/);
});

test('trace fetch serializes every filter and trusts valid server results rather than matching locally', async () => {
  const filters = normalizeTraceFilters({
    endpoint: ' 0 ',
    direction: 'OUT',
    kind: 'CONTROL',
    errors: true,
    q: ' unrelated ',
  });
  const result = await getTracePage(
    0xffffffff,
    filters,
    new AbortController().signal,
    async (url, options) => {
      assert.equal(
        url,
        '/api/trace?after=4294967295&endpoint=0&direction=out&kind=control&errors=true&q=unrelated',
      );
      assert.equal(options?.cache, 'no-store');
      return Response.json({ records: [{ sequence: 0, record }], cursor: 0, dropped: 4 });
    },
  );
  assert.equal(result.records[0].record.event.type, 'host');
  assert.equal(result.cursor, 0);
});

test('trace guard accepts wrap and rejects malformed, non-USB and oversized pages', () => {
  const page = {
    records: [
      { sequence: 0xffffffff, record },
      { sequence: 0, record },
    ],
    cursor: 0,
    dropped: 0,
  };
  assert.ok(validTracePage(page));
  for (const bad of [
    null,
    { ...page, cursor: -1 },
    { ...page, dropped: 2 ** 32 },
    { ...page, records: [{ sequence: -1, record }] },
    {
      ...page,
      records: [
        { sequence: 1, record: { timestamp_ms: 0, event: { type: 'hello', protocol_version: 1 } } },
      ],
    },
    { ...page, records: Array(4).fill(page.records[0]) },
  ])
    assert.equal(validTracePage(bad), false);
});

test('trace errors, deadlines including body reads, and external abort settle injected fetches', async () => {
  const filters = normalizeTraceFilters({ ...input, q: 'test' });
  for (const status of [400, 404])
    await assert.rejects(
      getTracePage(
        0,
        filters,
        new AbortController().signal,
        async () => new Response(null, { status }),
      ),
      new RegExp(`HTTP ${status}`),
    );
  for (const body of [false, true]) {
    let signal: AbortSignal | null | undefined;
    await assert.rejects(
      getTracePage(
        0,
        filters,
        new AbortController().signal,
        async (_, options) => {
          signal = options?.signal;
          if (!body) return new Promise<Response>(() => {});
          const response = Response.json({});
          response.json = () => new Promise(() => {});
          return response;
        },
        10,
      ),
      /timed out/,
    );
    assert.equal(signal?.aborted, true);
  }
  const controller = new AbortController();
  const reason = new Error('Filter changed');
  const pending = getTracePage(
    0,
    filters,
    controller.signal,
    async () => new Promise<Response>(() => {}),
  );
  controller.abort(reason);
  await assert.rejects(pending, (error) => error === reason);
});

test('fixture golden cases mirror Rust exact scope and paired search without whole-transfer association', () => {
  const history = [
    context,
    record,
    context,
    { timestamp_ms: 3, event: { type: 'hello', protocol_version: 1 } } as ProtocolRecord,
    record,
  ];
  for (const q of [
    'get_descriptor',
    'GET DESCRIPTOR',
    '8006000100001200',
    '80 06 00 01',
    'sent endpoint 0',
  ]) {
    const page = fixtureTracePage(
      history,
      new URLSearchParams({ endpoint: '0', direction: 'out', kind: 'control', q }),
    );
    assert.deepEqual(
      page?.records.map((r) => r.sequence),
      [1, 2],
    );
    assert.equal(page?.cursor, 5);
  }
  assert.deepEqual(
    fixtureTracePage(history, new URLSearchParams({ endpoint: '128' }))?.records,
    [],
  );
  assert.equal(fixtureTracePage(history, new URLSearchParams({ endpoint: '256' })), null);
});

test('fixture retention counts original records, pagination scans past nonmatches without counting them as drops', () => {
  const hello: ProtocolRecord = { timestamp_ms: 0, event: { type: 'hello', protocol_version: 1 } };
  const history = [...Array<ProtocolRecord>(68).fill(hello), record];
  assert.deepEqual(fixtureTracePage(history, new URLSearchParams()), {
    records: [{ sequence: 69, record }],
    cursor: 69,
    dropped: 5,
  });
  assert.deepEqual(fixtureTracePage(history, new URLSearchParams({ q: 'absent' })), {
    records: [],
    cursor: 69,
    dropped: 5,
  });
  const page = fixtureTracePage([record, record, record, hello, record], new URLSearchParams());
  assert.equal(page?.cursor, 3);
  assert.deepEqual(
    fixtureTracePage(
      [record, record, record, hello, record],
      new URLSearchParams({ after: '3' }),
    )?.records.map((r) => r.sequence),
    [5],
  );
});
