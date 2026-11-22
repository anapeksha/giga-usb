# GigaUSB

A bare-metal Rust USB host and diagnostic tool for the Arduino GIGA R1 WiFi.
Inspect connected devices, HID input and completed host transfers in a browser.
Experimental read-only FAT32 storage support is also included.

GigaUSB observes traffic initiated by its own host controller. It is **not a
passive USB sniffer**: external traffic, handshakes, CRCs and wire timing are not
captured. Compatibility is bounded, and hardware validation remains partial;
see [support limits and validation](AGENTS.md#usb-correctness-and-tracing).

## Build

Requires stable Rust, the `thumbv7em-none-eabihf` target and Node.js 24 LTS.
From the repository root:

```sh
(cd web && npm ci && npm run check)
cargo build -p m4 --release --features defmt --locked
cargo build -p m7 --release --features defmt --locked
```

Build each core separately; do not use `--workspace` or `--all-features` for
firmware builds. See [engineering checks](AGENTS.md#builds-and-tooling) for full validation.

## External dashboard assets

The web build produces `dist/index.html.gz`, `dist/dashboard.qspi.bin` and
`dist/dashboard.qspi.bin.json`. The dashboard is not embedded in M7; UI-only
updates require provisioning the external container, not rebuilding firmware.
Without valid assets M7 serves a small HTTP 503 notice; diagnostics/API remain available.

The container targets external offset `0xF00000`, not internal flash. Never pass
it to a normal firmware download command. See [format and provisioning](AGENTS.md#external-dashboard-provisioning)
for the header, offline preparation tool, part-specific loader and readback checks.
On 2026-11-22, the authorized internal/external erase and reflash completed with
verified asset readback and firmware startup. On-board HTTP/browser validation
remains pending; see the dated evidence in `AGENTS.md`. Future destructive updates
require separate authorization.

## Flash

An external SWD probe is required. Read the
[hardware setup and boot options](AGENTS.md#hardware-setup-and-debugging) first.
**Flashing M7 overwrites the Arduino bootloader.** Release any debugger using the
probe, then flash matching images in this order:

```sh
probe-rs download --chip STM32H747XI --speed 1000 target/thumbv7em-none-eabihf/release/m4
probe-rs download --chip STM32H747XI --speed 1000 target/thumbv7em-none-eabihf/release/m7
probe-rs reset --chip STM32H747XI
```

Verify programming and use a full system reset. Routine updates use sector-scoped
flashing, not mass erase. Recovery and CubeProgrammer instructions are in the
[engineering guide](AGENTS.md#flash-log-and-debug).

## Use

1. Power the board normally. Join the open **GigaUSB** network from a separate
   phone or laptop and open **http://192.168.4.1/**. The board provides DHCP,
   not Internet access.
2. Select **Connect**, attach a supported peripheral to **USB-A**, then select
   **Start** to power the port. **Never connect USB-A to another computer.**
   Respect the board's power limits.
3. Inspect topology, input and traces. **Stop** disables the host;
   **Disconnect** only closes the dashboard stream. Exports may contain sensitive input.

Networking is AP-only: no router setup, password entry or credential saving.
Do not change the development Mac's Wi-Fi; use a separate client for testing.
The AP is open and HTTP is unauthenticated and unencrypted. Use a trusted environment.
Removing historical credential support does not securely erase old credentials.

The trace retains the latest 500 USB records; JSON exports are oldest-first.
**Pause display** does not stop capture; **Clear trace** clears browser history only.
Filters/search run on the board's latest 64 retained records and new arrivals,
not older browser-only history. They do not change capture or the unfiltered export.
Endpoint addresses are decimal (`0` is EP0 OUT, `128` is EP0 IN); IN means device
to board. Search is limited to 96 UTF-8 bytes. Optional columns and expandable
records expose transfer details; identification is a likely ID/class match or
reported storage text, not authenticated make/model detection.

Supported FAT32 drives can be listed and files downloaded read-only (root 8.3
aliases, up to 32 entries, files up to 64 KiB). No writing or formatting is supported.
**Request queued** means acceptance, not execution; completion arrives through events.
A single SSE stream reconnects transient failures with processed cursors; a continuity
reset clears stale board state. For connection problems, LED meanings, detailed
limits and dated validation evidence, see [AGENTS.md](AGENTS.md).

## Dashboard development

SolidJS/TypeScript, Tailwind v4 and source-copied [solid-ui](https://www.solid-ui.com/docs)
components live under `web/src/`. From `web/`:

```sh
npm ci
npm run dev
```

Open the URL printed by Vite. `/api` proxies to `http://192.168.4.1`; the dev
server does not simulate a board.

| Command | Purpose |
| --- | --- |
| `npm run check` | Formatting, lint, types, unit tests and production build |
| `npm run check:all` | All checks, including Chromium browser tests |
| `npm run build` / `npm run preview` | Build / preview production assets |
| `npm test` / `npm run test:e2e` | Unit tests / build and browser tests |
| `npm run typecheck` | TypeScript checks |
| `npm run lint` / `npm run lint:fix` | ESLint checks / fixes |
| `npm run format:check` / `npm run format` | Prettier checks / formatting |

Install the matching browser after `npm ci` and Playwright updates:

```sh
npx playwright install chromium
npm run check:all
```

On Linux use `npx playwright install --with-deps chromium`. Tests in `web/src/e2e/`
serve the real gzip bundle on loopback with intercepted API fixtures and axe
accessibility checks. They do not validate hardware or change Wi-Fi. Failure
traces remain in ignored `web/test-results/`; `npm run check` needs no browser.

## Offline USB registry

`web/src/lib/usb.ids` contains the complete pinned 2026.06.26 snapshot (3,427
vendors, 20,537 products), SHA-256
`f5a48b0cc8dae1607c2f0bae6b8dc13f2ecef69dbeaeaf34b4be8e280d34dba4`.
[USB-IDS-LICENSE.txt](USB-IDS-LICENSE.txt) contains provenance and BSD-3-Clause
terms and is included in the dashboard and release archive. No runtime Internet
registry access is used. Preserve the original bytes (including malformed UTF-8),
scoped lookups and notices; update provenance, hash, counts and tests together.
