import { test } from 'vitest';
import assert from 'node:assert/strict';
import {
  createState,
  applyRecord,
  HISTORY_LIMIT,
  validRecord,
  PROTOCOL_VERSION,
  summary,
} from './protocol';
const record = (event: unknown) => ({ timestamp_ms: 42, event });
test('protocol version is 9', () => {
  assert.equal(PROTOCOL_VERSION, 9);
  const golden = JSON.parse('{"timestamp_ms":42,"event":{"type":"hello","protocol_version":9}}');
  const state = createState();
  assert.equal(applyRecord(state, golden), true);
  assert.equal(state.protocol, 9);
});

test('network JSON golden retains diagnostics without changing USB state', () => {
  const golden = JSON.parse(
    '{"timestamp_ms":42,"event":{"type":"host","event":{"network":{"info":{"mode":"connected","mac":[0,1,2,253,254,255],"ip":"192.168.4.1","error":null}}}}}',
  );
  const state = createState();
  assert.equal(state.network, undefined);
  applyRecord(state, record({ type: 'host', event: { error: { code: 'stall' } } }));
  applyRecord(state, record({ type: 'dropped', count: 3 }));
  const before = { ...state, history: [] };
  assert.equal(applyRecord(state, golden), true);
  assert.deepEqual(state.network, golden.event.event.network.info);
  assert.deepEqual(
    { ...state, recordSequence: 0, history: [], network: undefined },
    { ...before, recordSequence: 0, network: undefined },
  );
  assert.equal(state.history.length, 1);
  assert.equal(summary(golden), 'Network · connected · 192.168.4.1');

  for (const mode of ['setup', 'associating', 'link_wait', 'dhcp', 'connected', 'error']) {
    const info = { mode, mac: [0, 1, 2, 3, 4, 5], ip: null, error: 'é'.repeat(32) };
    assert.equal(applyRecord(state, record({ type: 'host', event: { network: { info } } })), true);
    assert.deepEqual(state.network, info);
    assert.equal(state.errors, 1);
    assert.equal(state.queueDropped, 3);
    assert.equal(state.dropped, 0);
  }
  const info = { mode: 'connected', mac: [0, 1, 2, 3, 4, 5], ip: '255.255.255.255', error: null };
  assert.equal(validRecord(record({ type: 'host', event: { network: { info } } })), true);
});

test('malformed network diagnostics are rejected without state mutation', () => {
  const info = { mode: 'setup', mac: [0, 1, 2, 3, 4, 5], ip: null, error: null };
  const invalid = [
    null,
    [],
    info,
    { info: null },
    { info: [] },
    ...[
      { mode: 'unknown' },
      { mode: 'connected-too-long' },
      { mode: null },
      { mode: undefined },
      { mac: undefined },
      { mac: '00:01:02:03:04:05' },
      { mac: [0, 1, 2, 3, 4] },
      { mac: [0, 1, 2, 3, 4, 5, 6] },
      { mac: [0, 1, 2, 3, 4, 256] },
      { mac: [0, 1, 2, 3, 4, -1] },
      { mac: [0, 1, 2, 3, 4, 1.5] },
      { mac: [0, 1, 2, 3, 4, '5'] },
      { ip: undefined },
      { ip: 123 },
      { ip: '1'.repeat(16) },
      { ip: 'é'.repeat(8) },
      { error: undefined },
      { error: false },
      { error: 'x'.repeat(65) },
      { error: 'é'.repeat(33) },
    ].map((patch) => ({ info: { ...info, ...patch } })),
  ];
  const state = createState();
  for (const network of invalid) {
    const candidate = record({ type: 'host', event: { network } });
    assert.equal(validRecord(candidate), false);
    assert.equal(applyRecord(state, candidate), false);
    assert.deepEqual(state, createState());
  }
});
test('externally tagged host variants and safe state updates', () => {
  const s = createState();
  const apply = (e: unknown) =>
    assert.equal(applyRecord(s, record({ type: 'host', event: e })), true);
  apply('ready');
  apply({ attached: { speed: 'full' } });
  apply({ configured: { address: 2, configuration: 1 } });
  apply({
    transfer: { endpoint: 129, direction: 'in', bytes: 8, pid: 0, data: [0, 1, 2, 3, 4, 5, 6, 7] },
  });
  apply({ error: { code: 'stall' } });
  applyRecord(s, record({ type: 'dropped', count: 3 }));
  applyRecord(s, record({ type: 'dropped', count: 2 }));
  assert.equal(s.transfers, 1);
  assert.equal(s.errors, 1);
  assert.equal(s.queueDropped, 5);
  apply({
    status: { enabled: true, attached: true, address: 2, transfers: 20, errors: 4, dropped: 5 },
  });
  assert.equal(s.transfers, 20);
  assert.equal(s.dropped, 5);
  applyRecord(s, record({ type: 'dropped', count: 1 }));
  assert.equal(s.queueDropped, 6);
  assert.equal(s.dropped, 5);
  const before = JSON.stringify(s);
  assert.equal(
    applyRecord(s, record({ type: 'host', event: { status: { attached: 'yes' } } })),
    false,
  );
  assert.equal(
    applyRecord(s, record({ type: 'host', event: { type: 'attached', speed: 'full' } })),
    false,
  );
  assert.equal(JSON.stringify(s), before);
  apply('detached');
  assert.equal(s.attached, false);
  assert.equal(s.address, 2);
});
test('device, HID, interface and endpoint state; detach retains partial descriptors', () => {
  const s = createState();
  const device = {
    usb_version: 512,
    vendor_id: 0x1209,
    product_id: 1,
    device_version: 256,
    class: 0,
    subclass: 0,
    protocol: 0,
    max_packet_size: 64,
    configurations: 1,
  };
  applyRecord(s, record({ type: 'device', descriptor: device }));
  const descriptor = { number: 0, alternate: 0, class: 3, subclass: 1, protocol: 1, endpoints: 1 };
  applyRecord(s, record({ type: 'interface', descriptor }));
  applyRecord(s, record({ type: 'interface', descriptor: { ...descriptor, protocol: 2 } }));
  applyRecord(
    s,
    record({
      type: 'endpoint',
      descriptor: {
        interface: 0,
        alternate: 0,
        address: 129,
        attributes: 3,
        max_packet_size: 8,
        interval: 10,
      },
    }),
  );
  applyRecord(s, record({ type: 'keyboard', modifiers: 1, keys: [4], pressed: [4], released: [] }));
  applyRecord(s, record({ type: 'mouse', buttons: 1, dx: -1, dy: 2, wheel: 0 }));
  assert.equal(s.interfaces.length, 1);
  assert.equal(s.interfaces[0].protocol, 2);
  assert.equal(s.device?.vendor_id, 0x1209);
  assert.equal(s.keyboard?.keys[0], 4);
  applyRecord(s, record({ type: 'host', event: 'detached' }));
  assert.equal(s.device?.vendor_id, 0x1209);
  assert.equal(s.endpoints.length, 1);
  assert.equal(s.keyboard?.keys[0], 4);
});
test('raw byte host records validate without accepting invalid byte arrays', () => {
  for (const event of [
    { hid_report: { endpoint: 129, data: [0, 255] } },
    { descriptor: { descriptor_type: 1, index: 0, offset: 0, total_length: 18, data: [18, 1] } },
  ]) {
    assert.equal(validRecord(record({ type: 'host', event })), true);
  }
  assert.equal(
    validRecord(record({ type: 'host', event: { hid_report: { endpoint: 1, data: [256] } } })),
    false,
  );
  for (const invalid of [
    null,
    [],
    42,
    { timestamp_ms: 0, event: null },
    record({ type: 'mouse', buttons: 0, dx: -129, dy: 0, wheel: 0 }),
    record({ type: 'host', event: { error: { code: 'unknown' } } }),
  ]) {
    assert.equal(validRecord(invalid), false);
    const state = createState();
    assert.equal(applyRecord(state, invalid), false);
    assert.deepEqual(state, createState());
  }
});
test('protocol 6 raw setup golden and complete bounded packets', () => {
  const setup = JSON.parse(
    '{"timestamp_ms":123,"event":{"type":"host","event":{"transfer":{"endpoint":0,"direction":"out","bytes":8,"pid":3,"data":[128,6,0,1,0,0,18,0]}}}}',
  );
  assert.equal(validRecord(setup), true);
  const state = createState();
  assert.equal(applyRecord(state, setup), true);
  for (const transfer of [
    { endpoint: 128, direction: 'in', bytes: 64, pid: 0, data: Array(64).fill(255) },
    { endpoint: 129, direction: 'in', bytes: 8, pid: 2, data: [0, 0, 4, 0, 0, 0, 0, 0] },
    { endpoint: 0, direction: 'out', bytes: 0, pid: 2, data: [] },
    { endpoint: 128, direction: 'in', bytes: 0, pid: 2, data: [] },
  ]) {
    const packet = record({ type: 'host', event: { transfer } });
    assert.equal(applyRecord(state, packet), true);
    assert.ok(Buffer.byteLength(JSON.stringify(packet)) <= 1024);
    assert.deepEqual(state.history.at(-1), { ...packet, sequence: state.recordSequence });
  }
  assert.equal(state.transfers, 5);
  assert.equal(state.keyboard, null); // Raw packets do not duplicate semantic HID decoding.
  const valid = { endpoint: 128, direction: 'in', bytes: 1, pid: 2, data: [255] };
  for (const patch of [
    { data: undefined },
    { pid: undefined },
    { pid: 1 },
    { pid: 256 },
    { bytes: 2 },
    { bytes: 65, data: Array(65).fill(0) },
    { data: [-1] },
    { data: [256] },
    { data: [1.5] },
    { endpoint: 256 },
    { endpoint: 144 },
    { direction: 'out' },
    { pid: 3 },
    { endpoint: 0, direction: 'out', pid: 3 },
  ]) {
    const before = JSON.stringify(state);
    assert.equal(
      applyRecord(state, record({ type: 'host', event: { transfer: { ...valid, ...patch } } })),
      false,
    );
    assert.equal(JSON.stringify(state), before);
  }
});

test('generic IPC errors preserve USB counts, diagnostics and lifecycle without entering exports', () => {
  const state = createState();
  const host = (event: unknown) => applyRecord(state, record({ type: 'host', event }));
  host({ lifecycle: { state: 'error', session_id: 1 } });
  const diagnostic = {
    code: 'timeout',
    setup: [0, 0, 0, 0, 0, 0, 0, 0],
    session_id: 1,
    transfer_id: 2,
    stage: 'data',
    endpoint: 129,
    elapsed_us: 1000,
    retries: 2,
    hcint: 1,
    hcchar: 2,
    hctsiz: 3,
    hprt: 4,
  };
  assert.equal(host({ diagnostic }), true);
  host({
    status: { enabled: true, attached: true, address: 1, transfers: 5, errors: 1, dropped: 0 },
  });
  const before = structuredClone(state);
  assert.equal(
    applyRecord(state, record({ type: 'error', message: 'IPC transport failed' })),
    true,
  );
  assert.deepEqual(state, {
    ...before,
    recordSequence: before.recordSequence + 1,
    warnings: 1,
  });
  assert.deepEqual(JSON.parse(JSON.stringify(state.history)), before.history);
  host({ error: { code: 'transaction' } });
  assert.equal(state.errors, 2);
  assert.deepEqual(state.diagnostic, diagnostic);
  host({
    status: { enabled: true, attached: true, address: 1, transfers: 5, errors: 1, dropped: 0 },
  });
  assert.equal(state.errors, 1);
  assert.equal(state.warnings, 1);
});

test('history is a bounded 500-record ring and fresh sessions are empty', () => {
  const s = createState();
  for (let i = 0; i < 700; i++)
    applyRecord(s, {
      timestamp_ms: i,
      event: { type: 'host', event: { error: { code: 'stall' } } },
    });
  assert.equal(s.history.length, HISTORY_LIMIT);
  assert.equal(s.history[0].timestamp_ms, 200);
  assert.equal(s.history.at(-1)?.timestamp_ms, 699);
  assert.equal(createState().history.length, 0);
});
