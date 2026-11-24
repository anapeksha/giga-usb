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

## Verified installer

RC3 and later releases include one `giga-usb-TAG.installer.mjs` containing the signed
firmware, dashboard and part-specific RAM loader. It runs on the **computer**;
never pass this file or the signed container to a flash programmer as firmware.
Runtime requirements are **Node.js 24+, STM32CubeProgrammer and an external SWD
probe**, on macOS or Linux. Rust, imgtool and Internet access are not needed.

Use a trusted copy of the installer and obtain the public signing key through an
independently trusted channel. A public key downloaded beside an untrusted file
is not sufficient. Verification cannot authenticate bootstrap code already running.

```sh
# Verification only: no board access.
node giga-usb-TAG.installer.mjs --key trusted-signing-public.pem

# Replace paths, probe serial and TAG with the actual values.
node giga-usb-TAG.installer.mjs --key trusted-signing-public.pem --install \
  --programmer /absolute/path/to/STM32_Programmer_CLI --probe STLINK_SERIAL \
  --backup-dir /absolute/path/to/durable-backups --replace-dashboard
```

The installer authenticates all assets before board access, takes two matching
full external-flash backups plus internal/option backups, programs only allocated
dashboard sectors with the header last, installs **M4 then M7**, verifies readback
and preservation, compares option settings, then system-resets. Identical assets
are not rewritten. Arbitrary occupied external data is rejected. It never enables
proxy forwarding or changes protection, option bytes or OTP/security areas.

Failures retain evidence and attempt to halt instead of booting uncertain images.
Do not blindly retry writes: inspect the saved recovery instructions. For an exact
staged image only, `--resume-from /absolute/prior-install-directory` validates the
prior backups and fresh flash before committing the header and finishing. It is
not general rollback. Keep backups outside the repository on durable storage.

The normal full-install and interrupted-header recovery paths passed on the
current board using test-signed development images. This does not establish
on-board HTTP rendering or HID forwarding, or validate a published release image.

## Flash manually

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

## Keyboard and mouse proxy

Proxying is **off by default**. USB-C exposes serial diagnostics plus generic boot
keyboard and mouse interfaces; no keyboard or mouse input is sent until you
explicitly enable forwarding.

1. Connect **USB-C** to the computer that should receive input. Connect the
   physical keyboard, mouse or supported receiver to **USB-A**—never reverse
   these connections or connect USB-A to a computer.
2. From a separate AP client, open the dashboard, connect and start the host.
3. Check the reported keyboard/mouse support and USB-C availability, then select
   **Enable proxy**. Wait for **Ready** before exercising non-sensitive test input.
4. Select **Disable proxy** to stop forwarding and request neutral key/button
   reports. A queued response or disabled state is not release confirmation;
   check the separately reported input-release status.

This is an **active proxy**, not passive USB sniffing: the computer sees emulated
GigaUSB devices, not the peripheral's original identity, descriptors or timing.
Supported input is one boot keyboard (modifiers and six keys) and one boot mouse
(three buttons and signed relative X/Y). There is no wheel, report-ID forwarding,
non-boot forwarding, hub support or aggregation of duplicate boot interfaces.
Keyboard Num/Caps/Scroll LED output is forwarded to the selected physical keyboard.

Stop, detach, session changes, suspend and proxy faults invalidate forwarding;
reenabling requires a new explicit request. Queues are bounded: consumer stalls
fail closed rather than promising lossless forwarding. On uncertain USB-C output,
HID and serial diagnostics shut down until the **physical board reset button or a
power cycle**. **Reset USB** does not recover USB-C; HTTP diagnostics remain
available. A disconnected/unresponsive computer cannot acknowledge a release.

Proxy software tests pass, but actual PC enumeration, forwarding, LED behavior
and fault recovery are **not yet hardware-validated**. Flash both matching core
images and update the dashboard together: this feature uses protocol 10 and is
not compatible with the protocol-9 RC images.

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
