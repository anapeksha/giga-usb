import { readFile } from 'node:fs/promises';
import { test, expect } from './fixtures';

for (const initial of [true, false]) {
  test(`generic IPC errors are visible ${initial ? 'on connection' : 'while paused'} without USB errors or trace`, async ({
    page,
    api,
    appURL,
  }) => {
    const message = 'IPC transport failed';
    api.emit({
      type: 'host',
      event: {
        status: {
          enabled: false,
          attached: false,
          address: 0,
          transfers: 0,
          errors: 1,
          dropped: 0,
        },
      },
    });
    if (initial) api.emit({ type: 'error', message });
    await page.goto(appURL);
    await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
    const errors = page.getByText('Errors', { exact: true }).locator('..');
    await expect(errors).toHaveText('Errors1');
    if (!initial) {
      await page.getByRole('button', { name: 'Pause display', exact: true }).click();
      api.emit({ type: 'error', message });
    }
    await expect(page.getByRole('alert').filter({ hasText: message })).toBeVisible();
    const toast = page.locator('[data-sonner-toast]').filter({ hasText: message });
    await expect(toast).toHaveCount(1);
    if (initial)
      await expect(
        page.locator('[data-sonner-toast]').filter({ hasText: 'Connected.' }),
      ).toHaveCount(0);
    api.emit({ type: 'error', message });
    await expect.poll(() => api.deliveredCursors.at(-1)).toBe(3);
    await expect(toast).toHaveCount(1);
    if (!initial) await page.getByRole('button', { name: 'Resume display', exact: true }).click();
    await expect(errors).toHaveText('Errors1');
    await expect(
      page.getByText('0 records · newest first · board time', { exact: true }),
    ).toBeVisible();
    const download = page.waitForEvent('download');
    await page.getByRole('button', { name: 'Export trace', exact: true }).click();
    expect(JSON.parse(await readFile((await (await download).path())!, 'utf8'))).toEqual([]);
    await page.getByRole('button', { name: 'Start', exact: true }).click();
    await expect.poll(() => api.commands.includes('start')).toBe(true);
    await expect(page.getByRole('alert').filter({ hasText: message })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Disconnect', exact: true })).toBeVisible();
  });
}
