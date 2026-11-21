import { createServer, type ServerResponse } from 'node:http';
import { readFile } from 'node:fs/promises';
import { test as base, expect } from '@playwright/test';
import type { Command } from '../lib/http';
import type { ProtocolRecord, RecordEvent } from '../lib/protocol';
import { fixtureTracePage } from './trace-query-fixture';

function createApi() {
  const streams = new Set<{ response: ServerResponse; push: () => void }>();
  const clients: { token?: string; proof?: string; response?: ServerResponse }[] = [{}, {}];
  const releases = new Set<() => void>();
  let held: { command: Command; status: number; wait: Promise<void> } | undefined;
  let heldEvents: Promise<void> | undefined;
  function gate() {
    let release!: () => void;
    const wait = new Promise<void>((resolve) => {
      release = () => {
        releases.delete(release);
        resolve();
      };
    });
    releases.add(release);
    return { wait, release };
  }
  const api = {
    records: [] as ProtocolRecord[],
    commands: [] as Command[],
    cursors: [] as number[],
    deliveredCursors: [] as number[],
    requests: [] as URLSearchParams[],
    admittedRequests: [] as URLSearchParams[],
    traceRequests: [] as URLSearchParams[],
    resets: [] as string[],
    commandStatus: 202,
    eventsStatus: 200,
    traceStatus: 200,
    pending: false,
    malformed: false,
    stalled: false,
    dropNextReset: false,
    droppedResets: [] as URLSearchParams[],
    onCommand: undefined as ((command: Command) => void) | undefined,
    tracePage: fixtureTracePage,
    get activeStreams() {
      return streams.size;
    },
    holdEvents() {
      const held = gate();
      heldEvents = held.wait;
      return held.release;
    },
    holdCommand(command: Command, status = 202) {
      const heldGate = gate();
      held = { command, status, wait: heldGate.wait };
      return heldGate.release;
    },
    emit(event: RecordEvent) {
      api.records.push({ timestamp_ms: api.records.length + 1, event });
      for (const stream of streams) stream.push();
    },
    endStreams() {
      for (const { response } of streams) response.end();
      streams.clear();
    },
    breakStreams() {
      for (const { response } of streams) response.destroy();
    },
    reboot() {
      api.endStreams();
      api.records.length = 0;
      for (const slot of clients) {
        slot.token = undefined;
        slot.proof = undefined;
        slot.response = undefined;
      }
    },
  };
  const heartbeat = setInterval(() => {
    if (!api.stalled) for (const { response } of streams) response.write(': heartbeat\n\n');
  }, 2000);
  return {
    api,
    async handle(request: import('node:http').IncomingMessage, response: ServerResponse) {
      const url = new URL(request.url!, 'http://127.0.0.1');
      if (url.pathname === '/api/events') {
        let after = Number(url.searchParams.get('after'));
        let traceAfter = Number(url.searchParams.get('trace_after'));
        api.cursors.push(after);
        api.requests.push(url.searchParams);
        if (url.searchParams.get('trace') === 'true') api.traceRequests.push(url.searchParams);
        const wait = heldEvents;
        heldEvents = undefined;
        if (wait) await wait;
        if (response.destroyed || api.pending) return;
        const status =
          api.eventsStatus !== 200
            ? api.eventsStatus
            : url.searchParams.get('trace') === 'true'
              ? api.traceStatus
              : 200;
        if (status !== 200) {
          response.writeHead(status).end();
          return;
        }
        const token = url.searchParams.get('client');
        const attempt = url.searchParams.get('attempt');
        const resume = url.searchParams.get('resume');
        const validToken = (value: string | null) =>
          value !== null && /^[0-9a-fA-F]{32}$/.test(value);
        if (
          !validToken(token) ||
          !validToken(attempt) ||
          (resume !== null && !validToken(resume))
        ) {
          response.writeHead(400).end();
          return;
        }
        const remembered = clients.find((slot) => slot.token === token);
        const slot = remembered ?? clients.find((slot) => !slot.response);
        if (!slot || slot.response) {
          response.writeHead(503).end();
          return;
        }
        const resumed = !!slot.proof && slot.token === token && slot.proof === resume;
        if (!resumed) slot.proof = attempt!;
        api.admittedRequests.push(url.searchParams);
        slot.token = token!;
        slot.response = response;
        response.on('close', () => {
          if (slot.response === response) slot.response = undefined;
        });
        response.writeHead(200, {
          'Content-Type': 'text/event-stream',
          'Cache-Control': 'no-store',
        });
        response.flushHeaders();
        const send = (event: string, data: unknown) =>
          response.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`);
        let reset = !resumed;
        let initial = true;
        const stream = {
          response,
          push() {
            if (api.stalled || response.destroyed) return;
            if (reset) {
              if (api.dropNextReset) {
                api.dropNextReset = false;
                api.droppedResets.push(url.searchParams);
                response.destroy();
                return;
              }
              send('reset', { resume: attempt });
              api.resets.push(token!);
              after = 0;
              traceAfter = 0;
              reset = false;
            }
            if (api.malformed) {
              send('events', { records: [null], cursor: 0, dropped: 0 });
              return;
            }
            while (initial || after < api.records.length) {
              initial = false;
              const records = api.records.slice(after, after + 3);
              after += records.length;
              send('events', { records, cursor: after, dropped: 0 });
              api.deliveredCursors.push(after);
            }
            if (url.searchParams.get('trace') === 'true') {
              do {
                const params = new URLSearchParams(url.searchParams);
                params.set('after', String(traceAfter));
                const result = api.tracePage(api.records, params);
                if (!result) {
                  response.end();
                  return;
                }
                send('trace', result);
                if (result.cursor === traceAfter) break;
                traceAfter = result.cursor;
              } while (traceAfter < api.records.length);
            }
          },
        };
        streams.add(stream);
        response.on('close', () => streams.delete(stream));
        stream.push();
      } else if (url.pathname === '/api/command') {
        if (api.pending) return;
        let body = '';
        for await (const chunk of request) body += String(chunk);
        const { command } = JSON.parse(body) as { command: Command };
        api.commands.push(command);
        api.onCommand?.(command);
        const commandGate =
          held && JSON.stringify(held.command) === JSON.stringify(command) ? held : undefined;
        if (commandGate) {
          held = undefined;
          await commandGate.wait;
        }
        response.writeHead(commandGate?.status ?? api.commandStatus).end();
      } else response.writeHead(404).end();
    },
    dispose() {
      clearInterval(heartbeat);
      for (const release of releases) release();
      api.endStreams();
    },
  };
}

export const test = base.extend<{ api: ReturnType<typeof createApi>['api']; appURL: string }>({
  api: async ({ browserName }, use) => {
    if (browserName !== 'chromium') throw new Error('This suite targets Chromium');
    const fixture = createApi();
    handlers.set(fixture.api, fixture);
    try {
      await use(fixture.api);
    } finally {
      fixture.dispose();
    }
  },
  appURL: async ({ api, browserName, context }, use) => {
    if (browserName !== 'chromium') throw new Error('This suite targets Chromium');
    const bundle = await readFile(new URL('../../../dist/index.html.gz', import.meta.url));
    // The API and gzip dashboard share one real loopback origin for each test.
    const fixture = handlers.get(api);
    if (!fixture) throw new Error('Missing API handler');
    const server = createServer((request, response) => {
      if (request.url === '/') {
        response.writeHead(200, { 'Content-Type': 'text/html', 'Content-Encoding': 'gzip' });
        response.end(bundle);
      } else void fixture.handle(request, response);
    });
    await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('Missing fixture address');
    try {
      await use(`http://127.0.0.1:${address.port}/`);
    } finally {
      await Promise.all(context.pages().map((page) => page.close()));
      api.endStreams();
      server.closeAllConnections();
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
    }
  },
});
const handlers = new WeakMap<ReturnType<typeof createApi>['api'], ReturnType<typeof createApi>>();
export { expect };
