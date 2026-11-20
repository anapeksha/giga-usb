import { test } from 'vitest';
import assert from 'node:assert/strict';
import { decodeSetup, groupTrace, filterTrace } from './trace';
import { applyRecord, createState, hostEvent, summary, type ProtocolRecord } from './protocol';
const record = (event: unknown) =>
  ({ timestamp_ms: 1, event: { type: 'host', event } }) as ProtocolRecord;
const context = {
  session_id: 1,
  transfer_id: 2,
  endpoint: 0,
  kind: 'control',
  stage: 'setup',
  elapsed_us: 3,
  retries: 1,
};
const setup = [128, 6, 0, 1, 0, 0, 18, 0];
const packet = record({
  transfer: { endpoint: 0, direction: 'out', pid: 3, bytes: 8, data: setup },
});
test('normal summaries omit packet encoding while raw records retain it', () => {
  const packet = record({
    transfer: { direction: 'in', endpoint: 129, bytes: 2, pid: 2, data: [1, 2] },
  });
  assert.equal(summary(packet), 'Received · endpoint 129');
  assert.match(JSON.stringify(packet), /"pid":2/);
});

test('standard, HID and storage requests; truncated request', () => {
  assert.match(decodeSetup(setup), /Get descriptor/);
  assert.match(decodeSetup([161, 1, 0, 0, 0, 0, 8, 0]), /Get report/);
  assert.match(decodeSetup([161, 254, 0, 0, 0, 0, 1, 0]), /Get max LUN/);
  assert.equal(decodeSetup(setup.slice(1)), 'Incomplete request');
});
test('groups only context-before-packet, preserves partial and mixed history', () => {
  const c = record({ transfer_context: context });
  const groups = groupTrace([c, packet, c, packet]);
  assert.equal(groups.length, 1);
  assert.equal(groups[0].records.length, 4);
  assert.equal(groupTrace([packet, c]).length, 2);
  assert.equal(groupTrace([c, record('detached'), packet]).length, 3);
  assert.equal(
    groupTrace([record({ transfer_context: { ...context, endpoint: 129 } }), packet]).length,
    2,
  );
  assert.equal(
    filterTrace(groups, { endpoint: '0', direction: 'out', kind: 'control', errors: false }).length,
    1,
  );
  assert.equal(
    filterTrace(groups, { endpoint: '129', direction: '', kind: '', errors: false }).length,
    0,
  );
  assert.equal(
    filterTrace(groups, { endpoint: '', direction: '', kind: '', errors: true }).length,
    0,
  );
});
test('new payload guards reject missing fields, overlong data and numeric overflow', () => {
  const payloads = {
    lifecycle: { state: 'ready', session_id: 1 },
    transfer_context: context,
    diagnostic: {
      code: 'timeout',
      setup,
      session_id: 1,
      transfer_id: 2,
      stage: 'data',
      endpoint: 0,
      elapsed_us: 1000,
      retries: 2,
      hcint: 0,
      hcchar: 0,
      hctsiz: 0,
      hprt: 0,
    },
    storage: { blocks: 2, block_size: 512, vendor: 'vendor', product: 'product', fat32: true },
    file: { index: 0, name: 'TEST.TXT', size: 1, directory: false },
    file_data: { index: 0, offset: 0, total_size: 1, data: [1], eof: true },
    storage_error: { message: 'Failed' },
  };
  for (const [key, value] of Object.entries(payloads)) {
    assert.ok(hostEvent({ [key]: value }));
    for (const field of Object.keys(value)) {
      const bad = { ...value };
      delete bad[field as keyof typeof bad];
      assert.equal(hostEvent({ [key]: bad }), null, `${key}.${field}`);
    }
  }
  assert.equal(hostEvent({ file_data: { ...payloads.file_data, data: Array(129).fill(0) } }), null);
  assert.equal(hostEvent({ transfer_context: { ...context, retries: 2 ** 32 } }), null);
  assert.equal(hostEvent({ diagnostic: { ...payloads.diagnostic, setup: [1] } }), null);
  for (const elapsed_us of [-1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1]) {
    assert.equal(hostEvent({ diagnostic: { ...payloads.diagnostic, elapsed_us } }), null);
  }
  for (const retries of [-1, 0.5, NaN, Infinity, 2 ** 32]) {
    assert.equal(hostEvent({ diagnostic: { ...payloads.diagnostic, retries } }), null);
  }
  assert.ok(hostEvent({ diagnostic: { ...payloads.diagnostic, elapsed_us: 0, retries: 0 } }));
  assert.ok(
    hostEvent({
      diagnostic: {
        ...payloads.diagnostic,
        elapsed_us: Number.MAX_SAFE_INTEGER,
        retries: 0xffffffff,
      },
    }),
  );
  assert.equal(hostEvent({ lifecycle: { state: ['ready'], session_id: 1 } }), null);
  assert.equal(hostEvent({ transfer_context: { ...context, stage: ['data'] } }), null);
  assert.equal(hostEvent({ transfer_context: { ...context, kind: ['control'] } }), null);
});
test('lifecycle preserves partial topology on error/status and resets on new session', () => {
  const s = createState();
  applyRecord(s, record({ lifecycle: { state: 'enumerating', session_id: 1 } }));
  s.interfaces = [{ number: 0, alternate: 0, class: 3, subclass: 1, protocol: 1, endpoints: 1 }];
  applyRecord(s, record({ lifecycle: { state: 'error', session_id: 1 } }));
  applyRecord(
    s,
    record({
      status: { enabled: true, attached: false, address: 0, transfers: 0, errors: 1, dropped: 0 },
    }),
  );
  assert.equal(s.interfaces.length, 1);
  assert.equal(s.lifecycle, 'error');
  applyRecord(s, record({ lifecycle: { state: 'attached', session_id: 2 } }));
  assert.equal(s.interfaces.length, 0);
  for (const state of ['stopped', 'waiting', 'attached', 'enumerating', 'ready', 'error']) {
    applyRecord(s, record({ lifecycle: { state, session_id: 2 } }));
    assert.equal(s.lifecycle, state);
  }
});

test('non-USB floods update state without consuming USB history or export', () => {
  const s = createState();
  applyRecord(s, packet);
  for (let i = 0; i < 700; i++) {
    applyRecord(
      s,
      record({
        network: {
          info: { mode: 'setup', mac: [0, 1, 2, 3, 4, 5], ip: '192.168.4.1', error: null },
        },
      }),
    );
    applyRecord(s, record({ lifecycle: { state: 'ready', session_id: 1 } }));
    applyRecord(s, record('ready'));
    applyRecord(
      s,
      record({
        status: { enabled: true, attached: true, address: 1, transfers: 20, errors: 2, dropped: 3 },
      }),
    );
  }
  applyRecord(s, { timestamp_ms: 1, event: { type: 'error', message: 'Processor error' } });
  applyRecord(s, { timestamp_ms: 1, event: { type: 'dropped', count: 4 } });
  applyRecord(s, record({ storage_error: { message: 'No file' } }));
  assert.equal(s.history.length, 1);
  assert.equal(s.history[0].event, packet.event);
  assert.equal(s.network?.ip, '192.168.4.1');
  assert.equal(s.lifecycle, 'ready');
  assert.equal(s.address, 1);
  assert.equal(s.transfers, 20);
  assert.equal(s.errors, 3);
  assert.equal(s.queueDropped, 4);
  assert.equal(s.storageError, 'No file');
  const exported = JSON.parse(JSON.stringify(s.history)) as ProtocolRecord[];
  assert.equal(exported.length, 1);
  assert.deepEqual(exported[0].event, packet.event);
  applyRecord(s, record({ error: { code: 'stall' } }));
  assert.equal(s.history.length, 2);
});

test('omitted non-USB records break context pairing across pages and JSON export', () => {
  const s = createState();
  applyRecord(s, record({ transfer_context: context }));
  applyRecord(s, record('ready'));
  applyRecord(s, packet);
  assert.equal(groupTrace(s.history).length, 2);
  assert.equal(groupTrace(JSON.parse(JSON.stringify(s.history))).length, 2);
  applyRecord(s, record({ transfer_context: context }));
  applyRecord(s, packet);
  const groups = groupTrace(s.history);
  assert.equal(groups.length, 3);
  assert.equal(groups[2].records.length, 2);
});
