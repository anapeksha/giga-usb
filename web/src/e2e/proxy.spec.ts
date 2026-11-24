import { test, expect } from './fixtures';
import type { ProxyStatus } from '../lib/protocol';

const status: ProxyStatus = {
  enabled: false,
  output_connected: true,
  ready: false,
  release: 'pending',
  session: 1,
  generation: 1,
  keyboard: 'boot',
  mouse: 'absent',
  fault: 'none',
  faults: 0,
  rejected_reports: 0,
  led_failures: 0,
};

test('proxy is opt-in and queued commands never imply readiness', async ({ page, api, appURL }) => {
  api.emit({ type: 'hello', protocol_version: 10 });
  api.emit({ type: 'host', event: { lifecycle: { state: 'ready', session_id: 1 } } });
  api.emit({ type: 'proxy', status });
  await page.goto(appURL);
  const enable = page.getByRole('button', { name: 'Enable proxy', exact: true });
  const disable = page.getByRole('button', { name: 'Disable proxy', exact: true });
  const output = page.getByRole('definition').filter({ hasText: /^Disabled$/ });
  await expect(enable).toBeEnabled();
  expect(api.commands).toEqual(['status']);
  await expect(page.getByText(/Active proxy, not passive capture/)).toBeVisible();
  const enableCommand = { proxy_enable: { generation: status.generation } };
  const release = api.holdCommand(enableCommand);
  await enable.click();
  await expect(disable).toBeDisabled();
  await expect(enable).toHaveAttribute('aria-busy', 'true');
  await expect(output).toBeVisible();
  api.emit({ type: 'proxy', status: { ...status, generation: 2 } });
  await expect.poll(() => api.commands).toEqual(['status', enableCommand]);
  release();
  await expect(disable).toBeEnabled();
  await expect(output).toBeVisible();
  expect(api.commands).toEqual(['status', enableCommand]);
  api.emit({ type: 'proxy', status: { ...status, enabled: true, generation: 2 } });
  await expect(page.getByText('Enabled · not ready', { exact: true })).toBeVisible();
  api.emit({ type: 'host', event: { lifecycle: { state: 'error', session_id: 1 } } });
  api.emit({ type: 'proxy', status: { ...status, enabled: true, generation: 2, fault: 'output' } });
  api.commandStatus = 503;
  await disable.click();
  await expect(page.getByRole('alert').first()).toBeVisible();
  await expect(disable).toBeEnabled();
  expect(api.commands.filter((command) => command === 'proxy_disable')).toHaveLength(1);
  api.commandStatus = 202;
  await disable.click();
  await expect(disable).toBeEnabled();
  await page.getByRole('button', { name: 'Disconnect', exact: true }).click();
  await expect(disable).toBeDisabled();
  await expect(enable).toBeDisabled();
});

test('proxy requires fresh supported status and available output; disable stays available', async ({
  page,
  api,
  appURL,
}) => {
  api.emit({ type: 'hello', protocol_version: 10 });
  api.emit({ type: 'host', event: { lifecycle: { state: 'ready', session_id: 1 } } });
  await page.goto(appURL);
  const enable = page.getByRole('button', { name: 'Enable proxy', exact: true });
  const disable = page.getByRole('button', { name: 'Disable proxy', exact: true });
  await expect(page.getByText('No proxy status reported')).toBeVisible();
  await expect(enable).toBeDisabled();
  await expect(disable).toBeEnabled();
  for (const keyboard of ['unsupported', 'ambiguous', 'absent'] as const) {
    api.emit({ type: 'proxy', status: { ...status, keyboard } });
    await expect(enable).toBeDisabled();
  }
  api.emit({ type: 'proxy', status: { ...status, output_connected: false } });
  await expect(page.getByText('Disconnected or suspended', { exact: true })).toBeVisible();
  await expect(enable).toBeDisabled();
  api.emit({ type: 'proxy', status });
  await expect(enable).toBeEnabled();
  const requests = api.requests.length;
  await page.waitForTimeout(2200);
  expect(api.requests).toHaveLength(requests);
  expect(api.commands).toEqual(['status']);
});

test('old protocol is rejected rather than enabling proxy', async ({ page, api, appURL }) => {
  api.emit({ type: 'hello', protocol_version: 9 });
  await page.goto(appURL);
  await expect(page.getByRole('alert').first()).toContainText('Connection failed');
  await expect(page.getByRole('button', { name: 'Enable proxy', exact: true })).toBeDisabled();
});

test('disabled is not release confirmation and recovery requires reported proof', async ({
  page,
  api,
  appURL,
}) => {
  api.emit({ type: 'hello', protocol_version: 10 });
  api.emit({ type: 'proxy', status });
  await page.goto(appURL);
  await expect(
    page.getByText('Waiting for input release confirmation', { exact: true }),
  ).toBeVisible();
  await expect(page.getByText('Input release confirmed', { exact: true })).toHaveCount(0);
  api.emit({ type: 'proxy', status: { ...status, release: 'failed' } });
  await expect(
    page.getByRole('alert').filter({ hasText: 'Input release unconfirmed' }),
  ).toBeVisible();
  api.emit({
    type: 'proxy',
    status: { ...status, generation: 2, output_connected: false, release: 'failed' },
  });
  await expect(page.getByText('Input release unconfirmed', { exact: true })).toBeVisible();
  api.emit({ type: 'proxy', status: { ...status, generation: 3, release: 'failed' } });
  await expect(page.getByText('Input release unconfirmed', { exact: true })).toBeVisible();
  await expect(page.getByRole('alert')).toContainText('Reset or power-cycle the board');
  await expect(page.getByRole('alert')).toContainText('Reset USB won’t help');
  await expect(page.getByRole('button', { name: 'Enable proxy', exact: true })).toBeDisabled();
  expect(api.commands).toEqual(['status']);
  api.reboot();
  api.emit({ type: 'hello', protocol_version: 10 });
  api.emit({ type: 'proxy', status: { ...status, generation: 0, release: 'released' } });
  await expect(page.getByText('Input release confirmed', { exact: true })).toBeVisible();
  api.emit({
    type: 'proxy',
    status: { ...status, generation: 0, enabled: true, ready: true, release: 'pending' },
  });
  await expect(
    page.getByText('Waiting for input release confirmation', { exact: true }),
  ).toBeVisible();
  await expect(page.getByText('Input release confirmed', { exact: true })).toHaveCount(0);
  await expect(page.getByText(/Reset or power-cycle the board/)).toHaveCount(0);
});
