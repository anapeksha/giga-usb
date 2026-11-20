import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { test as base, expect } from '@playwright/test';
import type { Command } from '../lib/http';
import type { ProtocolRecord, RecordEvent } from '../lib/protocol';
import { fixtureTracePage } from './trace-query-fixture';

export const test = base.extend<
  {
    api: {
      records: ProtocolRecord[];
      commands: Command[];
      cursors: number[];
      commandStatus: number;
      eventsStatus: number;
      traceStatus: number;
      traceRequests: URLSearchParams[];
      holdEvents: () => () => void;
      holdCommand: (command: Command, status?: number) => () => void;
      pending: boolean;
      malformed: boolean;
      onCommand?: (command: Command) => void;
      emit: (event: RecordEvent) => void;
    };
  },
  { appURL: string }
>({
  appURL: [
    async ({ browserName }, use) => {
      if (browserName !== 'chromium') throw new Error('This suite targets Chromium');
      const bundle = await readFile(new URL('../../../dist/index.html.gz', import.meta.url));
      const server = createServer((request, response) => {
        if (request.url !== '/') {
          response.writeHead(404).end();
          return;
        }
        response.writeHead(200, { 'Content-Type': 'text/html', 'Content-Encoding': 'gzip' });
        response.end(bundle);
      });
      await new Promise<void>((resolve) => server.listen(0, '127.0.0.1', resolve));
      const address = server.address();
      if (!address || typeof address === 'string') throw new Error('Missing fixture address');
      try {
        await use(`http://127.0.0.1:${address.port}/`);
      } finally {
        server.closeAllConnections();
        await new Promise<void>((resolve, reject) =>
          server.close((error) => (error ? reject(error) : resolve())),
        );
      }
    },
    { scope: 'worker' },
  ],
  api: async ({ page }, use) => {
    const releases = new Set<() => void>();
    let held: { command: Command; status: number; wait: Promise<void> } | undefined;
    let heldEvents: Promise<void> | undefined;
    const api = {
      records: [] as ProtocolRecord[],
      commands: [] as Command[],
      cursors: [] as number[],
      commandStatus: 202,
      eventsStatus: 200,
      traceStatus: 200,
      traceRequests: [] as URLSearchParams[],
      holdEvents() {
        let release!: () => void;
        heldEvents = new Promise<void>((resolve) => {
          const timer = setTimeout(() => release(), 5000);
          release = () => {
            clearTimeout(timer);
            releases.delete(release);
            resolve();
          };
        });
        releases.add(release);
        return release;
      },
      holdCommand(command: Command, status = 202) {
        let release!: () => void;
        const wait = new Promise<void>((resolve) => {
          const timer = setTimeout(resolve, 5000);
          release = () => {
            clearTimeout(timer);
            releases.delete(release);
            resolve();
          };
        });
        releases.add(release);
        held = { command, status, wait };
        return release;
      },
      pending: false,
      malformed: false,
      onCommand: undefined as ((command: Command) => void) | undefined,
      emit(event: RecordEvent) {
        this.records.push({ timestamp_ms: this.records.length + 1, event });
      },
    };
    await page.route('**/api/**', async (route) => {
      const url = new URL(route.request().url());
      if (api.pending) return;
      if (url.pathname === '/api/command') {
        const { command } = route.request().postDataJSON() as { command: Command };
        api.commands.push(command);
        api.onCommand?.(command);
        const gate =
          held && JSON.stringify(held.command) === JSON.stringify(command) ? held : undefined;
        if (gate) {
          held = undefined;
          await gate.wait;
        }
        await route.fulfill({ status: gate?.status ?? api.commandStatus });
      } else if (url.pathname === '/api/trace') {
        api.traceRequests.push(url.searchParams);
        const result = fixtureTracePage(api.records, url.searchParams);
        await route.fulfill(
          api.traceStatus !== 200
            ? { status: api.traceStatus }
            : result === null
              ? { status: 400 }
              : { json: result },
        );
      } else if (url.pathname === '/api/events') {
        const after = Number(url.searchParams.get('after'));
        api.cursors.push(after);
        const gate = heldEvents;
        heldEvents = undefined;
        if (gate) await gate;
        if (api.eventsStatus !== 200) {
          await route.fulfill({ status: api.eventsStatus });
          return;
        }
        const records = api.records.slice(after, after + 3);
        await route.fulfill({
          json: api.malformed
            ? { records: [null], cursor: 0, dropped: 0 }
            : { records, cursor: after + records.length, dropped: 0 },
        });
      } else {
        throw new Error(`Unexpected API request: ${url.pathname}`);
      }
    });
    try {
      await use(api);
    } finally {
      for (const release of releases) release();
    }
  },
});
export { expect };
