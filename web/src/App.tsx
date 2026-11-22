import { For, Show, Suspense } from 'solid-js';
import usbIdsLicense from '../../USB-IDS-LICENSE.txt?raw';
import {
  Accordion,
  AccordionItem,
  AccordionTrigger,
  AccordionContent,
} from './components/ui/accordion';
import { createHttpController, type Command } from './http';
import { Button } from './components/ui/button';
import { Card, CardContent } from './components/ui/card';
import { Topology, Hid } from './components/topology';
import { Trace } from './components/trace';
import { decodeSetup } from './lib/trace';
import { Toaster } from './components/ui/sonner';
import { Callout } from './components/ui/callout';
import { Flex } from './components/ui/flex';
import { Separator } from './components/ui/separator';
import { Skeleton } from './components/ui/skeleton';
import { Tooltip, TooltipTrigger, TooltipContent } from './components/ui/tooltip';

export default function App() {
  const http = createHttpController();
  const state = http.snapshot;
  const commands: { command: Command; label: string }[] = [
    { command: 'start', label: 'Start' },
    { command: 'stop', label: 'Stop' },
    { command: 'reset', label: 'Reset USB' },
    { command: 'status', label: 'Refresh USB status' },
  ];
  return (
    <main class="mx-auto max-w-360 px-4 py-7 sm:px-8">
      <header class="mb-6 flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 class="text-2xl font-semibold tracking-tight">Giga USB</h1>
        </div>
        <div class="flex items-center gap-4">
          <span class="transport" classList={{ online: http.connected() }}>
            <i />
            {http.busy() ? 'Connecting' : http.connected() ? 'Connected' : 'Disconnected'}
          </span>
          <Tooltip>
            <TooltipTrigger
              as={Button}
              loading={http.isLoading()}
              onClick={() => void http.toggleConnection()}
            >
              {http.busy() ? 'Cancel connection' : http.connected() ? 'Disconnect' : 'Connect'}
            </TooltipTrigger>
            <TooltipContent>
              Disconnecting leaves the USB host running. Use Stop to turn off USB-A power.
            </TooltipContent>
          </Tooltip>
        </div>
      </header>

      <Card>
        <CardContent class="p-5">
          <Flex class="flex-wrap gap-5">
            <div>
              <h2 class="text-base font-semibold">USB host</h2>
              <p class="mt-2 text-sm text-muted-foreground">
                Connect a USB device to view its activity.
              </p>
            </div>
            <div class="flex flex-wrap gap-2">
              <For each={commands}>
                {(item) => (
                  <Button
                    variant="outline"
                    size="sm"
                    loading={http.isPending() && http.pendingCommand() === item.command}
                    disabled={
                      !http.connected() ||
                      http.busy() ||
                      http.isPending() ||
                      (http.downloading() && item.command !== 'stop')
                    }
                    onClick={() => void http.command(item.command)}
                  >
                    {item.label}
                  </Button>
                )}
              </For>
            </div>
          </Flex>
          <Separator class="mt-5" />
          <Show when={http.isLoading()}>
            <span class="sr-only" role="status">
              Loading status
            </span>
          </Show>
          <dl class="status-grid" aria-label="USB host status" aria-busy={http.isLoading()}>
            <div>
              <dt>Peripheral</dt>
              <dd>
                <Suspense
                  fallback={
                    <Skeleton
                      aria-hidden="true"
                      height={20}
                      width={144}
                      class="max-w-full rounded"
                    />
                  }
                >
                  {!state().attached && !http.connected()
                    ? 'Connect to view'
                    : state().attached
                      ? `Attached${state().speed ? ` · ${state().speed} speed` : ''}`
                      : 'No device reported'}
                </Suspense>
              </dd>
            </div>
            <div>
              <dt>Host</dt>
              <dd>
                <Suspense
                  fallback={
                    <Skeleton
                      aria-hidden="true"
                      height={20}
                      width={96}
                      class="max-w-full rounded"
                    />
                  }
                >
                  {state().lifecycle
                    ? state().lifecycle!.charAt(0).toUpperCase() + state().lifecycle!.slice(1)
                    : 'Unknown'}
                </Suspense>
              </dd>
            </div>
            <div>
              <dt>Address</dt>
              <dd>
                <Suspense
                  fallback={
                    <Skeleton
                      aria-hidden="true"
                      height={20}
                      width={48}
                      class="max-w-full rounded"
                    />
                  }
                >
                  {state().address ?? '—'}
                </Suspense>
              </dd>
            </div>
          </dl>
        </CardContent>
      </Card>
      <Show when={http.notice().error && http.notice().message}>
        <Callout role="alert" aria-live="off" variant="error" class="my-4 text-sm">
          {http.notice().message}
        </Callout>
      </Show>
      <Toaster theme="dark" />
      <section
        aria-label="Trace counters"
        aria-busy={http.isLoading()}
        class="my-5 grid grid-cols-2 gap-3 lg:grid-cols-5"
      >
        <For
          each={
            [
              {
                key: 'transfers',
                label: 'Transfers',
              },
              {
                key: 'errors',
                label: 'Errors',
              },
              {
                key: 'dropped',
                label: 'Capture losses',
              },
              {
                key: 'transportDropped',
                label: 'Missing records',
              },
              {
                key: 'queueDropped',
                label: 'Processing losses',
              },
            ] as const
          }
        >
          {(counter) => (
            <Card>
              <CardContent class="p-4">
                <p class="text-xs text-muted-foreground">{counter.label}</p>
                <p class="my-2 font-mono text-2xl tabular-nums">
                  <Suspense
                    fallback={
                      <Skeleton
                        as="span"
                        aria-hidden="true"
                        height={32}
                        width={80}
                        class="block max-w-full rounded"
                      />
                    }
                  >
                    {state()[counter.key].toLocaleString()}
                  </Suspense>
                </p>
              </CardContent>
            </Card>
          )}
        </For>
      </section>
      <Suspense fallback={<p role="status">Loading device details…</p>}>
        <div class="grid items-start gap-5 lg:grid-cols-[minmax(0,2fr)_minmax(280px,1fr)]">
          <Topology state={state()} />
          <Hid state={state()} />
        </div>
      </Suspense>
      <Suspense fallback={<p role="status">Loading diagnostics and storage…</p>}>
        <Show when={state().diagnostic}>
          {(failure) => (
            <Card class="mt-5">
              <CardContent class="p-5">
                <h2>Last failure</h2>
                <p>{failure().code.replaceAll('_', ' ')}</p>
                <Accordion collapsible>
                  <AccordionItem value="failure">
                    <AccordionTrigger>Technical details</AccordionTrigger>
                    <AccordionContent>
                      <p class="text-sm text-muted-foreground">
                        Stage: {failure().stage} · Elapsed: {failure().elapsed_us.toLocaleString()}{' '}
                        µs · Retries: {failure().retries.toLocaleString()}
                      </p>
                      <p>{decodeSetup(failure().setup)}</p>
                      <pre>{JSON.stringify(failure(), null, 2)}</pre>
                    </AccordionContent>
                  </AccordionItem>
                </Accordion>
              </CardContent>
            </Card>
          )}
        </Show>
        <Show when={state().storage}>
          {(storage) => (
            <Card class="mt-5">
              <CardContent class="p-5">
                <h2>Storage</h2>
                <p>
                  {storage().vendor} {storage().product} ·{' '}
                  {(storage().blocks * storage().block_size).toLocaleString()} bytes
                </p>
                <p class="text-sm text-muted-foreground">Read-only files</p>
                <Accordion collapsible>
                  <AccordionItem value="storage">
                    <AccordionTrigger>Storage details</AccordionTrigger>
                    <AccordionContent>
                      <p>
                        FAT32 root only · up to 32 entries · files up to 64 KiB · no subfolders or
                        writes.
                      </p>
                    </AccordionContent>
                  </AccordionItem>
                </Accordion>
                <Button
                  loading={http.isPending() && http.pendingCommand() === 'storage_list'}
                  disabled={
                    !storage().fat32 ||
                    !http.connected() ||
                    !state().attached ||
                    http.downloading() ||
                    http.isPending()
                  }
                  onClick={() => {
                    void http.command('storage_list');
                  }}
                >
                  List files
                </Button>
                <Show when={state().storageError}>
                  <Callout variant="error" role="alert" class="my-3 text-sm">
                    {state().storageError}
                  </Callout>
                </Show>
                <For each={state().files}>
                  {(file) => (
                    <div class="flex flex-wrap items-center justify-between gap-3 py-2 break-all">
                      <span>
                        {file.name} ·{' '}
                        {file.directory ? 'Folder' : `${file.size.toLocaleString()} bytes`}
                      </span>
                      <Button
                        variant="outline"
                        aria-label={`Download ${file.name}`}
                        loading={http.downloading() && http.downloadingFile() === file.index}
                        disabled={
                          file.directory ||
                          !state().attached ||
                          !http.connected() ||
                          http.downloading() ||
                          http.isPending()
                        }
                        onClick={() => http.download(file)}
                      >
                        Download
                      </Button>
                    </div>
                  )}
                </For>
              </CardContent>
            </Card>
          )}
        </Show>
      </Suspense>
      <Suspense fallback={<p role="status">Loading USB trace…</p>}>
        <Trace
          records={state().history}
          filteredRecords={http.filteredRecords()}
          filtersActive={http.traceFilters().active}
          setTraceFilters={http.setTraceFilters}
          traceLoading={http.traceLoading()}
          traceDropped={http.traceDropped()}
          connected={http.connected()}
          traceError={http.traceError()}
          retryTrace={http.retryTrace}
          transportDropped={state().transportDropped}
          hostDropped={state().dropped}
          paused={http.paused()}
          togglePaused={http.togglePaused}
          clear={http.clear}
          exportTrace={http.exportTrace}
        />
      </Suspense>
      <details class="mt-6 text-sm text-muted-foreground">
        <summary class="cursor-pointer rounded focus-visible:outline-2">
          USB ID database notices
        </summary>
        <pre class="mt-2 max-h-64 overflow-auto whitespace-pre-wrap">{usbIdsLicense}</pre>
      </details>
    </main>
  );
}
