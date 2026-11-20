import { test } from 'vitest';
import assert from 'node:assert/strict';
import type { ProtocolRecord } from './protocol';
import { groupTrace } from './trace';
import { survivingAnchor, traceRecordKey, viewportGroups } from './trace-viewport';

const record = (sequence: number): ProtocolRecord => ({
  sequence,
  timestamp_ms: 1,
  event: {
    type: 'host',
    event: {
      transfer: {
        endpoint: 129,
        direction: 'in',
        pid: 0,
        bytes: 1,
        data: [0],
      },
    },
  },
});

test('standalone keys survive prepend, object replacement and oldest eviction', () => {
  const original = [record(1), record(2), record(3)];
  const before = viewportGroups(groupTrace(original));
  const after = viewportGroups(groupTrace([record(2), record(3), record(4)]));
  assert.equal(before[1].key, after[0].key);
  assert.equal(before[2].key, after[1].key);
  assert.equal(traceRecordKey(original[1]), traceRecordKey(record(2)));
});

test('large transfer groups stay bounded and retain surviving bucket keys', () => {
  const records = Array.from({ length: 500 }, (_, i) => record(i));
  const group = {
    key: '1:2:control',
    records,
    error: false,
    context: {
      session_id: 1,
      transfer_id: 2,
      endpoint: 0,
      kind: 'control' as const,
      stage: 'data' as const,
      elapsed_us: 0,
      retries: 0,
    },
  };
  const before = viewportGroups([group]);
  const after = viewportGroups([{ ...group, records: records.slice(3).concat(record(500)) }]);
  assert.ok(before.length > 60);
  assert.ok(before.every((row) => row.records.length <= 8));
  assert.equal(before[0].key, after[0].key);
  assert.equal(before[1].key, after[1].key);
  assert.deepEqual(
    before.flatMap((row) => row.records),
    records,
  );
});

test('anchor retains identity on prepend and selects nearest survivor on eviction', () => {
  assert.equal(survivingAnchor(['c', 'b', 'a'], ['d', 'c', 'b'], 'b'), 'b');
  assert.equal(survivingAnchor(['c', 'b', 'a'], ['d', 'c', 'b'], 'a'), 'b');
  assert.equal(survivingAnchor(['c', 'b', 'a'], ['c', 'a'], 'b'), 'c');
  assert.equal(survivingAnchor(['a'], [], 'a'), undefined);
});

test('legacy records without sequence receive distinct stable non-index identities', () => {
  const a = { ...record(1), sequence: undefined };
  const b = { ...record(1), sequence: undefined };
  assert.equal(traceRecordKey(a), traceRecordKey(a));
  assert.notEqual(traceRecordKey(a), traceRecordKey(b));
});
