export interface WifiStatus {
  mode: 'setup' | 'connecting' | 'connected';
  saved: boolean;
  ssid?: string;
  ip?: string;
  error?: string;
}

export interface WifiCredentials {
  ssid: string;
  password: string;
}

export function validateWifiCredentials(value: WifiCredentials): string | undefined {
  const length = new TextEncoder().encode(value.ssid).length;
  if (length < 1 || length > 32 || value.ssid.includes('\0'))
    return 'Enter a network name of up to 32 characters (fewer for some languages), without null characters.';
  if (
    value.password !== '' &&
    !/^[\x20-\x7e]{8,63}$/.test(value.password) &&
    !/^[0-9a-fA-F]{64}$/.test(value.password)
  )
    return 'Enter 8–63 letters, numbers, spaces or basic symbols, or a 64-digit hexadecimal key. Leave blank for an open network.';
}

export function validWifiStatus(value: unknown): value is WifiStatus {
  if (typeof value !== 'object' || value === null) return false;
  return (
    'mode' in value &&
    typeof value.mode === 'string' &&
    ['setup', 'connecting', 'connected'].includes(value.mode) &&
    'saved' in value &&
    typeof value.saved === 'boolean' &&
    ['ssid', 'ip', 'error'].every(
      (key) => !(key in value) || typeof (value as Record<string, unknown>)[key] === 'string',
    )
  );
}

async function wifiRequest<T>(
  signal: AbortSignal,
  operation: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const controller = new AbortController();
  const abort = () => controller.abort();
  signal.addEventListener('abort', abort, { once: true });
  if (signal.aborted) controller.abort();
  const timer = setTimeout(abort, 8000);
  try {
    return await operation(controller.signal);
  } finally {
    clearTimeout(timer);
    signal.removeEventListener('abort', abort);
  }
}

export async function getWifiStatus(
  signal: AbortSignal,
  request: typeof fetch = fetch,
): Promise<WifiStatus> {
  return wifiRequest(signal, async (requestSignal) => {
    const response = await request('/api/wifi', { signal: requestSignal, cache: 'no-store' });
    if (!response.ok) throw new Error(`Wi-Fi status failed: HTTP ${response.status}`);
    const value: unknown = await response.json();
    if (!validWifiStatus(value)) throw new Error('Invalid Wi-Fi status response');
    return value;
  });
}

export async function configureWifi(
  credentials: WifiCredentials,
  signal: AbortSignal,
  request: typeof fetch = fetch,
): Promise<void> {
  const error = validateWifiCredentials(credentials);
  if (error) throw new Error(error);
  await wifiRequest(signal, async (requestSignal) => {
    const response = await request('/api/wifi', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(credentials),
      signal: requestSignal,
    });
    if (response.status === 400)
      throw new Error('The board rejected the Wi-Fi credentials as invalid.');
    if (response.status === 409)
      throw new Error('Wi-Fi setup is no longer available. Refresh Wi-Fi status.');
    if (response.status !== 202) throw new Error(`Wi-Fi setup failed: HTTP ${response.status}`);
  });
}
