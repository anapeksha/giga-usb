import { Show, createEffect, createMemo, createSignal } from 'solid-js';
import type { ProtocolRecord } from '../lib/protocol';
import type { TraceFilterInput } from '../lib/http';
import { TraceDataTable } from './trace-data-table';
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

function TraceFilter(props: {
  label: string;
  options: string[];
  disabled?: boolean;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <Combobox
      disabled={props.disabled}
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
  filteredRecords?: ProtocolRecord[];
  filtersActive?: boolean;
  setTraceFilters?: (filters: TraceFilterInput) => void;
  traceLoading?: boolean;
  traceDropped?: number;
  connected?: boolean;
  traceError?: string;
  retryTrace?: () => void;
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
  const filtersDisabled = () => props.paused || !props.connected;
  createEffect(() =>
    props.setTraceFilters?.({
      endpoint: endpoint(),
      direction: direction(),
      kind: kind(),
      errors: errors(),
      q: search(),
    }),
  );
  const endpointInvalid = createMemo(
    () =>
      endpoint() !== '' &&
      (!/^\d+$/.test(endpoint()) ||
        !Number.isInteger(Number(endpoint())) ||
        Number(endpoint()) < 0 ||
        Number(endpoint()) > 255),
  );
  const records = createMemo(
    () => (props.filtersActive ? (props.filteredRecords ?? []) : props.records),
    undefined,
    {
      equals: (a, b) => a.length === b.length && a.every((r, i) => r === b[i]),
    },
  );
  const filterKey = createMemo(() =>
    JSON.stringify([endpoint(), direction(), kind(), errors(), search()]),
  );
  return (
    <Card class="mt-5 min-w-0">
      <CardHeader class="flex-row flex-wrap items-center justify-between gap-4 space-y-0">
        <div>
          <CardTitle>USB trace</CardTitle>
          <CardDescription class="mt-2">
            {records().length} records · newest first · board time
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
        <fieldset
          disabled={filtersDisabled()}
          class="mb-4 flex flex-wrap items-start gap-3"
          aria-label="Trace filters"
        >
          <NumberField
            class="grid w-40 gap-1"
            disabled={filtersDisabled()}
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
            disabled={filtersDisabled()}
            label="Direction"
            options={['All', 'IN', 'OUT']}
            value={direction()}
            onChange={setDirection}
          />
          <TraceFilter
            disabled={filtersDisabled()}
            label="Transfer"
            options={['All', 'Control', 'Interrupt', 'Bulk']}
            value={kind()}
            onChange={setKind}
          />
          <TextField
            class="grid min-w-0 w-full gap-1 sm:w-64"
            disabled={filtersDisabled()}
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
            <Checkbox
              class="h-10"
              destructive
              disabled={filtersDisabled()}
              checked={errors()}
              onChange={setErrors}
            >
              Errors only
            </Checkbox>
          </div>
        </fieldset>
        <p class="mb-3 text-xs text-muted-foreground">Searches recent board history.</p>
        <Show when={props.filtersActive && (props.traceDropped ?? 0) > 0}>
          <p class="mb-3 text-sm text-muted-foreground">
            Earlier board history is no longer available.
          </p>
        </Show>
        <Show when={props.traceLoading}>
          <p role="status" class="mb-3 text-sm">
            Searching…
          </p>
        </Show>
        <Show when={props.traceError}>
          <Callout variant="error" class="mb-3 text-sm" role="alert">
            {props.traceError}{' '}
            <Button
              size="sm"
              variant="outline"
              disabled={filtersDisabled()}
              onClick={props.retryTrace}
            >
              Retry search
            </Button>
          </Callout>
        </Show>
        <TraceDataTable records={records()} resetKey={filterKey()} />
        <p class="mt-3 text-xs text-muted-foreground">
          Pausing only freezes this view. Exports may contain sensitive keyboard input.
        </p>
      </CardContent>
    </Card>
  );
}
