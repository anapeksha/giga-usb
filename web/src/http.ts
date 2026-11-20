import { createQuery, createMutation, useQueryClient } from '@tanstack/solid-query';
import { createSignal, onCleanup, onMount } from 'solid-js';
import { appendFileChunk } from './lib/storage';
import {
  createState,
  hostEvent,
  type ProtocolRecord,
  type HostPayloads,
  PROTOCOL_VERSION,
} from './lib/protocol';
import {
  applyEventPage,
  createLossRefreshGuard,
  getEventPage,
  sendCommand,
  type Command,
} from './lib/http';
export type { Command } from './lib/http';

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));
let nextSession = 0;

export function createHttpController() {
  const client = useQueryClient();
  let state = { ...createState(), transportDropped: 0 };
  let cursor = 0;
  const listeners = new Set<(record: ProtocolRecord) => void>();
  const [downloading, setDownloading] = createSignal(false);
  let session: AbortController | null = null;
  let refreshGuard = createLossRefreshGuard();
  const [sessionId, setSessionId] = createSignal(0);
  const [active, setActive] = createSignal(false);
  const [snapshot, setSnapshot] = createSignal(structuredClone(state));
  const [connected, setConnected] = createSignal(false);
  const [paused, setPaused] = createSignal(false);
  const [notice, setNotice] = createSignal({ message: 'Connecting to GIGA…', error: false });
  const notify = (text: string, error = false) => setNotice({ message: text, error });
  const publish = () => setSnapshot({ ...state, history: [...state.history] });

  const command = createMutation(() => ({
    retry: false,
    mutationFn: async ({ command, current }: { command: Command; current: AbortController }) => {
      await sendCommand(command, current.signal);
    },
    onSuccess: (_, { current }) => {
      if (session === current) notify(`Request accepted.`);
    },
    onError: (error, { current }) => {
      if (session === current) notify(message(error), true);
    },
  }));
  const refreshStatus = (current: AbortController) => {
    if (!command.isPending) command.mutate({ command: 'status', current });
  };

  createQuery(() => ({
    queryKey: ['events', sessionId()],
    enabled: active(),
    retry: false,
    gcTime: 0,
    staleTime: 0,
    refetchOnMount: 'always',
    refetchOnReconnect: false,
    refetchInterval: 150,
    queryFn: async ({ signal }) => {
      const current = session;
      if (!current) throw new Error('Disconnected');
      try {
        let count: number;
        do {
          const page = await getEventPage(cursor, signal);
          if (session !== current || current.signal.aborted) throw new Error('Disconnected');
          cursor = page.cursor;
          applyEventPage(state, page);
          for (const record of page.records) for (const listener of listeners) listener(record);
          setConnected(true);
          if (notice().message === 'Connecting to GIGA…') notify('Connected.');
          for (const record of page.records) {
            if (record.event.type === 'hello' && record.event.protocol_version !== PROTOCOL_VERSION)
              notify(
                'Board software is incompatible with this app. Update the board and app together.',
                true,
              );
          }
          if (refreshGuard(page.dropped)) refreshStatus(current);
          if (!paused()) publish();
          count = page.records.length;
        } while (count === 3 && !signal.aborted);
        return { cursor };
      } catch (error) {
        if (session === current && !signal.aborted) {
          notify(`Connection failed: ${message(error)} Click Connect to retry.`, true);
          disconnect();
        }
        throw error;
      }
    },
  }));

  function disconnect() {
    session?.abort();
    session = null;
    setActive(false);
    setConnected(false);
    void client.cancelQueries({ queryKey: ['events', sessionId()], exact: true });
  }
  function connect() {
    if (session) return;
    const current = new AbortController();
    session = current;
    cursor = 0;
    refreshGuard = createLossRefreshGuard();
    state = { ...createState(), transportDropped: 0 };
    setPaused(false);
    publish();
    notify('Connecting to GIGA…');
    setSessionId(++nextSession);
    setActive(true);
    // Status acceptance is independent of event polling, including on initial failure.
    refreshStatus(current);
  }
  const download = createMutation(() => ({
    retry: false,
    mutationFn: async (file: HostPayloads['file']) => {
      const current = session;
      const usbSession = state.session;
      if (!current || !connected() || !state.attached) throw new Error('Device disconnected');
      setDownloading(true);
      const chunks: number[] = [];
      try {
        do {
          const offset = chunks.length;
          // Events can arrive before POST resolves: install the listener first,
          // then require both acceptance and the matching chunk before advancing.
          const chunk = await new Promise<HostPayloads['file_data']>((resolve, reject) => {
            let settled = false;
            let accepted = false;
            let received: HostPayloads['file_data'] | undefined;
            const finish = (error?: Error, data?: HostPayloads['file_data']) => {
              if (settled) return;
              if (!error && data && !accepted) {
                received = data;
                return;
              }
              settled = true;
              clearTimeout(timer);
              listeners.delete(receive);
              current.signal.removeEventListener('abort', abort);
              if (error) reject(error);
              else resolve(data!);
            };
            const abort = () => finish(new Error('Disconnected'));
            const receive = (record: ProtocolRecord) => {
              const p = record.event.type === 'host' ? hostEvent(record.event.event) : null;
              if (record.event.type === 'error') finish(new Error(record.event.message));
              if (
                p?.[0] === 'detached' ||
                p?.[0] === 'error' ||
                (p?.[0] === 'lifecycle' &&
                  (p[1].session_id !== usbSession ||
                    ['stopped', 'waiting', 'error'].includes(p[1].state)))
              )
                finish(new Error('USB device unavailable'));
              if (p?.[0] === 'storage_error') finish(new Error(p[1].message));
              if (p?.[0] === 'file_data' && p[1].index === file.index && p[1].offset === offset)
                finish(undefined, p[1]);
            };
            const timer = setTimeout(() => finish(new Error('File response timed out')), 10000);
            listeners.add(receive);
            current.signal.addEventListener('abort', abort, { once: true });
            void sendCommand({ storage_read: { file: file.index, offset } }, current.signal)
              .then(() => {
                accepted = true;
                if (received) finish(undefined, received);
              })
              .catch((error) => finish(new Error(message(error))));
          });
          if (appendFileChunk(file, chunks, chunk)) break;
        } while (chunks.length < 65536);
        const url = URL.createObjectURL(new Blob([new Uint8Array(chunks)]));
        const link = document.createElement('a');
        link.href = url;
        link.download = file.name;
        link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
        notify(chunks.length < file.size ? 'Downloaded first 64 KiB.' : 'Download complete.');
      } finally {
        setDownloading(false);
      }
    },
    onError: (error: unknown) => notify(message(error), true),
  }));
  onMount(connect);
  onCleanup(disconnect);
  return {
    snapshot,
    downloading,
    download: (file: HostPayloads['file']) => {
      if (!downloading()) download.mutate(file);
    },
    connected,
    busy: () => active() && !connected(),
    paused,
    notice,
    toggleConnection: () => {
      if (session) {
        disconnect();
        notify('Disconnected.');
      } else connect();
    },
    command: (value: Command) => {
      if (session && connected()) {
        if (value === 'storage_list') {
          state.files = [];
          state.storageError = null;
          publish();
        }
        command.mutate({ command: value, current: session });
      }
    },
    togglePaused: () => {
      setPaused((value) => !value);
      if (!paused()) publish();
    },
    clear: () => {
      state.history = [];
      publish();
    },
    exportTrace: () => {
      const url = URL.createObjectURL(
        new Blob([JSON.stringify(state.history, null, 2) + '\n'], { type: 'application/json' }),
      );
      const link = document.createElement('a');
      link.href = url;
      link.download = `giga-usb-${new Date().toISOString().replaceAll(':', '-')}.json`;
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    },
  };
}
