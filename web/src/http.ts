import { createQuery, createMutation, useQueryClient } from '@tanstack/solid-query';
import { createSignal, onCleanup, onMount } from 'solid-js';
import { toast } from 'solid-sonner';
import { appendFileChunk } from './lib/storage';
import {
  createState,
  hostEvent,
  type ProtocolRecord,
  type HostPayloads,
  PROTOCOL_VERSION,
} from './lib/protocol';
import {
  getTracePage,
  normalizeTraceFilters,
  type TraceFilterInput,
  applyEventPage,
  createLossRefreshGuard,
  getEventPage,
  sendCommand,
  type Command,
} from './lib/http';
export type { Command } from './lib/http';

const message = (error: unknown) => (error instanceof Error ? error.message : String(error));
const MAX_EVENT_PAGES = 8;
let nextSession = 0;

export function createHttpController() {
  const client = useQueryClient();
  let state = { ...createState(), transportDropped: 0 };
  let cursor = 0;
  let lastGenericError: string | undefined;
  const listeners = new Set<(record: ProtocolRecord) => void>();

  let session: AbortController | null = null;
  let refreshGuard = createLossRefreshGuard();
  const [sessionId, setSessionId] = createSignal(0);
  const [active, setActive] = createSignal(false);
  const [snapshot, setSnapshot] = createSignal(structuredClone(state));
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

  const events = createQuery(() => ({
    queryKey: ['events', sessionId()],
    enabled: active(),
    networkMode: 'always',
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
        // Bound catch-up work even when the board continuously returns full pages.
        for (let pageIndex = 0; pageIndex < MAX_EVENT_PAGES; pageIndex++) {
          const page = await getEventPage(cursor, signal);
          if (session !== current || current.signal.aborted) throw new Error('Disconnected');
          if (
            page.records.some(
              (record) =>
                record.event.type === 'hello' && record.event.protocol_version !== PROTOCOL_VERSION,
            )
          )
            throw new Error(
              'Board software is incompatible with this app. Update the board and app together.',
            );
          cursor = page.cursor;
          applyEventPage(state, page);
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
              toast.success('Connected.');
            if (!notice().error) notify('Connected.', false, false);
          }
          if (refreshGuard(page.dropped)) refreshStatus(current);
          if (page.records.length < 3 || signal.aborted) break;
        }
        if (!paused()) publish();
        return { cursor };
      } catch (error) {
        if (session === current && !signal.aborted) {
          if (!paused()) publish();
          notify(`Connection failed: ${message(error)} Click Connect to retry.`, true);
          disconnect();
        }
        throw error;
      }
    },
  }));

  const [traceFilters, setTraceFilters] = createSignal(
    normalizeTraceFilters({ endpoint: '', direction: '', kind: '', errors: false, q: '' }),
  );
  const [traceEpoch, setTraceEpoch] = createSignal(0);
  const [traceDebouncing, setTraceDebouncing] = createSignal(false);
  let traceSearchTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(traceSearchTimer));
  const [filteredRecords, setFilteredRecords] = createSignal<ProtocolRecord[]>([]);
  const [traceDropped, setTraceDropped] = createSignal(0);
  let traceCursor = 0;
  let traceHistory: ProtocolRecord[] = [];
  const resetTrace = () => {
    void client.cancelQueries({ queryKey: ['trace', sessionId(), traceEpoch()], exact: true });
    traceCursor = 0;
    setTraceDropped(0);
    traceHistory = [];
    setFilteredRecords([]);
    setTraceEpoch((value) => value + 1);
  };
  const filteredTrace = createQuery<{ cursor: number }>(() => ({
    queryKey: ['trace', sessionId(), traceEpoch()],
    enabled: active() && traceFilters().active && !traceFilters().error && !traceDebouncing(),
    networkMode: 'always',
    retry: false,
    gcTime: 0,
    staleTime: 0,
    refetchOnReconnect: false,
    refetchInterval: (query) => (query.state.status === 'error' ? false : 150),
    queryFn: async ({ signal }) => {
      const epoch = traceEpoch();
      const current = session;
      const filters = traceFilters();
      try {
        for (let index = 0; index < MAX_EVENT_PAGES; index++) {
          const page = await getTracePage(traceCursor, filters, signal);
          if (signal.aborted || epoch !== traceEpoch() || current !== session)
            throw new Error('Cancelled');
          const seen = new Set(traceHistory.map((record) => record.sequence));
          for (const item of page.records) {
            if (!seen.has(item.sequence)) {
              traceHistory.push({ ...item.record, sequence: item.sequence });
              seen.add(item.sequence);
            }
          }
          traceHistory = traceHistory.slice(-500);
          traceCursor = page.cursor;
          setTraceDropped(page.dropped);
          if (page.records.length < 3) break;
        }
      } finally {
        if (epoch === traceEpoch() && current === session && !signal.aborted && !paused())
          setFilteredRecords([...traceHistory]);
      }
      return { cursor: traceCursor };
    },
  }));

  function disconnect() {
    void client.cancelQueries({ queryKey: ['trace', sessionId(), traceEpoch()], exact: true });
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
    lastGenericError = undefined;
    resetTrace();
    refreshGuard = createLossRefreshGuard();
    state = { ...createState(), transportDropped: 0 };
    setPaused(false);
    publish();
    notify('Connecting to GIGA…', false, false);
    setSessionId(++nextSession);
    setActive(true);
    // Status acceptance is independent of event polling, including on initial failure.
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
    snapshot,
    traceFilters,
    filteredRecords,
    traceDropped,
    setTraceFilters: (input: TraceFilterInput) => {
      if (paused() || !connected()) return;
      const normalized = normalizeTraceFilters(input);
      if (JSON.stringify(normalized) === JSON.stringify(traceFilters())) return;
      const searchChanged = normalized.q !== traceFilters().q;
      clearTimeout(traceSearchTimer);
      setTraceDebouncing(searchChanged);
      setTraceFilters(normalized);
      resetTrace();
      if (searchChanged) traceSearchTimer = setTimeout(() => setTraceDebouncing(false), 200);
    },
    traceLoading: () =>
      traceFilters().active &&
      !traceFilters().error &&
      active() &&
      (traceDebouncing() || filteredTrace.isLoading),
    traceError: () =>
      traceFilters().error ||
      (traceFilters().active && filteredTrace.error ? message(filteredTrace.error) : ''),
    retryTrace: () => {
      if (active() && !traceFilters().error) void filteredTrace.refetch();
    },
    downloading: () => download.isPending,
    downloadingFile: () => (download.isPending ? download.variables?.file.index : undefined),
    download: (file: HostPayloads['file']) => {
      if (session && !download.isPending && !command.isPending)
        download.mutate({ file, current: session, usbSession: state.session });
    },
    connected,
    busy: () => active() && !connected(),
    isLoading: () => active() && !connected() && events.isLoading,
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
