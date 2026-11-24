import { createQuery, createMutation, useQueryClient } from '@tanstack/solid-query';
import { batch, createResource, createSignal, onCleanup, onMount } from 'solid-js';
import { toast } from 'solid-sonner';
import { appendFileChunk } from './lib/storage';
import { createState, hostEvent, type ProtocolRecord, type HostPayloads } from './lib/protocol';
import {
  normalizeTraceFilters,
  type TraceFilterInput,
  applyEventPage,
  createLossRefreshGuard,
  sendCommand,
  type Command,
} from './lib/http';
import { runEventStream } from './lib/event-stream';
export type { Command } from './lib/http';

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));

let nextSession = 0;

export function createHttpController() {
  const client = useQueryClient();
  let state = { ...createState(), transportDropped: 0 };
  const cursors: { events: number; trace: number; resume?: string } = { events: 0, trace: 0 };
  const interruptions = new Set<() => void>();
  let deliveryGeneration = 0;
  const failDownloads = () => {
    deliveryGeneration++;
    for (const fail of interruptions) fail();
  };
  let lastGenericError: string | undefined;
  const listeners = new Set<(record: ProtocolRecord) => void>();

  let session: AbortController | null = null;
  let streamClient = '';
  let refreshGuard = createLossRefreshGuard();
  const [sessionId, setSessionId] = createSignal(0);
  const [active, setActive] = createSignal(false);
  let readiness: { current: AbortController; resolve: () => void } | undefined;
  const settleReadiness = (current: AbortController | null) => {
    if (readiness?.current !== current) return;
    readiness?.resolve();
    readiness = undefined;
  };
  const [ready] = createResource(sessionId, () => {
    const current = session;
    if (!current) return;
    return new Promise<void>((resolve) => {
      readiness = { current, resolve };
    });
  });
  const [snapshot, setSnapshot] = createSignal(structuredClone(state));
  const [controlState, setControlState] = createSignal({
    proxy: state.proxy,
    lifecycle: state.lifecycle,
  });
  const [connected, setConnected] = createSignal(false);
  const [paused, setPaused] = createSignal(false);
  const [notice, setNotice] = createSignal({ message: 'Connecting to GIGA…', error: false });
  const notify = (text: string, error = false, announce = true) => {
    if (!error) lastGenericError = undefined;
    setNotice({ message: text, error });
    if (announce) {
      if (error) toast.error(text);
      else toast.success(text);
    }
  };
  const publish = () => setSnapshot({ ...state, history: [...state.history] });

  const command = createMutation(() => ({
    networkMode: 'always',
    retry: false,
    mutationFn: async ({
      command,
      current,
    }: {
      command: Command;
      current: AbortController;
      silent?: boolean;
    }) => {
      await sendCommand(command, current.signal);
    },
    onSuccess: (_, { current, silent }) => {
      if (session === current && !current.signal.aborted && !silent) notify('Request queued.');
    },
    onError: (error, { current }) => {
      if (session === current && !current.signal.aborted) notify(message(error), true);
    },
  }));
  const refreshStatus = (current: AbortController) => {
    if (!command.isPending) command.mutate({ command: 'status', current, silent: true });
  };

  const [traceFilters, setTraceFilters] = createSignal(
    normalizeTraceFilters({ endpoint: '', direction: '', kind: '', errors: false, q: '' }),
  );
  const [traceEpoch, setTraceEpoch] = createSignal(0);
  const [traceDebouncing, setTraceDebouncing] = createSignal(false);
  let traceSearchTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(traceSearchTimer));
  const [filteredRecords, setFilteredRecords] = createSignal<ProtocolRecord[]>([]);
  const [traceDropped, setTraceDropped] = createSignal(0);
  let traceHistory: ProtocolRecord[] = [];
  const [traceReady, setTraceReady] = createSignal(false);
  const [streamError, setStreamError] = createSignal('');
  const resetTrace = () => {
    void client.cancelQueries({ queryKey: ['events', sessionId()] });
    setConnected(false);
    cursors.trace = 0;
    setTraceDropped(0);
    traceHistory = [];
    setFilteredRecords([]);
    setTraceReady(false);
    setTraceEpoch((value) => value + 1);
  };
  createQuery(() => ({
    queryKey: ['events', sessionId(), traceEpoch()],
    enabled: active(),
    networkMode: 'always',
    retry: false,
    gcTime: 0,
    refetchOnReconnect: false,
    refetchOnWindowFocus: false,
    queryFn: async ({ signal, queryKey }) => {
      const current = session;
      const filters = traceFilters();
      if (!current) throw new Error('Disconnected');
      const ensureCurrent = () => {
        signal.throwIfAborted();
        if (session !== current || current.signal.aborted || queryKey[2] !== traceEpoch())
          throw new DOMException('Cancelled', 'AbortError');
      };
      setStreamError('');
      try {
        return await runEventStream({
          signal,
          sessionSignal: current.signal,
          cursors,
          filters,
          client: streamClient,
          onReset: () => {
            ensureCurrent();
            failDownloads();
            state = { ...createState(), transportDropped: 0 };
            setControlState({ proxy: null, lifecycle: null });
            traceHistory = [];
            refreshGuard = createLossRefreshGuard();
            lastGenericError = undefined;
            setTraceDropped(0);
            setTraceReady(false);
            setStreamError('');
            setConnected(false);
            notify('Stream continuity changed. Refreshing device state…', false, false);
            if (!paused()) {
              publish();
              setFilteredRecords([]);
            }
          },
          onEvents: (page) => {
            ensureCurrent();
            if (page.dropped > state.transportDropped) failDownloads();
            applyEventPage(state, page);
            setControlState({ proxy: state.proxy, lifecycle: state.lifecycle });
            for (const record of page.records) {
              if (record.event.type === 'error') {
                const text = record.event.message;
                notify(text, true, text !== lastGenericError);
                lastGenericError = text;
              }
              for (const listener of listeners) listener(record);
            }
            if (!connected()) {
              setConnected(true);
              if (!page.records.some((record) => record.event.type === 'error'))
                notify('Connected.', false, false);
            }
            setStreamError('');
            if (refreshGuard(page.dropped)) refreshStatus(current);
            if (!paused()) publish();
            settleReadiness(current);
          },
          onTrace: (page) => {
            ensureCurrent();
            const seen = new Set(traceHistory.map((record) => record.sequence));
            for (const item of page.records) {
              if (!seen.has(item.sequence)) {
                traceHistory.push({ ...item.record, sequence: item.sequence });
                seen.add(item.sequence);
              }
            }
            traceHistory = traceHistory.slice(-500);
            setTraceDropped(page.dropped);
            setTraceReady(true);
            if (!paused()) setFilteredRecords([...traceHistory]);
          },
          onInterruption: (error) => {
            ensureCurrent();
            failDownloads();
            setConnected(false);
            setTraceReady(false);
            setStreamError(message(error));
            notify('Connection interrupted. Reconnecting…', true, false);
          },
        });
      } catch (error) {
        if (session === current && !signal.aborted && !current.signal.aborted) {
          notify('Connection failed: ' + message(error) + ' Click Connect to retry.', true);
          disconnect();
        }
        throw error;
      }
    },
  }));

  function disconnect() {
    clearTimeout(traceSearchTimer);
    setTraceDebouncing(false);
    settleReadiness(session);
    session?.abort();
    session = null;
    setActive(false);
    setConnected(false);
    void client.cancelQueries({ queryKey: ['events', sessionId()] });
  }
  function connect() {
    if (session) return;
    const current = new AbortController();
    session = current;
    // A continuity token, not authentication; getRandomValues also works over HTTP.
    streamClient = Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
      byte.toString(16).padStart(2, '0'),
    ).join('');
    cursors.events = 0;
    delete cursors.resume;
    lastGenericError = undefined;
    resetTrace();
    refreshGuard = createLossRefreshGuard();
    state = { ...createState(), transportDropped: 0 };
    setControlState({ proxy: null, lifecycle: null });
    setPaused(false);
    publish();
    notify('Connecting to GIGA…', false, false);
    setSessionId(++nextSession);
    setActive(true);
    // Status acceptance is independent of the event stream, including on initial failure.
    refreshStatus(current);
  }
  const download = createMutation(() => ({
    networkMode: 'always',
    retry: false,
    mutationFn: async ({
      file,
      current,
      usbSession,
    }: {
      file: HostPayloads['file'];
      current: AbortController;
      usbSession: number | null;
    }) => {
      if (current !== session || current.signal.aborted || !connected() || !state.attached)
        throw new Error('Device disconnected');
      const generation = deliveryGeneration;
      const chunks: number[] = [];
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
            interruptions.delete(interrupted);
            current.signal.removeEventListener('abort', abort);
            if (error) reject(error);
            else resolve(data!);
          };
          const abort = () => finish(new Error('Disconnected'));
          const interrupted = () => finish(new Error('Connection interrupted; download cancelled'));
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
          interruptions.add(interrupted);
          current.signal.addEventListener('abort', abort, { once: true });
          void sendCommand({ storage_read: { file: file.index, offset } }, current.signal)
            .then(() => {
              accepted = true;
              if (received) finish(undefined, received);
            })
            .catch((error) => finish(new Error(message(error))));
        });
        if (generation !== deliveryGeneration || current !== session || current.signal.aborted)
          throw new Error('Connection continuity lost; download cancelled');
        if (appendFileChunk(file, chunks, chunk)) break;
      } while (chunks.length < 65536);
      const url = URL.createObjectURL(new Blob([new Uint8Array(chunks)]));
      const link = document.createElement('a');
      link.href = url;
      link.download = file.name;
      link.click();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    },
    onSuccess: (_, { current }) => {
      if (session === current && !current.signal.aborted) notify('Download complete.');
    },
    onError: (error: unknown, { current }) => {
      if (session === current && !current.signal.aborted) notify(message(error), true);
    },
  }));
  onMount(connect);
  onCleanup(disconnect);
  return {
    snapshot: () => {
      ready();
      return snapshot();
    },
    proxyStatus: () => controlState().proxy,
    hostReady: () => controlState().lifecycle === 'ready',
    traceFilters,
    filteredRecords,
    traceDropped,
    setTraceFilters: (input: TraceFilterInput) => {
      if (paused() || !connected()) return;
      const normalized = normalizeTraceFilters(input);
      if (JSON.stringify(normalized) === JSON.stringify(traceFilters())) return;

      clearTimeout(traceSearchTimer);
      setTraceDebouncing(true);
      const apply = () =>
        batch(() => {
          setTraceFilters(normalized);
          resetTrace();
          setTraceDebouncing(false);
        });
      traceSearchTimer = setTimeout(apply, 200);
    },
    traceLoading: () =>
      traceFilters().active &&
      !traceFilters().error &&
      active() &&
      (traceDebouncing() || !traceReady()),
    traceError: () => traceFilters().error || (traceFilters().active ? streamError() : ''),
    retryTrace: () => {
      if (active() && !traceFilters().error) {
        void client.cancelQueries({ queryKey: ['events', sessionId()] });
        setConnected(false);
        setTraceReady(false);
        setTraceEpoch((value) => value + 1);
      }
    },
    downloading: () => download.isPending,
    downloadingFile: () => (download.isPending ? download.variables?.file.index : undefined),
    download: (file: HostPayloads['file']) => {
      if (session && !download.isPending && !command.isPending)
        download.mutate({ file, current: session, usbSession: state.session });
    },
    connected,
    busy: () => active() && !connected(),
    isLoading: () => active() && !connected(),
    isPending: () => command.isPending && command.variables?.current === session,
    pendingCommand: () =>
      command.isPending && command.variables?.current === session
        ? command.variables.command
        : undefined,
    paused,
    notice,
    toggleConnection: () => {
      if (session) {
        disconnect();
        notify('Disconnected.');
      } else connect();
    },
    command: (value: Command) => {
      if (session && connected() && !command.isPending) {
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
      if (!paused()) {
        publish();
        setFilteredRecords([...traceHistory]);
      }
    },
    clear: () => {
      state.history = [];
      traceHistory = [];
      setFilteredRecords([]);
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
