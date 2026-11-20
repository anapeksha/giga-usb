import { createMutation, createQuery, useQueryClient } from '@tanstack/solid-query';
import { createSignal, onCleanup, Show } from 'solid-js';
import { Button } from './ui/button';
import { Card, CardContent } from './ui/card';
import {
  configureWifi,
  getWifiStatus,
  validateWifiCredentials,
  type WifiCredentials,
} from '../lib/wifi';

export function WifiSetup() {
  const client = useQueryClient();
  const [ssid, setSsid] = createSignal('');
  const [password, setPassword] = createSignal('');
  const [handoff, setHandoff] = createSignal(false);
  const [notice, setNotice] = createSignal('');
  const [invalidField, setInvalidField] = createSignal<'ssid' | 'password'>();
  let ssidInput: HTMLInputElement | undefined;
  let passwordInput: HTMLInputElement | undefined;
  let submission: AbortController | undefined;
  let disposed = false;
  const wifi = createQuery(() => ({
    queryKey: ['wifi', 'status'],
    queryFn: async ({ signal }) => {
      const next = await getWifiStatus(signal);
      if (!disposed) {
        if (handoff() && next.mode !== 'connecting') {
          setHandoff(false);
          setNotice('');
        }
      }
      return next;
    },
    staleTime: 2000,
    refetchInterval: (query) => (query.state.status === 'error' ? false : 3000),
  }));
  const status = () => wifi.data;
  const mutation = createMutation(() => ({
    retry: false,
    gcTime: 0,
    mutationFn: async (credentials: WifiCredentials) => {
      submission = new AbortController();
      await client.cancelQueries({ queryKey: ['wifi', 'status'] });
      await configureWifi(credentials, submission.signal);
    },
    onSuccess: () => {
      if (disposed) return;
      setPassword('');
      setHandoff(true);
      setNotice(
        'Credentials accepted. Connection is not yet confirmed. The setup network may disconnect.',
      );
      void client.invalidateQueries({ queryKey: ['wifi', 'status'] });
    },
    onError: (error) => {
      if (!disposed && !submission?.signal.aborted)
        setNotice(
          `${error instanceof Error ? error.message : 'Wi-Fi request failed.'} If the connection dropped, acceptance is unknown; reconnect and refresh status before retrying.`,
        );
    },
    onSettled: (_, __, credentials) => {
      credentials.password = '';
      credentials.ssid = '';
      mutation.reset();
    },
  }));
  const busy = () => mutation.isPending;
  function refresh() {
    setNotice('');
    setInvalidField(undefined);
    void wifi.refetch();
  }
  function submit(event: SubmitEvent) {
    event.preventDefault();
    if (busy() || handoff() || status()?.mode !== 'setup') return;
    const credentials = { ssid: ssid(), password: password() };
    const error = validateWifiCredentials(credentials);
    if (error) {
      const field = validateWifiCredentials({ ...credentials, password: '' }) ? 'ssid' : 'password';
      setInvalidField(field);
      setNotice(error);
      (field === 'ssid' ? ssidInput : passwordInput)?.focus();
      return;
    }
    setInvalidField(undefined);
    setNotice('Sending credentials…');
    mutation.mutate(credentials);
  }
  onCleanup(() => {
    disposed = true;
    submission?.abort();
    setPassword('');
  });

  const inputClass =
    'mt-2 w-full rounded-md border border-input bg-background px-3 py-2 text-sm focus-visible:outline-2 focus-visible:outline-ring';
  return (
    <Card class="mb-5">
      <CardContent class="p-5">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <div>
            <h2 class="text-base font-semibold">Wi-Fi</h2>
            <p class="mt-1 text-sm text-muted-foreground">
              {status()?.mode === 'connected'
                ? `Connected to ${status()?.ssid ?? 'router'}${status()?.ip ? ` · ${status()?.ip}` : ''}`
                : status()?.mode === 'connecting'
                  ? 'Joining router network'
                  : 'Connect GigaUSB to your Wi-Fi network.'}
            </p>
          </div>
          <Button variant="outline" size="sm" disabled={busy()} onClick={refresh}>
            Refresh Wi-Fi status
          </Button>
        </div>
        <Show when={status()?.saved}>
          <p class="mt-3 text-sm text-muted-foreground">
            Saved network available for automatic reconnect
          </p>
        </Show>
        <Show when={status()?.mode === 'setup' && status()?.saved}>
          <p class="mt-2 text-sm text-muted-foreground">
            Saved network unavailable. Enter credentials to retry; a failed join keeps the saved
            credentials.
          </p>
        </Show>
        <Show when={status()?.mode === 'connected' && status()?.saved === false}>
          <p class="mt-3 text-sm text-muted-foreground">
            Credentials not saved. Reboot will return to setup.
          </p>
        </Show>
        <Show when={!status() || status()?.mode === 'setup'}>
          <form class="mt-4" onSubmit={(event) => void submit(event)} autocomplete="off" novalidate>
            <div class="grid gap-4 sm:grid-cols-2">
              <label class="text-sm" for="wifi-ssid">
                Network name
                <input
                  ref={ssidInput}
                  id="wifi-ssid"
                  class={inputClass}
                  value={ssid()}
                  onInput={(event) => setSsid(event.currentTarget.value)}
                  autocomplete="off"
                  autocapitalize="off"
                  spellcheck={false}
                  required
                  disabled={busy()}
                  aria-invalid={invalidField() === 'ssid' || undefined}
                  aria-describedby={
                    invalidField() === 'ssid' ? 'wifi-help wifi-notice' : 'wifi-help'
                  }
                />
              </label>
              <label class="text-sm" for="wifi-password">
                Password <span class="text-muted-foreground">(leave blank for open networks)</span>
                <input
                  ref={passwordInput}
                  id="wifi-password"
                  class={inputClass}
                  type="password"
                  value={password()}
                  onInput={(event) => setPassword(event.currentTarget.value)}
                  autocomplete="new-password"
                  maxlength={64}
                  disabled={busy()}
                  aria-invalid={invalidField() === 'password' || undefined}
                  aria-describedby={
                    invalidField() === 'password' ? 'wifi-help wifi-notice' : 'wifi-help'
                  }
                />
              </label>
            </div>
            <p id="wifi-help" class="mt-3 text-xs text-muted-foreground">
              Credentials are sent and saved without encryption. Use a trusted network.
            </p>
            <Button
              class="mt-4"
              type="submit"
              disabled={busy() || handoff() || status()?.mode !== 'setup'}
            >
              {busy() ? 'Sending…' : 'Join network'}
            </Button>
          </form>
        </Show>
        <Show when={handoff() || status()?.mode === 'connecting'}>
          <div class="mt-4 rounded-md border p-4 text-sm">
            <p class="font-medium">Switch to your Wi-Fi network</p>
            <p class="mt-2 text-muted-foreground">
              Join your normal network, find GigaUSB in your router’s device list, and open its
              address in your browser. This page won’t redirect automatically.
            </p>
            <p class="mt-2 text-muted-foreground">
              If joining fails, reconnect to the GigaUSB setup network, open http://192.168.4.1, and
              try again.
            </p>
          </div>
        </Show>
        <p id="wifi-notice" class="mt-3 text-sm text-muted-foreground" role="status">
          {notice() ||
            (wifi.isError
              ? 'Wi-Fi status unavailable. Reconnect to GigaUSB if needed, then refresh status.'
              : wifi.isPending
                ? 'Checking Wi-Fi status…'
                : status()?.error || '')}
        </p>
      </CardContent>
    </Card>
  );
}
