import { createMutation } from '@tanstack/solid-query';
import { createResource, createSignal, onCleanup, Show, Suspense } from 'solid-js';
import { Button } from './ui/button';
import { createUpdateClient, type UpdateStatus } from '../lib/ota-update';

export function FirmwareUpdate(props: {
  connected: () => boolean;
  sessionAbortSignal: () => AbortSignal | undefined;
}) {
  const [status, setStatus] = createSignal<UpdateStatus>();
  const [file, setFile] = createSignal<File>();
  const [notice, setNotice] = createSignal('');
  const [installing, setInstalling] = createSignal(false);
  const client = createUpdateClient(setStatus);
  let current: AbortController | undefined;
  const [readStatus, { refetch }] = createResource(async () => status());
  const operation = createMutation(() => ({
    networkMode: 'always',
    retry: false,
    mutationFn: async (action: 'status' | 'upload' | 'install' | 'cancel') => {
      const session = props.sessionAbortSignal();
      if (!session || session.aborted) throw new Error('Connect to GigaUSB first.');
      const controller = new AbortController();
      current = controller;
      const abort = () => controller.abort(session.reason);
      session.addEventListener('abort', abort, { once: true });
      setNotice('');
      try {
        if (action === 'status') await client.status(controller.signal);
        if (action === 'upload') {
          await client.upload(file()!, controller.signal);
          setNotice('Board verified the update. Ready to install.');
        }
        if (action === 'install') {
          setInstalling(true);
          await client.install(controller.signal);
          setNotice('New build confirmed.');
        }
        if (action === 'cancel') {
          await client.cancel(controller.signal);
          setNotice('Board confirmed cancellation.');
        }
        await refetch();
      } finally {
        session.removeEventListener('abort', abort);
        if (current === controller) current = undefined;
        setInstalling(false);
      }
    },
    onError: (error: unknown) =>
      setNotice(
        `${error instanceof Error ? error.message : String(error)} Staged work may remain; reconnect and check status. An armed update continues on the board.`,
      ),
  }));
  const disabled = () => !props.connected() || operation.isPending;
  onCleanup(() => current?.abort());
  return (
    <details class="my-6 rounded-lg border p-4">
      <summary class="cursor-pointer font-semibold">Firmware update</summary>
      <p class="my-3 text-sm text-muted-foreground">
        Upload a signed OTA file. USB host and forwarding stop during update.
      </p>
      <div class="flex flex-wrap items-center gap-3">
        <Button disabled={disabled()} onClick={() => operation.mutate('status')}>
          Check status
        </Button>
        <label class="text-sm">
          OTA file{' '}
          <input
            type="file"
            accept=".bin"
            disabled={disabled()}
            onChange={(event) => {
              setFile(event.currentTarget.files?.[0]);
              setNotice('');
            }}
          />
        </label>
        <Button disabled={disabled() || !file()} onClick={() => operation.mutate('upload')}>
          Start upload
        </Button>
        <Button
          disabled={disabled() || status()?.phase !== 'ready' || status()?.busy}
          onClick={() => operation.mutate('install')}
        >
          Install and restart
        </Button>
        <Button
          disabled={
            !props.connected() ||
            installing() ||
            status()?.token == null ||
            ['committing', 'armed', 'cancelled'].includes(status()?.phase ?? '')
          }
          onClick={() => {
            current?.abort(new Error('Upload requests stopped; cancellation not yet confirmed.'));
            // Finish the interrupted mutation before issuing the explicit cancel request.
            void (async () => {
              for (let turn = 0; operation.isPending && turn < 800; turn++)
                await new Promise((resolve) => setTimeout(resolve, 10));
              if (!operation.isPending && props.connected()) operation.mutate('cancel');
            })();
          }}
        >
          Cancel update
        </Button>
      </div>
      <Suspense fallback={<p>Reading update status…</p>}>
        <Show when={status() || readStatus()}>
          {(s) => (
            <div class="mt-3 text-sm">
              <p>Current build: {s().build_id}</p>
              <p>
                Status: {s().phase}
                {s().confirmed ? ' · boot confirmed' : ' · boot not confirmed'}
              </p>
              <p>
                Progress: {s().offset} / {s().total} bytes
              </p>
              <Show when={s().total > 0}>
                <progress aria-label="Update progress" value={s().offset} max={s().total} />
              </Show>
              <p>{s().message}</p>
            </div>
          )}
        </Show>
      </Suspense>
      <Show when={notice()}>
        <p role="status" class="mt-3 text-sm">
          {notice()}
        </p>
      </Show>
    </details>
  );
}
