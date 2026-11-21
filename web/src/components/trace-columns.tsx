import { For, Show, type Accessor, type Setter } from 'solid-js';
import type { ColumnDef } from '@tanstack/solid-table';
import { traceRecordKey, viewportGroups } from '../lib/trace-viewport';
import { hex, hostEvent, summary, type ProtocolRecord } from '../lib/protocol';
import { decodeSetup } from '../lib/trace';
import { Accordion, AccordionItem, AccordionTrigger, AccordionContent } from './ui/accordion';

type TraceRow = ReturnType<typeof viewportGroups>[number];

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

export function traceColumns(props: {
  expanded: Accessor<Set<string>>;
  setExpanded: Setter<Set<string>>;
}): ColumnDef<TraceRow>[] {
  return [
    {
      id: 'time',
      header: 'Time (ms)',
      accessorFn: (row) => row.records[0].timestamp_ms,
      size: 96,
      cell: (cell) => (
        <span class="font-mono text-xs tabular-nums">
          {cell.row.original.records[0].timestamp_ms.toLocaleString()}
        </span>
      ),
    },
    {
      id: 'endpoint',
      header: 'Endpoint',
      accessorKey: 'endpoint',
      size: 80,
      cell: (cell) => <span class="font-mono text-xs">{cell.row.original.endpoint ?? '—'}</span>,
    },
    {
      id: 'transfer',
      header: 'Transfer',
      accessorKey: 'kind',
      size: 112,
      cell: (cell) => (
        <span class="text-xs">
          {cell.row.original.kind ?? '—'}
          {cell.row.original.direction ? ` · ${cell.row.original.direction.toUpperCase()}` : ''}
        </span>
      ),
    },
    {
      id: 'details',
      header: 'Details',
      enableHiding: false,
      cell: (cell) => (
        <Accordion
          multiple
          collapsible
          value={cell.row.original.records
            .map(traceRecordKey)
            .filter((key) => props.expanded().has(key))}
          onChange={(values) =>
            props.setExpanded((previous) => {
              const next = new Set(previous);
              for (const record of cell.row.original.records) {
                const key = traceRecordKey(record);
                if (values.includes(key)) next.add(key);
                else next.delete(key);
              }
              return next;
            })
          }
        >
          <For each={cell.row.original.records}>{(record) => <TraceRecord record={record} />}</For>
        </Accordion>
      ),
    },
  ];
}
