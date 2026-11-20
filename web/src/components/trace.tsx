import { For, Show, createMemo, createSignal } from 'solid-js';
import { hex, hostEvent, summary, type ProtocolRecord } from '../lib/protocol';
import { decodeSetup, groupTrace, filterTrace } from '../lib/trace';
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from './ui/card';
import { Button } from './ui/button';
import { Accordion, AccordionItem, AccordionTrigger, AccordionContent } from './ui/accordion';
import { Checkbox } from './ui/checkbox';
import {
  Combobox,
  ComboboxControl,
  ComboboxInput,
  ComboboxTrigger,
  ComboboxContent,
  ComboboxItem,
  ComboboxItemLabel,
  ComboboxItemIndicator,
} from './ui/combobox';
import { Table, TableHeader, TableBody, TableHead, TableRow, TableCell } from './ui/table';

function TraceRecord(props: { record: ProtocolRecord }) {
  const packet = () =>
    props.record.event.type === 'host' ? hostEvent(props.record.event.event) : null;
  const raw = () => {
    const p = packet();
    return p?.[0] === 'transfer' ? p[1].data : undefined;
  };
  const setup = () => {
    const p = packet();
    return p?.[0] === 'transfer' && p[1].pid === 3 ? decodeSetup(p[1].data) : undefined;
  };
  return (
    <AccordionItem value={String(props.record.sequence ?? props.record.timestamp_ms)}>
      <AccordionTrigger class="gap-3 px-2 text-left font-mono text-xs">
        {summary(props.record)}
      </AccordionTrigger>
      <AccordionContent>
        <Show when={setup()}>{(text) => <p class="px-2 py-2 text-sm">{text()}</p>}</Show>
        <pre class="m-2 rounded bg-muted p-3">{JSON.stringify(props.record, null, 2)}</pre>
        <Show when={raw()}>
          {(bytes) => (
            <pre class="m-2 rounded bg-muted p-3">
              RAW BYTES ({bytes().length}){'\n'}
              {bytes()
                .map((value) => hex(value).slice(2))
                .join(' ')}
            </pre>
          )}
        </Show>
      </AccordionContent>
    </AccordionItem>
  );
}
function TraceFilter(props: {
  label: string;
  options: string[];
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <Combobox
      options={props.options}
      value={props.value || 'All'}
      onChange={(value) => props.onChange(value === 'All' ? '' : (value ?? ''))}
      itemComponent={(item) => (
        <ComboboxItem item={item.item}>
          <ComboboxItemLabel>{item.item.rawValue}</ComboboxItemLabel>
          <ComboboxItemIndicator />
        </ComboboxItem>
      )}
    >
      <div class="grid w-40 gap-1">
        <label class="text-xs text-muted-foreground" for={`trace-${props.label}`}>
          {props.label}
        </label>
        <ComboboxControl>
          <ComboboxInput id={`trace-${props.label}`} />
          <ComboboxTrigger aria-label={`Show ${props.label.toLowerCase()} options`} />
        </ComboboxControl>
      </div>
      <ComboboxContent />
    </Combobox>
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
  const endpoints = createMemo(() => [
    'All',
    ...new Set(
      props.records.flatMap((r) => {
        const p = r.event.type === 'host' ? hostEvent(r.event.event) : null;
        return p && 'endpoint' in p[1] ? [String(p[1].endpoint)] : [];
      }),
    ),
  ]);
  const records = createMemo(() => props.records, undefined, {
    equals: (a, b) => a.length === b.length && a.every((r, i) => r === b[i]),
  });
  const [expanded, setExpanded] = createSignal(new Map<ProtocolRecord, boolean>());
  const newest = createMemo(() =>
    filterTrace(groupTrace(records()), {
      endpoint: endpoint(),
      direction: direction().toLowerCase(),
      kind: kind().toLowerCase(),
      errors: errors(),
    }).reverse(),
  );
  return (
    <Card class="mt-5 min-w-0">
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
          <div class="notice notice-error">
            Some activity records are missing.
            <Accordion collapsible>
              <AccordionItem value="loss">
                <AccordionTrigger>Recording losses</AccordionTrigger>
                <AccordionContent>
                  {props.hostDropped} host records dropped · {props.transportDropped} missing in the
                  latest response. Packet associations may be incomplete.
                </AccordionContent>
              </AccordionItem>
            </Accordion>
          </div>
        </Show>
        <div class="mb-4 flex flex-wrap items-end gap-3">
          <TraceFilter
            label="Endpoint"
            options={endpoints()}
            value={endpoint()}
            onChange={setEndpoint}
          />
          <TraceFilter
            label="Direction"
            options={['All', 'IN', 'OUT']}
            value={direction()}
            onChange={setDirection}
          />
          <TraceFilter
            label="Transfer"
            options={['All', 'Control', 'Interrupt', 'Bulk']}
            value={kind()}
            onChange={setKind}
          />
          <Checkbox class="mb-3" checked={errors()} onChange={setErrors}>
            Errors only
          </Checkbox>
        </div>
        <div class="trace-scroll" role="region" tabIndex={0} aria-label="USB trace records">
          <Table>
            <caption class="sr-only">USB transfers and diagnostics</caption>
            <TableHeader>
              <TableRow>
                <TableHead scope="col">Time (ms)</TableHead>
                <TableHead scope="col">Endpoint</TableHead>
                <TableHead scope="col">Transfer</TableHead>
                <TableHead scope="col">Details</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <For
                each={newest()}
                fallback={
                  <TableRow>
                    <TableCell colSpan={4} class="empty p-5">
                      No matching events. Connect and start USB, or adjust the filters.
                    </TableCell>
                  </TableRow>
                }
              >
                {(group) => (
                  <TableRow classList={{ 'text-destructive': group.error }}>
                    <TableCell class="align-top font-mono text-xs tabular-nums">
                      {group.records[0].timestamp_ms.toLocaleString()}
                    </TableCell>
                    <TableCell class="align-top font-mono text-xs">
                      {group.endpoint ?? '—'}
                    </TableCell>
                    <TableCell class="align-top text-xs">
                      {group.kind ?? '—'}
                      {group.direction ? ` · ${group.direction.toUpperCase()}` : ''}
                    </TableCell>
                    <TableCell class="min-w-60 max-w-2xl p-0">
                      <Accordion
                        multiple
                        collapsible
                        value={group.records
                          .filter((r) => expanded().get(r))
                          .map((r) => String(r.sequence ?? r.timestamp_ms))}
                        onChange={(values) =>
                          setExpanded(
                            (previous) =>
                              new Map(
                                records().map((r) => [
                                  r,
                                  group.records.includes(r)
                                    ? values.includes(String(r.sequence ?? r.timestamp_ms))
                                    : (previous.get(r) ?? false),
                                ]),
                              ),
                          )
                        }
                      >
                        <For each={group.records}>
                          {(record) => <TraceRecord record={record} />}
                        </For>
                      </Accordion>
                    </TableCell>
                  </TableRow>
                )}
              </For>
            </TableBody>
          </Table>
        </div>
        <p class="mt-3 text-xs text-muted-foreground">
          Pausing only freezes this view. Exports may contain sensitive keyboard input.
        </p>
      </CardContent>
    </Card>
  );
}
