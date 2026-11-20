import { hostEvent, type ProtocolRecord, type HostPayloads } from './protocol';
export function decodeSetup(data: number[]): string {
  if (data.length !== 8) return 'Incomplete request';
  const [type, request] = data;
  const standard: Record<number, string> = {
    0: 'Get status',
    1: 'Clear feature',
    3: 'Set feature',
    5: 'Set address',
    6: 'Get descriptor',
    7: 'Set descriptor',
    8: 'Get configuration',
    9: 'Set configuration',
    10: 'Get interface',
    11: 'Set interface',
    12: 'Synchronize frame',
  };
  const classes: Record<number, string> = {
    1: 'Get report',
    2: 'Get idle',
    3: 'Get protocol',
    9: 'Set report',
    10: 'Set idle',
    11: 'Set protocol',
    254: 'Get max LUN',
    255: 'Storage reset',
  };
  const name =
    (type & 0x60) === 0 ? standard[request] : (type & 0x60) === 0x20 ? classes[request] : undefined;
  return `${name ?? 'Unknown request'} · ${type & 128 ? 'IN' : 'OUT'} · value ${data[2] | (data[3] << 8)} · index ${data[4] | (data[5] << 8)} · ${data[6] | (data[7] << 8)} bytes`;
}
export interface TraceGroup {
  key: string;
  records: ProtocolRecord[];
  context?: HostPayloads['transfer_context'];
  error: boolean;
  direction?: string;
  endpoint?: number;
  kind?: string;
}
/** Group only adjacent context/payload pairs; bounded history may start mid-transfer. */
export function groupTrace(records: ProtocolRecord[]): TraceGroup[] {
  const result: TraceGroup[] = [];
  const groups = new Map<string, TraceGroup>();
  let pending: HostPayloads['transfer_context'] | undefined;
  records.forEach((r, i) => {
    const p = r.event.type === 'host' ? hostEvent(r.event.event) : null;
    if (p?.[0] === 'transfer_context') {
      pending = p[1];
      result.push({
        key: `context-${i}`,
        records: [r],
        context: p[1],
        error: false,
        endpoint: p[1].endpoint,
        kind: p[1].kind,
      });
      return;
    }
    const context = pending;
    pending = undefined;
    if (
      p?.[0] === 'transfer' &&
      context &&
      context.endpoint === p[1].endpoint &&
      (r.sequence === undefined ||
        records[i - 1].sequence === undefined ||
        r.sequence === records[i - 1].sequence! + 1)
    ) {
      result.pop();
      const key = `${context.session_id}:${context.transfer_id}:${context.kind}`;
      let group = groups.get(key);
      if (!group) {
        group = {
          key,
          records: [],
          context,
          error: false,
          kind: context.kind,
          endpoint: context.endpoint,
          direction: p[1].direction,
        };
        groups.set(key, group);
        result.push(group);
      }
      group.records.push(records[i - 1], r);
      return;
    }
    if (p?.[0] === 'diagnostic') {
      const group = [...groups.values()].find(
        (g) =>
          g.context?.session_id === p[1].session_id && g.context?.transfer_id === p[1].transfer_id,
      );
      if (group) {
        group.error = true;
        group.records.push(r);
        return;
      }
    }
    result.push({
      key: `record-${i}`,
      records: [r],
      error:
        r.event.type === 'error' ||
        p?.[0] === 'error' ||
        p?.[0] === 'diagnostic' ||
        p?.[0] === 'storage_error',
      endpoint: p && 'endpoint' in p[1] ? p[1].endpoint : undefined,
      direction: p?.[0] === 'transfer' ? p[1].direction : undefined,
    });
  });
  return result;
}
export function filterTrace(
  groups: TraceGroup[],
  filter: { endpoint: string; direction: string; kind: string; errors: boolean },
) {
  return groups.filter(
    (g) =>
      (!filter.endpoint ||
        g.records.some((r) => {
          const p = r.event.type === 'host' ? hostEvent(r.event.event) : null;
          return p && 'endpoint' in p[1] && p[1].endpoint === Number(filter.endpoint);
        })) &&
      (!filter.direction ||
        g.records.some((r) => {
          const p = r.event.type === 'host' ? hostEvent(r.event.event) : null;
          return p?.[0] === 'transfer' && p[1].direction === filter.direction;
        })) &&
      (!filter.kind || g.kind === filter.kind) &&
      (!filter.errors || g.error),
  );
}
