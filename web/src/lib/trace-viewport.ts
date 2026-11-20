import type { ProtocolRecord } from './protocol';
import type { TraceGroup } from './trace';

const fallbackKeys = new WeakMap<ProtocolRecord, string>();
let nextFallback = 0;
export function traceRecordKey(record: ProtocolRecord): string {
  if (record.sequence !== undefined) return `sequence-${record.sequence}`;
  let key = fallbackKeys.get(record);
  if (!key) {
    key = `local-${nextFallback++}`;
    fallbackKeys.set(record, key);
  }
  return key;
}

/** Sequence buckets bound large transfers without shifting keys on oldest eviction. */
export function viewportGroups(groups: TraceGroup[]): TraceGroup[] {
  return groups.flatMap((group) => {
    const chunks = new Map<string, ProtocolRecord[]>();
    for (const record of group.records) {
      const bucket =
        record.sequence === undefined
          ? traceRecordKey(record)
          : String(Math.floor(record.sequence / 8));
      const records = chunks.get(bucket) ?? [];
      records.push(record);
      chunks.set(bucket, records);
    }
    const transfer =
      group.context && !group.key.startsWith('context-')
        ? `transfer-${group.context.session_id}-${group.context.transfer_id}-${group.context.kind}`
        : traceRecordKey(group.records[0]);
    return [...chunks].map(([bucket, records]) => ({
      ...group,
      key: `${transfer}/${bucket}`,
      records,
    }));
  });
}

export function survivingAnchor(
  oldKeys: string[],
  newKeys: string[],
  key: string,
): string | undefined {
  const retained = new Set(newKeys);
  if (retained.has(key)) return key;
  const index = oldKeys.indexOf(key);
  for (let distance = 1; distance < oldKeys.length; distance++) {
    for (const candidate of [oldKeys[index - distance], oldKeys[index + distance]]) {
      if (candidate !== undefined && retained.has(candidate)) return candidate;
    }
  }
  return newKeys[0];
}
