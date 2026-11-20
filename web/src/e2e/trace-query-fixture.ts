import { isUsbTraceRecord, type ProtocolRecord, type HostEvent } from '../lib/protocol';

// Local API simulation, not a production matcher or hardware evidence.
export function fixtureTracePage(history: ProtocolRecord[], params: URLSearchParams) {
  const endpoint = params.get('endpoint');
  const direction = params.get('direction');
  const kind = params.get('kind');
  const errors = params.get('errors') === 'true';
  const q = (params.get('q') ?? '').trim().toLowerCase();
  if (
    (endpoint !== null && (!/^\d+$/.test(endpoint) || Number(endpoint) > 255)) ||
    (direction !== null && !['in', 'out'].includes(direction)) ||
    (kind !== null && !['control', 'interrupt', 'bulk'].includes(kind)) ||
    new TextEncoder().encode(q).length > 96
  )
    return null;
  const start = Math.max(0, history.length - 64);
  const after = Number(params.get('after') ?? 0);
  const first = after > history.length ? start : Math.max(start, after);
  const host = (index: number) => {
    const event = history[index]?.event;
    return event?.type === 'host' ? event.event : undefined;
  };
  const pair = (index: number) => {
    const event = host(index);
    if (!event || typeof event !== 'object') return [event];
    const previous = index > start ? host(index - 1) : undefined;
    const next = host(index + 1);
    const compatible = (a: HostEvent | undefined, b: HostEvent | undefined) =>
      a &&
      b &&
      typeof a === 'object' &&
      typeof b === 'object' &&
      'transfer_context' in a &&
      (('transfer' in b && a.transfer_context.endpoint === b.transfer.endpoint) ||
        ('diagnostic' in b &&
          a.transfer_context.endpoint === b.diagnostic.endpoint &&
          a.transfer_context.transfer_id === b.diagnostic.transfer_id &&
          a.transfer_context.session_id === b.diagnostic.session_id));
    return compatible(previous, event)
      ? [previous, event]
      : compatible(event, next)
        ? [event, next]
        : [event];
  };
  const records: { sequence: number; record: ProtocolRecord }[] = [];
  let cursor = first;
  for (let index = first; index < history.length; index++) {
    cursor = index + 1;
    const record = history[index];
    if (!isUsbTraceRecord(record)) continue;
    const events = pair(index).filter(
      (event): event is Exclude<HostEvent, string> => !!event && typeof event === 'object',
    );
    const text = events
      .map((event) => {
        if (!event || typeof event !== 'object') return '';
        if ('transfer' in event) {
          const p = event.transfer;
          const hex = p.data.map((byte) => byte.toString(16).padStart(2, '0'));
          return `${p.direction === 'in' ? 'received' : 'sent'} endpoint ${p.endpoint} ${hex.join('')} ${hex.join(' ')} ${p.pid === 3 && p.data[1] === 6 ? 'get_descriptor get descriptor' : ''}`;
        }
        return JSON.stringify(event).toLowerCase();
      })
      .join(' ');
    if (
      endpoint !== null &&
      !events.some(
        (e) =>
          e &&
          ('transfer' in e
            ? e.transfer.endpoint === Number(endpoint)
            : 'transfer_context' in e
              ? e.transfer_context.endpoint === Number(endpoint)
              : 'diagnostic' in e && e.diagnostic.endpoint === Number(endpoint)),
      )
    )
      continue;
    if (
      direction &&
      !events.some((e) => e && 'transfer' in e && e.transfer.direction === direction)
    )
      continue;
    if (
      kind &&
      !events.some((e) => e && 'transfer_context' in e && e.transfer_context.kind === kind)
    )
      continue;
    if (errors && !events.some((e) => e && ('diagnostic' in e || 'error' in e))) continue;
    if (q && !text.includes(q)) continue;
    records.push({ sequence: index + 1, record });
    if (records.length === 3) break;
  }
  return { records, cursor, dropped: after > history.length ? 0 : Math.max(0, start - after) };
}
