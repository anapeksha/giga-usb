# Dashboard development

A SolidJS and TypeScript dashboard, styled with Tailwind v4 and solid-ui
components. Use Node.js 24 LTS and a current browser.

## Use the dashboard

1. Power on GigaUSB. It creates the open **GigaUSB** Wi-Fi network; no router is needed.
2. Join that network from a separate phone or laptop, then open **http://192.168.4.1/**.
   The board assigns your client an address automatically using leasehund DHCP.
   This network does not provide Internet access.
3. Select **Connect**, plug a USB peripheral into the board's USB-A port, then select
   **Start** to power the port and view USB activity. Never connect USB-A to another computer.
4. Use **Stop** to turn off the USB host. **Disconnect** only disconnects the dashboard;
   it does not stop the host.

GigaUSB stays on its own network. There is no router sign-in or saved Wi-Fi profile.
The network is open and the dashboard uses unencrypted HTTP without authentication;
use it in a trusted environment. Trace exports may contain sensitive USB input.

The dashboard includes device details, input reports, trace filters and JSON export.
Supported FAT32 USB drives can be listed and files downloaded read-only; the dashboard
cannot write or format a drive. Trace records describe this board's USB transactions,
not passive capture of traffic between other devices.

USB trace retains the latest 500 transfer, transfer-context, diagnostic and typed
USB host-error records. Network updates, status snapshots, Ready/lifecycle logs,
parsed input/descriptors, storage replies and unscoped processor errors still update
dashboard state but do not consume trace history or appear in its JSON export.
Exports are oldest-first. A browser-local `sequence` field counts all validated
arrivals, including omitted records, to prevent false context/packet adjacency.
Expanded packet details stay open while polling and when new records arrive.

The accordion, combobox and checkbox wrappers in `src/components/ui/` are
source-copied from the [solid-ui registry](https://github.com/stefan-karger/solid-ui/tree/main/apps/docs/src/registry/ui)
and adapted to the existing dark theme, focus styles and labeled controls using
`@kobalte/core`. The existing table wrapper is also source-copied solid-ui.
These are local components, not an additional component-library dependency.
See the official [accordion](https://www.solid-ui.com/docs/components/accordion),
[combobox](https://www.solid-ui.com/docs/components/combobox),
[checkbox](https://www.solid-ui.com/docs/components/checkbox) and
[table](https://www.solid-ui.com/docs/components/table) documentation.

## Develop locally

From `web/`:

```sh
npm ci
npm run dev
```

Open the localhost URL printed by Vite. The development server proxies `/api`
to `http://192.168.4.1`; it does not simulate a board.

## Scripts

| Command                | Purpose                                             |
| ---------------------- | --------------------------------------------------- |
| `npm run check`        | Check formatting, lint, types and tests, then build |
| `npm run build`        | Build the production bundle                         |
| `npm run preview`      | Preview the production build locally                |
| `npm run typecheck`    | Check TypeScript types                              |
| `npm test`             | Run Vitest unit tests                               |
| `npm run test:e2e`     | Build and run Playwright Chromium tests             |
| `npm run check:all`    | Run all checks, including browser tests             |
| `npm run lint`         | Check ESLint rules                                  |
| `npm run lint:fix`     | Apply available lint fixes                          |
| `npm run format:check` | Check Prettier formatting                           |
| `npm run format`       | Format files                                        |

Install the matching managed browser once after `npm ci` (and again when Playwright changes):

```sh
npx playwright install chromium
npm run check:all
```

On Linux CI, use `npx playwright install --with-deps chromium` for browser system dependencies.
Browser tests in `src/e2e/` serve the actual gzip production bundle on loopback,
intercept all API calls with local fixtures, and run rendered axe accessibility checks.
They do not connect to the board, change Wi-Fi, or validate hardware. Failure traces
are saved under ignored `test-results/`. `npm run check` does not require a browser.

Application code and tests live in `src/`. The build writes the single-file
bundle to repository-root `dist/index.html.gz`. Build it before M7 and rebuild
M7 after dashboard changes.

See [the project README](../README.md) to run GigaUSB and
[AGENTS.md](../AGENTS.md) for engineering contracts and validation requirements.
