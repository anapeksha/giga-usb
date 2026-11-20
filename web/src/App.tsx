import { For, Show } from 'solid-js';
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
          <h1 class="text-2xl font-semibold tracking-tight">GigaUSB</h1>
        </div>
        <div class="flex items-center gap-4">
          <span class="transport" classList={{ online: http.connected() }}>
            <i />
            {http.connected() ? 'Connected' : 'Disconnected'}
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
          <dl class="status-grid">
            <div>
              <dt>Peripheral</dt>
              <dd>
                {!http.connected()
                  ? 'Connect to view'
                  : state().attached
                    ? `Attached${state().speed ? ` · ${state().speed} speed` : ''}`
                    : 'No device reported'}
              </dd>
            </div>
            <div>
              <dt>Host</dt>
              <dd>
                {state().lifecycle
                  ? state().lifecycle!.charAt(0).toUpperCase() + state().lifecycle!.slice(1)
                  : 'Unknown'}
              </dd>
            </div>
            <div>
              <dt>Address</dt>
              <dd>{state().address ?? '—'}</dd>
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
      <section aria-label="Trace counters" class="my-5 grid grid-cols-2 gap-3 lg:grid-cols-5">
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
                  {state()[counter.key].toLocaleString()}
                </p>
              </CardContent>
            </Card>
          )}
        </For>
      </section>
      <div class="grid items-start gap-5 lg:grid-cols-[minmax(0,2fr)_minmax(280px,1fr)]">
        <Topology state={state()} />
        <Hid state={state()} />
      </div>
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
                      Stage: {failure().stage} · Elapsed: {failure().elapsed_us.toLocaleString()} µs
                      · Retries: {failure().retries.toLocaleString()}
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
      <Trace
        records={state().history}
        transportDropped={state().transportDropped}
        hostDropped={state().dropped}
        paused={http.paused()}
        togglePaused={http.togglePaused}
        clear={http.clear}
        exportTrace={http.exportTrace}
      />
    </main>
  );
}
