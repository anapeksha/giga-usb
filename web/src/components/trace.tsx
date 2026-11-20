import { For, Show, createMemo, createSignal } from 'solid-js';
import { hex, hostEvent, summary, type ProtocolRecord } from '../lib/protocol';
import { decodeSetup, groupTrace, filterTrace } from '../lib/trace';
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from './ui/card';
import { Button } from './ui/button';

function TraceRecord(props: { record: ProtocolRecord }) {
  const raw = () => {
    if (props.record.event.type !== 'host') return undefined;
    const event = hostEvent(props.record.event.event);
    return event &&
      (event[0] === 'hid_report' || event[0] === 'descriptor' || event[0] === 'transfer')
      ? event[1].data
      : undefined;
  };
  const error = () =>
    props.record.event.type === 'error' ||
    (props.record.event.type === 'host' && hostEvent(props.record.event.event)?.[0] === 'error');
  return (
    <details class="trace-record" classList={{ 'trace-error': error() }}>
      <summary>
        <time>{props.record.timestamp_ms.toLocaleString()} ms</time>
        <span>{summary(props.record)}</span>
      </summary>
      <p class="px-3 py-2 text-sm">
        {(() => {
          const e = props.record.event;
          const p = e.type === 'host' ? hostEvent(e.event) : null;
          return p?.[0] === 'transfer' && p[1].pid === 3 ? decodeSetup(p[1].data) : '';
        })()}
      </p>
      <pre>{JSON.stringify(props.record, null, 2)}</pre>
      <Show when={raw()}>
        {(bytes) => (
          <pre>
            RAW BYTES ({bytes().length}){'\n'}
            {bytes()
              .map((value) => hex(value).slice(2))
              .join(' ')}
          </pre>
        )}
      </Show>
    </details>
  );
}
export function Trace(props: {
  records: ProtocolRecord[];
  transportDropped: number;
  hostDropped: number;
  paused: boolean;
  togglePaused: () => void;
  clear: () => void;
  exportTrace: () => void;
}) {
  const [endpoint, setEndpoint] = createSignal('');
  const [direction, setDirection] = createSignal('');
  const [kind, setKind] = createSignal('');
  const [errors, setErrors] = createSignal(false);
  const newest = createMemo(() =>
    filterTrace(groupTrace(props.records), {
      endpoint: endpoint(),
      direction: direction(),
      kind: kind(),
      errors: errors(),
    }).reverse(),
  );
  return (
    <Card class="mt-5">
      <CardHeader class="flex-row flex-wrap items-center justify-between gap-4 space-y-0">
        <div>
          <CardTitle>USB trace</CardTitle>
          <CardDescription class="mt-2">
            {props.records.length} records · newest first · board time
            {props.paused ? ' · display paused' : ''}
          </CardDescription>
        </div>
        <div class="flex flex-wrap gap-2">
          <Button variant="outline" size="sm" onClick={props.togglePaused}>
            {props.paused ? 'Resume display' : 'Pause display'}
          </Button>
          <Button variant="ghost" size="sm" onClick={props.clear}>
            Clear trace
          </Button>
          <Button variant="outline" size="sm" onClick={props.exportTrace}>
            Export trace
          </Button>
        </div>
      </CardHeader>
      <CardContent>
        <Show when={props.transportDropped > 0 || props.hostDropped > 0}>
          <div class="notice-error">
            Some activity records are missing.
            <details>
              <summary>Recording losses</summary>
              <p>
                {props.hostDropped} host records dropped · {props.transportDropped} missing in the
                latest response. Packet associations may be incomplete.
              </p>
            </details>
          </div>
        </Show>
        <div class="trace-filters mb-3 flex flex-wrap gap-3">
          <label>
            Endpoint{' '}
            <input
              class="w-20"
              type="number"
              min="0"
              max="255"
              value={endpoint()}
              onInput={(e) => setEndpoint(e.currentTarget.value)}
            />
          </label>
          <label>
            Direction{' '}
            <select value={direction()} onChange={(e) => setDirection(e.currentTarget.value)}>
              <option value="">All</option>
              <option value="in">IN</option>
              <option value="out">OUT</option>
            </select>
          </label>
          <label>
            Transfer{' '}
            <select value={kind()} onChange={(e) => setKind(e.currentTarget.value)}>
              <option value="">All</option>
              <option value="control">Control</option>
              <option value="interrupt">Interrupt</option>
              <option value="bulk">Bulk</option>
            </select>
          </label>
          <label>
            <input
              type="checkbox"
              checked={errors()}
              onChange={(e) => setErrors(e.currentTarget.checked)}
            />{' '}
            Errors only
          </label>
        </div>
        <For each={props.records.filter((r) => r.event.type === 'dropped')}>
          {(r) => (
            <p class="notice-error">
              Gap · {r.event.type === 'dropped' ? r.event.count : 0} records dropped by board
            </p>
          )}
        </For>
        <div class="trace-scroll" role="region" tabIndex={0} aria-label="USB trace records">
          <For
            each={newest()}
            fallback={
              <p class="empty p-5">
                No matching events. Connect and start USB, or adjust the filters.
              </p>
            }
          >
            {(group) => (
              <div>
                <Show when={group.context}>
                  <p class="px-3 pt-2 text-xs text-muted-foreground">{group.context?.kind}</p>
                </Show>
                <For each={group.records}>{(record) => <TraceRecord record={record} />}</For>
              </div>
            )}
          </For>
        </div>
        <p class="mt-3 text-xs text-muted-foreground">
          Pausing only freezes this view. Exports may contain sensitive keyboard input.
        </p>
      </CardContent>
    </Card>
  );
}
