import assert from 'node:assert/strict';
import test from 'node:test';
import { configureWifi, getWifiStatus, validWifiStatus, validateWifiCredentials } from './wifi';

const signal = () => new AbortController().signal;
const credentials = { ssid: 'Router', password: '' };

test('cancellation reaches in-flight Wi-Fi response bodies', async () => {
  const controller = new AbortController();
  let bodyStarted!: () => void;
  const started = new Promise<void>((resolve) => {
    bodyStarted = resolve;
  });
  const pending = getWifiStatus(controller.signal, async (_url, init) => {
    const body = new ReadableStream<Uint8Array>({
      start(stream) {
        init?.signal?.addEventListener(
          'abort',
          () => {
            stream.error(new DOMException('Aborted', 'AbortError'));
          },
          { once: true },
        );
        bodyStarted();
      },
    });
    return new Response(body);
  });
  await started;
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
});

test('SSID validation counts UTF-8 bytes, preserves spaces, and rejects NUL', () => {
  for (const ssid of ['a', ' '.repeat(32), 'é'.repeat(16), '😀'.repeat(8)])
    assert.equal(validateWifiCredentials({ ...credentials, ssid }), undefined);
  for (const ssid of ['', 'a'.repeat(33), 'é'.repeat(17), 'a\0b'])
    assert.ok(validateWifiCredentials({ ...credentials, ssid }));
});

test('password permits open networks, printable ASCII passphrases, or 64 hex digits only', () => {
  for (const password of ['', ' '.repeat(8), '~'.repeat(63), 'aB09'.repeat(16)])
    assert.equal(validateWifiCredentials({ ...credentials, password }), undefined);
  for (const password of ['1234567', 'g'.repeat(64), 'a'.repeat(65), '1234567\n', 'é'.repeat(8)])
    assert.ok(validateWifiCredentials({ ...credentials, password }));
});

test('runtime Wi-Fi status contract requires saved boolean in every mode and validates optional strings', () => {
  for (const mode of ['setup', 'connecting', 'connected']) {
    for (const saved of [false, true]) {
      assert.ok(validWifiStatus({ mode, saved }));
      assert.ok(validWifiStatus({ mode, saved, ssid: 'Router', ip: '192.168.1.20', error: '' }));
    }
    for (const saved of [undefined, null, 0, 1, 'true', 'false'])
      assert.equal(validWifiStatus({ mode, saved }), false);
    assert.equal(validWifiStatus({ mode }), false);
  }
  for (const value of [
    null,
    [],
    {},
    { mode: 'other', saved: false },
    { mode: ['setup'], saved: false },
    { mode: 'setup', saved: false, ip: 1 },
    { mode: 'setup', saved: true, error: null },
  ])
    assert.equal(validWifiStatus(value), false);
});

test('GET Wi-Fi status is same-origin, uncached and abortable', async () => {
  const abort = signal();
  const request: typeof fetch = async (url, init) => {
    assert.equal(url, '/api/wifi');
    assert.ok(init?.signal instanceof AbortSignal);
    assert.equal(init.signal.aborted, abort.aborted);
    assert.equal(init?.cache, 'no-store');
    return Response.json({ mode: 'setup', saved: true });
  };
  assert.deepEqual(await getWifiStatus(abort, request), { mode: 'setup', saved: true });
  for (const saved of [false, true]) {
    const status = { mode: 'connected', saved, ssid: 'Router', ip: '192.168.1.20' };
    assert.deepEqual(await getWifiStatus(abort, async () => Response.json(status)), status);
  }
  await assert.rejects(
    getWifiStatus(abort, async () => Response.json({ mode: 'connected' })),
    /Invalid/,
  );
  await assert.rejects(
    getWifiStatus(abort, async () => Response.json({ mode: 'bad', saved: false })),
    /Invalid/,
  );
  await assert.rejects(
    getWifiStatus(abort, async () => new Response(null, { status: 503 })),
    /503/,
  );
});

test('POST sends exact credentials, accepts only 202 without parsing body', async () => {
  const abort = signal();
  await configureWifi(credentials, abort, async (url, init) => {
    assert.equal(url, '/api/wifi');
    assert.equal(init?.method, 'POST');
    assert.ok(init?.signal instanceof AbortSignal);
    assert.equal(init.signal.aborted, abort.aborted);
    assert.deepEqual(init?.headers, { 'Content-Type': 'application/json' });
    assert.deepEqual(JSON.parse(String(init?.body)), credentials);
    return new Response(null, { status: 202 });
  });
  for (const [status, error] of [
    [400, /invalid/],
    [409, /no longer available/],
    [200, /200/],
    [500, /500/],
  ] as const)
    await assert.rejects(
      configureWifi(credentials, abort, async () => new Response(null, { status })),
      error,
    );
  await assert.rejects(
    configureWifi({ ...credentials, ssid: '' }, abort, async () => {
      assert.fail('Invalid credentials must not be sent');
    }),
    /network name/,
  );
});

test('validation gives actionable field labels without implementation terms', () => {
  assert.match(validateWifiCredentials({ ssid: '', password: '' })!, /network name/);
  assert.doesNotMatch(validateWifiCredentials({ ssid: '', password: '' })!, /SSID|UTF-8|bytes|NUL/);
  assert.match(validateWifiCredentials({ ssid: 'Router', password: 'short' })!, /Enter 8–63/);
});
