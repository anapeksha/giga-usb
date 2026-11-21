import { For, Show, createEffect, createMemo, createSignal, on, onCleanup } from 'solid-js';
import { createVirtualizer } from '@tanstack/solid-virtual';
import {
  createSolidTable,
  flexRender,
  getCoreRowModel,
  type VisibilityState,
} from '@tanstack/solid-table';
import { traceColumns } from './trace-columns';
import {
  DropdownMenu,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuCheckboxItem,
} from './ui/dropdown-menu';
import { traceRecordKey, viewportGroups, survivingAnchor } from '../lib/trace-viewport';
import type { ProtocolRecord } from '../lib/protocol';
import { groupTrace } from '../lib/trace';

import { Button } from './ui/button';
import { Table, TableHeader, TableBody, TableHead, TableRow, TableCell } from './ui/table';

export function TraceDataTable(props: { records: ProtocolRecord[]; resetKey: string }) {
  const records = () => props.records;
  const [expanded, setExpanded] = createSignal(new Set<string>());
  createEffect(() => {
    const retained = new Set(records().map(traceRecordKey));
    setExpanded((previous) => new Set([...previous].filter((key) => retained.has(key))));
  });
  const newest = createMemo(() => groupTrace(records()).reverse());
  const rows = createMemo(() => viewportGroups(newest()));
  const [columnVisibility, setColumnVisibility] = createSignal<VisibilityState>({});
  const table = createSolidTable({
    get data() {
      return rows();
    },
    columns: traceColumns({ expanded, setExpanded }),
    getCoreRowModel: getCoreRowModel(),
    getRowId: (row) => row.key,
    enableSorting: false,
    enableColumnFilters: false,
    enableRowSelection: false,
    onColumnVisibilityChange: setColumnVisibility,
    state: {
      get columnVisibility() {
        return columnVisibility();
      },
    },
  });
  const filterKey = () => props.resetKey;
  // Server results and unfiltered browser history use independent sequence spaces.
  createEffect(on(filterKey, () => setExpanded(new Set<string>()), { defer: true }));
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
    on([rows, filterKey, columnVisibility], ([next, filter, visibility], previous) => {
      const update = ++revision;
      const reset = previous !== undefined && previous[1] !== filter;
      const oldKeys = committedRows.map((row) => row.key);
      const saved = anchor;
      const follow = !viewport || viewport.scrollTop <= 1;
      restoring = true;
      queueMicrotask(() => {
        if (disposed || update !== revision) return;
        if (previous && previous[2] !== visibility) {
          virtualizer.measure();
          for (const element of viewport.querySelectorAll<HTMLTableRowElement>('[data-row-key]'))
            virtualizer.measureElement(element);
        }
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
    <>
      <div class="mb-3 flex justify-end">
        <DropdownMenu placement="bottom-end" modal={false}>
          <DropdownMenuTrigger as={Button<'button'>} variant="outline" size="sm">
            Columns
          </DropdownMenuTrigger>
          <DropdownMenuContent aria-label="Trace columns">
            <For each={table.getAllLeafColumns().filter((column) => column.getCanHide())}>
              {(column) => (
                <DropdownMenuCheckboxItem
                  checked={column.getIsVisible()}
                  onChange={(visible) => {
                    rememberAnchor();
                    column.toggleVisibility(visible);
                  }}
                  closeOnSelect={false}
                >
                  {column.id === 'time' ? 'Time' : (column.columnDef.header as string)}
                </DropdownMenuCheckboxItem>
              )}
            </For>
          </DropdownMenuContent>
        </DropdownMenu>
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
          aria-colcount={table.getVisibleLeafColumns().length}
        >
          <colgroup>
            <For each={table.getVisibleLeafColumns()}>
              {(column) => (
                <col
                  style={column.id === 'details' ? undefined : { width: `${column.getSize()}px` }}
                />
              )}
            </For>
          </colgroup>
          <caption class="sr-only">USB transfers and diagnostics</caption>
          <TableHeader>
            <For each={table.getHeaderGroups()}>
              {(headerGroup) => (
                <TableRow aria-rowindex={1}>
                  <For each={headerGroup.headers}>
                    {(header) => (
                      <TableHead scope="col" colSpan={header.colSpan}>
                        {flexRender(header.column.columnDef.header, header.getContext())}
                      </TableHead>
                    )}
                  </For>
                </TableRow>
              )}
            </For>
          </TableHeader>
          <TableBody>
            <TableRow aria-hidden="true" class="border-0 hover:bg-transparent">
              <TableCell
                colSpan={table.getVisibleLeafColumns().length}
                class="border-0 p-0"
                style={{ height: `${Math.max(0, firstStart() - 40)}px` }}
              />
            </TableRow>
            <For
              each={virtualRows().map((item) => item.key)}
              fallback={
                <TableRow aria-rowindex={2}>
                  <TableCell colSpan={table.getVisibleLeafColumns().length} class="empty p-5">
                    No matching events. Connect and start USB, or adjust the filters.
                  </TableCell>
                </TableRow>
              }
            >
              {(key) => {
                const index = () => rows().findIndex((row) => row.key === key);
                const row = createMemo(() => table.getRowModel().rowsById[key]);
                return (
                  <Show when={row()}>
                    {(row) => (
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
                        classList={{ 'text-destructive': row().original.error }}
                      >
                        <For each={row().getVisibleCells()}>
                          {(cell) => (
                            <TableCell
                              class={
                                cell.column.id === 'details'
                                  ? 'overflow-hidden p-0 [&_pre]:overflow-auto'
                                  : 'align-top'
                              }
                            >
                              {flexRender(cell.column.columnDef.cell, cell.getContext())}
                            </TableCell>
                          )}
                        </For>
                      </TableRow>
                    )}
                  </Show>
                );
              }}
            </For>
            <TableRow aria-hidden="true" class="border-0 hover:bg-transparent">
              <TableCell
                colSpan={table.getVisibleLeafColumns().length}
                class="border-0 p-0"
                style={{ height: `${Math.max(0, virtualizer.getTotalSize() - lastEnd())}px` }}
              />
            </TableRow>
          </TableBody>
        </Table>
      </div>
    </>
  );
}
