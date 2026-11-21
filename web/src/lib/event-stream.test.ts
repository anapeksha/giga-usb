import { describe, expect, it, vi } from 'vitest';
import {
  createSseParser,
  EventStreamProtocolError,
  runEventStream,
  type EventStreamOptions,
} from './event-stream';
import { normalizeTraceFilters } from './http';

const encode = (text: string) => new TextEncoder().encode(text);
const page = (cursor: number) =>
  `event: events\ndata: {"records":[],"cursor":${cursor},"dropped":0}\n\n`;
const params = (url: Parameters<typeof fetch>[0]) =>
  new URL(String(url), 'http://board').searchParams;
const reset = (url: Parameters<typeof fetch>[0]) =>
  `event: reset\ndata: ${JSON.stringify({ resume: params(url).get('attempt') })}\n\n`;
const response = (text: string, close = false) =>
  new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encode(text));
        if (close) controller.close();
      },
    }),
    { headers: { 'Content-Type': 'text/event-stream' } },
  );
function setup(overrides: Partial<EventStreamOptions> = {}) {
  const abort = new AbortController();
  const options: EventStreamOptions = {
    signal: abort.signal,
    client: '0123456789abcdef0123456789abcdef',
    onReset: vi.fn(),
    sessionSignal: new AbortController().signal,
    cursors: { events: 0, trace: 0 },
    filters: normalizeTraceFilters({ endpoint: '', direction: '', kind: '', errors: false, q: '' }),
    onEvents: vi.fn(),
    onTrace: vi.fn(),
    onInterruption: vi.fn(),
    timeoutMs: 20,
    retryMs: 1,
    ...overrides,
  };
  return { abort, options };
}

describe('SSE framing', () => {
  it('handles byte fragmentation, multibyte UTF-8, CRLF, comments and multiline data', () => {
    const received: string[][] = [];
    const parse = createSseParser((event, data) => received.push([event, data]));
    for (const byte of encode(
      ': heartbeat\r\nevent: events\r\ndata: {"text":\r\ndata: "é😀"}\r\n\r\nevent: unknown\ndata: ignored\n\n',
    ))
      parse(new Uint8Array([byte]));
    expect(received).toEqual([
      ['events', '{"text":\n"é😀"}'],
      ['unknown', 'ignored'],
    ]);
  });
  it('bounds unfinished lines and whole frames, including ignored fields', () => {
    expect(() => createSseParser(() => {})(encode(':' + 'x'.repeat(8192)))).toThrow(
      EventStreamProtocolError,
    );
    expect(() => createSseParser(() => {})(encode('ignored: x\n'.repeat(1000)))).toThrow(
      EventStreamProtocolError,
    );
    expect(() => createSseParser(() => {})(new Uint8Array([255]))).toThrow(
      EventStreamProtocolError,
    );
  });
  it('decodes line spans rather than bytes and enforces raw CRLF frame bounds', () => {
    const decode = vi.spyOn(TextDecoder.prototype, 'decode');
    try {
      const deliver = vi.fn();
      createSseParser(deliver)(encode('data: ' + 'x'.repeat(8000) + '\r\n\r\n'));
      expect(decode).toHaveBeenCalledTimes(2);
      expect(deliver).toHaveBeenCalledTimes(1);
      expect(() =>
        createSseParser(() => {})(encode('data: ' + 'é'.repeat(4092) + '\r\n\r\n')),
      ).toThrow(EventStreamProtocolError);
      const parse = createSseParser(() => {});
      parse(new Uint8Array([0xc3]));
      expect(() => parse(encode('\n'))).toThrow(EventStreamProtocolError);
    } finally {
      decode.mockRestore();
    }
  });
  it('does not dispatch incomplete frames', () => {
    const callback = vi.fn();
    createSseParser(callback)(encode('event: events\ndata: {}\n'));
    expect(callback).not.toHaveBeenCalled();
  });
});

describe('event stream lifetime', () => {
  it('resumes after EOF and commits only after delivery', async () => {
    const request = vi.fn<typeof fetch>();
    const { abort, options } = setup({ request });
    request
      .mockResolvedValueOnce(response(page(7), true))
      .mockImplementationOnce(async (url, init) => {
        expect(String(url)).toContain('after=7');
        expect(new URL(String(url), 'http://board').searchParams.get('client')).toBe(
          options.client,
        );
        expect(
          new URL(String(request.mock.calls[0][0]), 'http://board').searchParams.get('client'),
        ).toBe(options.client);
        expect(init?.signal).toBeInstanceOf(AbortSignal);
        abort.abort();
        throw new Error('cancelled');
      });
    options.onEvents = () => expect(options.cursors.events).toBe(0);
    await expect(runEventStream(options)).rejects.toBeDefined();
    expect(options.cursors.events).toBe(7);
    expect(request).toHaveBeenCalledTimes(2);
  });
  it('resets continuity before fresh pages in the same chunk without mixed history', async () => {
    const { options, abort } = setup({
      cursors: { events: 100, trace: 90 },
      request: vi
        .fn<typeof fetch>()
        .mockImplementation(async (url) => response(reset(url) + page(1))),
    });
    const history = [100];
    const order: string[] = [];
    options.onReset = () => {
      history.length = 0;
      order.push('reset');
    };
    options.onEvents = (page) => {
      expect(options.cursors).toEqual({
        events: 0,
        trace: 0,
        resume: expect.stringMatching(/^[0-9a-f]{32}$/),
      });
      history.push(page.cursor);
      order.push('events');
      setTimeout(() => abort.abort(), 0);
    };
    await expect(runEventStream(options)).rejects.toBeDefined();
    expect(order).toEqual(['reset', 'events']);
    expect(history).toEqual([1]);
    expect(options.cursors).toEqual({
      events: 1,
      trace: 0,
      resume: expect.stringMatching(/^[0-9a-f]{32}$/),
    });
  });
  it('does not deliver later frames in the same chunk after cancellation', async () => {
    const { options, abort } = setup({
      request: vi
        .fn<typeof fetch>()
        .mockImplementation(async (url) => response(page(1) + reset(url) + page(2))),
    });
    options.onEvents = vi.fn(() => abort.abort());
    await expect(runEventStream(options)).rejects.toBeDefined();
    expect(options.onEvents).toHaveBeenCalledTimes(1);
    expect(options.onReset).not.toHaveBeenCalled();
    expect(options.onInterruption).not.toHaveBeenCalled();
    expect(options.cursors.events).toBe(1);
  });
  it('rejects malformed reset data without committing cursors', async () => {
    const { options } = setup({
      cursors: { events: 10, trace: 5 },
      request: vi.fn<typeof fetch>().mockResolvedValue(response('event: reset\ndata: []\n\n')),
    });
    await expect(runEventStream(options)).rejects.toBeInstanceOf(EventStreamProtocolError);
    expect(options.onReset).not.toHaveBeenCalled();
    expect(options.cursors).toEqual({ events: 10, trace: 5 });
  });
  it.each(['cancel', 'throw'])(
    'does not acknowledge a reset when its callback triggers %s',
    async (mode) => {
      const old = 'a'.repeat(32);
      const { options, abort } = setup({
        cursors: { events: 100, trace: 90, resume: old },
        request: vi
          .fn<typeof fetch>()
          .mockImplementation(async (url) => response(reset(url) + page(1))),
      });
      options.onReset = () => {
        expect(options.cursors).toEqual({ events: 100, trace: 90, resume: old });
        if (mode === 'cancel') abort.abort();
        else throw new EventStreamProtocolError('reset callback failed');
      };
      await expect(runEventStream(options)).rejects.toBeDefined();
      expect(options.cursors).toEqual({ events: 100, trace: 90, resume: old });
      expect(options.onEvents).not.toHaveBeenCalled();
    },
  );
  it.each([{}, { resume: null }, { resume: 'a'.repeat(32) }, { resume: 'bad' }])(
    'rejects an invalid or mismatched reset proof %j',
    async (payload) => {
      const { options } = setup({
        request: vi
          .fn<typeof fetch>()
          .mockResolvedValue(response(`event: reset\ndata: ${JSON.stringify(payload)}\n\n`)),
      });
      await expect(runEventStream(options)).rejects.toBeInstanceOf(EventStreamProtocolError);
      expect(options.onReset).not.toHaveBeenCalled();
      expect(options.cursors.resume).toBeUndefined();
    },
  );
  it('keeps the old proof after a truncated new-boot reset and resets before new history on retry', async () => {
    const old = 'a'.repeat(32);
    const requests: URLSearchParams[] = [];
    const order: string[] = [];
    const history = [100];
    const { options, abort } = setup({ cursors: { events: 100, trace: 90, resume: old } });
    let storedProof: string | null = null;
    options.request = vi.fn<typeof fetch>().mockImplementation(async (url) => {
      const query = params(url);
      requests.push(query);
      expect(query.get('client')).toBe(options.client);
      expect(query.get('attempt')).toMatch(/^[0-9a-f]{32}$/);
      if (requests.length <= 2) {
        expect(query.get('resume')).toBe(old);
        expect(query.get('after')).toBe('100');
        expect(query.get('trace_after')).toBe('90');
        expect(query.get('resume')).not.toBe(storedProof);
        storedProof = query.get('attempt');
        if (requests.length === 1) return response(reset(url).slice(0, -1), true);
        return response(reset(url) + page(1), true);
      }
      expect(query.get('resume')).toBe(storedProof);
      expect(query.get('after')).toBe('1');
      expect(query.get('trace_after')).toBe('0');
      abort.abort();
      throw new Error('done');
    });
    options.onReset = () => {
      history.length = 0;
      order.push('reset');
    };
    options.onEvents = (page) => {
      history.push(page.cursor);
      order.push('events');
    };
    await expect(runEventStream(options)).rejects.toBeDefined();
    expect(requests).toHaveLength(3);
    expect(new Set(requests.map((query) => query.get('attempt'))).size).toBe(3);
    expect(order).toEqual(['reset', 'events']);
    expect(history).toEqual([1]);
    expect(options.cursors.resume).toBe(requests[1].get('attempt'));
  });
  it('omits the initial proof and retains an acknowledged proof across query replacements', async () => {
    const { options, abort } = setup();
    options.request = vi.fn<typeof fetch>().mockImplementation(async (url) => {
      expect(params(url).has('resume')).toBe(false);
      return response(reset(url) + page(2));
    });
    options.onEvents = () => setTimeout(() => abort.abort(), 0);
    await expect(runEventStream(options)).rejects.toBeDefined();
    const proof = options.cursors.resume;
    const next = setup({ cursors: options.cursors, client: options.client });
    next.options.request = vi.fn<typeof fetch>().mockImplementation(async (url) => {
      expect(params(url).get('resume')).toBe(proof);
      expect(params(url).get('after')).toBe('2');
      next.abort.abort();
      throw new Error('done');
    });
    await expect(runEventStream(next.options)).rejects.toBeDefined();
    expect(options.cursors.resume).toBe(proof);
  });
  it('keeps a delivered page before a malformed subsequent page and never retries it', async () => {
    const request = vi
      .fn<typeof fetch>()
      .mockResolvedValue(
        response(page(4) + 'event: events\ndata: {"records":[],"cursor":-1,"dropped":0}\n\n'),
      );
    const { options } = setup({ request });
    await expect(runEventStream(options)).rejects.toBeInstanceOf(EventStreamProtocolError);
    expect(options.cursors.events).toBe(4);
    expect(options.onEvents).toHaveBeenCalledTimes(1);
    expect(request).toHaveBeenCalledTimes(1);
  });
  it.each(['events', 'trace'])(
    'rejects malformed %s records before delivery or cursor commit',
    async (event) => {
      const { options } = setup({
        filters: normalizeTraceFilters({
          endpoint: '128',
          direction: '',
          kind: '',
          errors: false,
          q: '',
        }),
        request: vi
          .fn<typeof fetch>()
          .mockResolvedValue(
            response(`event: ${event}\ndata: {"records":[{}],"cursor":12,"dropped":0}\n\n`),
          ),
      });
      await expect(runEventStream(options)).rejects.toBeInstanceOf(EventStreamProtocolError);
      expect(options.cursors).toEqual({ events: 0, trace: 0 });
      expect(options.onEvents).not.toHaveBeenCalled();
      expect(options.onTrace).not.toHaveBeenCalled();
    },
  );
  it('does not commit if the callback fails', async () => {
    const { options } = setup({
      request: vi.fn<typeof fetch>().mockResolvedValue(response(page(9))),
      onEvents: () => {
        throw new EventStreamProtocolError('callback');
      },
    });
    await expect(runEventStream(options)).rejects.toThrow('callback');
    expect(options.cursors.events).toBe(0);
  });
  it.each(['headers', 'body'])('times out stalled %s even when abort is ignored', async (stage) => {
    const { abort, options } = setup();
    options.request = vi.fn<typeof fetch>().mockImplementation(() =>
      stage === 'headers'
        ? new Promise(() => {})
        : Promise.resolve(
            new Response(new ReadableStream({ pull: () => new Promise(() => {}) }), {
              headers: { 'Content-Type': 'text/event-stream' },
            }),
          ),
    );
    options.onInterruption = (error) => {
      expect(String(error)).toContain('timed out');
      abort.abort();
    };
    await expect(runEventStream(options)).rejects.toBeDefined();
  });
  it.each(['read', 'backoff'])('cancels immediately during %s', async (stage) => {
    const { abort, options } = setup({ timeoutMs: 10000, retryMs: 5000 });
    options.request = vi
      .fn<typeof fetch>()
      .mockResolvedValue(response(': heartbeat\n\n', stage === 'backoff'));
    if (stage === 'backoff') options.onInterruption = () => setTimeout(() => abort.abort(), 0);
    else setTimeout(() => abort.abort(), 5);
    await expect(runEventStream(options)).rejects.toBeDefined();
    expect(options.request).toHaveBeenCalledTimes(1);
  });
  it('ignores unknown events and omits invalid filter parameters without stopping canonical events', async () => {
    const { options, abort } = setup({
      filters: normalizeTraceFilters({
        endpoint: 'bad',
        direction: '',
        kind: '',
        errors: false,
        q: '',
      }),
    });
    options.request = vi.fn<typeof fetch>().mockImplementation(async (url) => {
      expect(String(url)).toContain('trace=false');
      expect(String(url)).not.toContain('endpoint');
      return response('event: future\ndata: not json\n\n' + page(2));
    });
    options.onEvents = () => setTimeout(() => abort.abort(), 0);
    await expect(runEventStream(options)).rejects.toBeDefined();
    expect(options.cursors.events).toBe(2);
  });
});
