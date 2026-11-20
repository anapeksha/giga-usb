import { For, Show, createEffect, createMemo, createSignal, on, onCleanup } from 'solid-js';
import { createVirtualizer } from '@tanstack/solid-virtual';
import { traceRecordKey, viewportGroups, survivingAnchor } from '../lib/trace-viewport';
import { hex, hostEvent, summary, type ProtocolRecord } from '../lib/protocol';
import { decodeSetup, groupTrace, filterTrace } from '../lib/trace';
import { Card, CardHeader, CardTitle, CardDescription, CardContent } from './ui/card';
import { Button } from './ui/button';
import { Accordion, AccordionItem, AccordionTrigger, AccordionContent } from './ui/accordion';
import { Checkbox } from './ui/checkbox';
import { Callout } from './ui/callout';
import { TextField, TextFieldInput, TextFieldLabel } from './ui/text-field';
import {
  NumberField,
  NumberFieldLabel,
  NumberFieldGroup,
  NumberFieldInput,
  NumberFieldErrorMessage,
} from './ui/number-field';
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
    <AccordionItem
      value={traceRecordKey(props.record)}
      data-trace-record={traceRecordKey(props.record)}
    >
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
        <label class="h-4 text-xs leading-4 text-muted-foreground" for={`trace-${props.label}`}>
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
  const [search, setSearch] = createSignal('');
  const searchQuery = createMemo(() => search().trim().toLowerCase());
  const endpointInvalid = createMemo(
    () =>
      endpoint() !== '' &&
      (!/^\d+$/.test(endpoint()) ||
        !Number.isInteger(Number(endpoint())) ||
        Number(endpoint()) < 0 ||
        Number(endpoint()) > 255),
  );
  const records = createMemo(() => props.records, undefined, {
    equals: (a, b) => a.length === b.length && a.every((r, i) => r === b[i]),
  });
  const [expanded, setExpanded] = createSignal(new Set<string>());
  createEffect(() => {
    const retained = new Set(records().map(traceRecordKey));
    setExpanded((previous) => new Set([...previous].filter((key) => retained.has(key))));
  });
  const newest = createMemo(() =>
    endpointInvalid()
      ? []
      : filterTrace(groupTrace(records()), {
          endpoint: endpoint(),
          direction: direction().toLowerCase(),
          kind: kind().toLowerCase(),
          errors: errors(),
        })
          .filter(
            (group) =>
              !searchQuery() ||
              group.records.some((record) => {
                const packet = record.event.type === 'host' ? hostEvent(record.event.event) : null;
                const text = [summary(record)];
                if (packet?.[0] === 'transfer') {
                  text.push(packet[1].data.map((value) => hex(value).slice(2)).join(' '));
                  if (packet[1].pid === 3) text.push(decodeSetup(packet[1].data));
                }
                return text.some((value) => value.toLowerCase().includes(searchQuery()));
              }),
          )
          .reverse(),
  );
  const rows = createMemo(() => viewportGroups(newest()));
  const filterKey = createMemo(() =>
    JSON.stringify([endpoint(), direction(), kind(), errors(), searchQuery()]),
  );
  let viewport!: HTMLDivElement;
  let committedRows: ReturnType<typeof rows> = [];
  let anchor: { key: string; offset: number; record?: string; recordOffset?: number } | undefined;
  let revision = 0;
  let restoring = false;
  let disposed = false;
  onCleanup(() => {
    disposed = true;
  });
  const rememberAnchor = () => {
    if (restoring || committedRows !== rows() || !viewport) return;
    const offset = viewport.scrollTop;
    const item = virtualizer.getVirtualItems().find((item) => item.end > offset);
    anchor = item ? { key: String(item.key), offset: offset - item.start } : undefined;
    const top = viewport.getBoundingClientRect().top;
    const record = [...viewport.querySelectorAll<HTMLElement>('[data-trace-record]')].find(
      (element) => element.getBoundingClientRect().bottom > top,
    );
    const row = record?.closest<HTMLElement>('[data-row-key]');
    if (anchor && record && row) {
      const measured = virtualizer
        .getVirtualItems()
        .find((item) => item.key === row.dataset.rowKey);
      if (measured)
        anchor = {
          key: String(measured.key),
          offset: offset - measured.start,
          record: record.dataset.traceRecord,
          recordOffset: record.getBoundingClientRect().top - top,
        };
    }
  };
  const virtualizer = createVirtualizer<HTMLDivElement, HTMLTableRowElement>({
    get count() {
      return rows().length;
    },
    getScrollElement: () => viewport,
    get getItemKey() {
      // A new key function invalidates cached measurements even when the row count is unchanged.
      const staticRows = rows();
      return (index: number) => staticRows[index].key;
    },
    estimateSize: (index) => rows()[index].records.length * 44,
    overscan: 3,
    paddingStart: 40,
    onChange: rememberAnchor,
  });
  createEffect(
    on([rows, filterKey], ([next, filter], previous) => {
      const update = ++revision;
      const reset = previous !== undefined && previous[1] !== filter;
      const oldKeys = committedRows.map((row) => row.key);
      const saved = anchor;
      const follow = !viewport || viewport.scrollTop <= 1;
      restoring = true;
      queueMicrotask(() => {
        if (disposed || update !== revision) return;
        if (reset || follow || !next.length) {
          virtualizer.scrollToOffset(0);
        } else if (saved) {
          const key = survivingAnchor(
            oldKeys,
            next.map((row) => row.key),
            saved.key,
          );
          const recordIndex = saved.record
            ? next.findIndex((row) =>
                row.records.some((record) => traceRecordKey(record) === saved.record),
              )
            : -1;
          const index = recordIndex >= 0 ? recordIndex : next.findIndex((row) => row.key === key);
          if (index >= 0) {
            const position = virtualizer.getOffsetForIndex(index, 'start');
            if (position) virtualizer.scrollToOffset(position[0] + saved.offset);
          }
        }
        committedRows = next;
        requestAnimationFrame(() => {
          if (disposed || update !== revision) return;
          if (!reset && !follow && saved?.record) {
            const record = [...viewport.querySelectorAll<HTMLElement>('[data-trace-record]')].find(
              (element) => element.dataset.traceRecord === saved.record,
            );
            if (record) {
              const delta =
                record.getBoundingClientRect().top -
                viewport.getBoundingClientRect().top -
                (saved.recordOffset ?? 0);
              virtualizer.scrollToOffset(viewport.scrollTop + delta);
            }
          }
          restoring = false;
          rememberAnchor();
        });
      });
    }),
  );
  const virtualRows = createMemo(() =>
    virtualizer.getVirtualItems().map((item) => ({
      key: String(item.key),
      index: item.index,
    })),
  );
  const firstStart = () => virtualizer.getVirtualItems()[0]?.start ?? 40;
  const lastEnd = () => virtualizer.getVirtualItems().at(-1)?.end ?? 40;
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
          <Callout variant="warning" class="mb-4 text-sm">
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
          </Callout>
        </Show>
        <div class="mb-4 flex flex-wrap items-start gap-3">
          <NumberField
            class="grid w-40 gap-1"
            value={endpoint()}
            onChange={setEndpoint}
            minValue={0}
            maxValue={255}
            step={1}
            format={false}
            changeOnWheel={false}
            validationState={endpointInvalid() ? 'invalid' : 'valid'}
          >
            <NumberFieldLabel class="h-4 text-xs leading-4 font-normal text-muted-foreground">
              Endpoint
            </NumberFieldLabel>
            <NumberFieldGroup>
              <NumberFieldInput placeholder="All" />
            </NumberFieldGroup>
            <NumberFieldErrorMessage>
              Enter a whole number from 0 to 255, or clear for all.
            </NumberFieldErrorMessage>
          </NumberField>
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
          <TextField
            class="grid min-w-0 w-full gap-1 sm:w-64"
            value={search()}
            onChange={setSearch}
          >
            <TextFieldLabel class="h-4 text-xs leading-4 font-normal text-muted-foreground">
              Search USB trace
            </TextFieldLabel>
            <TextFieldInput type="search" placeholder="Summary, SETUP or hex bytes" />
          </TextField>
          <div class="grid gap-1">
            <span aria-hidden="true" class="h-4" />
            <Checkbox class="h-10" checked={errors()} onChange={setErrors}>
              Errors only
            </Checkbox>
          </div>
        </div>
        <div
          ref={viewport}
          class="trace-scroll"
          style={{ height: 'min(520px, 65vh)', 'overflow-anchor': 'none' }}
          onScroll={rememberAnchor}
          role="region"
          tabIndex={0}
          aria-label="USB trace records"
        >
          <Table
            wrapperClass="overflow-visible"
            class="min-w-160 table-fixed"
            aria-rowcount={Math.max(1, rows().length) + 1}
          >
            <colgroup>
              <col class="w-24" />
              <col class="w-20" />
              <col class="w-28" />
              <col />
            </colgroup>
            <caption class="sr-only">USB transfers and diagnostics</caption>
            <TableHeader>
              <TableRow aria-rowindex={1}>
                <TableHead scope="col">Time (ms)</TableHead>
                <TableHead scope="col">Endpoint</TableHead>
                <TableHead scope="col">Transfer</TableHead>
                <TableHead scope="col">Details</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              <TableRow aria-hidden="true" class="border-0 hover:bg-transparent">
                <TableCell
                  colSpan={4}
                  class="border-0 p-0"
                  style={{ height: `${Math.max(0, firstStart() - 40)}px` }}
                />
              </TableRow>
              <For
                each={virtualRows().map((item) => item.key)}
                fallback={
                  <TableRow aria-rowindex={2}>
                    <TableCell colSpan={4} class="empty p-5">
                      No matching events. Connect and start USB, or adjust the filters.
                    </TableCell>
                  </TableRow>
                }
              >
                {(key) => {
                  const index = () => rows().findIndex((row) => row.key === key);
                  const group = createMemo(() => rows()[index()]);
                  return (
                    <Show when={group()}>
                      {(group) => (
                        <TableRow
                          ref={(element) => {
                            queueMicrotask(() => {
                              if (!disposed && element.isConnected)
                                virtualizer.measureElement(element);
                            });
                          }}
                          data-index={index()}
                          data-row-key={key}
                          aria-rowindex={index() + 2}
                          classList={{ 'text-destructive': group().error }}
                        >
                          <TableCell class="align-top font-mono text-xs tabular-nums">
                            {group().records[0].timestamp_ms.toLocaleString()}
                          </TableCell>
                          <TableCell class="align-top font-mono text-xs">
                            {group().endpoint ?? '—'}
                          </TableCell>
                          <TableCell class="align-top text-xs">
                            {group().kind ?? '—'}
                            {group().direction ? ` · ${group().direction!.toUpperCase()}` : ''}
                          </TableCell>
                          <TableCell class="overflow-hidden p-0 [&_pre]:overflow-auto">
                            <Accordion
                              multiple
                              collapsible
                              value={group()
                                .records.map(traceRecordKey)
                                .filter((key) => expanded().has(key))}
                              onChange={(values) =>
                                setExpanded((previous) => {
                                  const next = new Set(previous);
                                  for (const record of group().records) {
                                    const key = traceRecordKey(record);
                                    if (values.includes(key)) next.add(key);
                                    else next.delete(key);
                                  }
                                  return next;
                                })
                              }
                            >
                              <For each={group().records}>
                                {(record) => <TraceRecord record={record} />}
                              </For>
                            </Accordion>
                          </TableCell>
                        </TableRow>
                      )}
                    </Show>
                  );
                }}
              </For>
              <TableRow aria-hidden="true" class="border-0 hover:bg-transparent">
                <TableCell
                  colSpan={4}
                  class="border-0 p-0"
                  style={{ height: `${Math.max(0, virtualizer.getTotalSize() - lastEnd())}px` }}
                />
              </TableRow>
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
