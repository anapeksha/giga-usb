# GigaUSB

A bare-metal Rust USB host and diagnostic tool for the Arduino GIGA R1 WiFi.
Inspect connected devices, HID input and completed host transfers in a browser.
Experimental read-only FAT32 storage support is also included.

GigaUSB observes traffic initiated by its own host controller. It is **not a
passive USB sniffer**: external traffic, handshakes, CRCs and wire timing are not
captured. Compatibility is bounded, and hardware validation remains partial;
see [support limits and validation](AGENTS.md#usb-correctness-and-tracing).

## Experimental local OTA

This branch adds a persistent updater. **RC3 is not OTA-capable:** the first
installation requires SWD and changes the M7 application address. Do not flash
individual application ELFs from this branch over an RC3 installation.

Build a paired local test set from the repository root:

```sh
node web/src/ota/local.ts --output /absolute/path/to/giga-usb/target/local-ota-ready --defmt
```

The output directory must be new. The command creates a **test-only** trust key,
`giga-usb-ota-bootstrap-test.hex` for `bootstrap-test-a`, and an OTA file for
`candidate-test-b`. Never publish the test private key or mix files from different
runs; rebuilding a set creates a different trust anchor.

Initial provisioning requires a separately validated external loader and procedure;
none is supplied here. See the bootstrap requirements below. Only after provisioning:

1. Connect a separate client to `GigaUSB`, open the dashboard and expand
   **Firmware update**. Check status: `bootstrap-test-a` must be confirmed.
2. Select `giga-usb-candidate-test-b.ota.bin` and start the upload. USB-A and proxy
   forwarding stop during staging. Wait for verification, then choose
   **Install and restart**.
3. Rejoin the AP if necessary. Success requires **`candidate-test-b` and confirmed**,
   not just HTTP acceptance or the appearance of the dashboard.

The HEX is for initial SWD provisioning; the `.ota.bin` is for dashboard upload.
Initial provisioning is **not power-fail-safe**. On-board boot, watchdog, OTA
transfer and physical rollback remain **hardware-unvalidated**. Retain SWD recovery.
See [OTA engineering details](AGENTS.md#local-ota-architecture).

## Build

Requires stable Rust, the `thumbv7em-none-eabihf` target and Node.js 24 LTS.
From the repository root:

```sh
(cd web && npm ci && npm run check)
cargo build -p m4 --release --features defmt --locked
cargo build -p m7 --release --features defmt --locked
```

The single Cargo workspace has four members: `firmware/m7`, `firmware/m4`,
`firmware/shared` and `firmware/loader` (package `loader`).
Build each core and the standalone loader separately; do not use `--workspace`
or `--all-features` for firmware builds. The M7 package also builds `bootloader`
with its separate linker layout; that binary is not a workspace member. Normal builds use the pinned production public key; the local test builder
explicitly compiles both images with its generated test key and build IDs.
Cargo’s `OUT_DIR` holds build-generated Rust included at compile time. The public
key and build ID are compiled into firmware; production firmware does not read
runtime environment variables. No private key is embedded.
See [engineering checks](AGENTS.md#builds-and-tooling) for full validation.

## External dashboard assets

The web build produces `dist/index.html.gz`, `dist/dashboard.qspi.bin` and
`dist/dashboard.qspi.bin.json`. The dashboard is not embedded in M7. In the OTA layout, two external slots retain
old/new assets; signed release metadata binds each application image to its matching
dashboard. Use the complete OTA bundle rather than replacing the live container.
The bootloader fails closed if no authenticated firmware/assets pair exists.

The container targets external offset `0xF00000`, not internal flash. Never pass
it to a normal firmware download command. See [format and provisioning](AGENTS.md#external-dashboard-provisioning)
for the header, offline preparation tool, part-specific loader and readback checks.
On 2026-11-22, the authorized internal/external erase and reflash completed with
verified asset readback and firmware startup. On-board HTTP/browser validation
remains pending; see the dated evidence in `AGENTS.md`. Future destructive updates
require separate authorization.

## Intel HEX bootstrap and recovery

The Node installer has been removed. Node 24 tools in `web/src/ota/` build
offline artifacts only; they do not access the board. `build.ts bootstrap` emits
`bootstrap.hex` and `BOOTSTRAP-WARNING.txt`; `local.ts` emits the test HEX named above.

| HEX address | Contents |
| --- | --- |
| `0x08000000` | Bootloader, padded to 128 KiB with `0xFF` |
| `0x08020000` | Combined ACTIVE, `0x1E0000` bytes (M7 and M4, padded) |
| `0x90200000` | Factory state journal |
| `0x90202000` | Allocation record |
| `0x90220000` | Signed manifest (metadata A) |
| `0x90F00000` | Dashboard container (slot A) |

External addresses mean **`0x90000000 + NOR offset`**, not initialized memory.
**No validated compatible external loader is supplied.** Programming requires an
actual-part- and board-routing-specific compatible loader validated separately.
There is no safe flashing command provided here. HEX neither initializes QSPI nor
checks existing allocations, takes backups, defines erase policy or guarantees
header-last programming. Unrepresented bytes are **not** implicitly erased;
slot B, metadata B and scratch are not populated by this HEX. Independently
authenticate artifacts, establish allocation consent/backups and verify readback
with a reviewed provisioning procedure before attempting board access.

First provisioning is **not power-fail-safe**; retain an external SWD recovery
path. It overwrites the Arduino bootloader. Do not mass erase or change option
bytes, protection or OTP/security settings. Partial application ELF downloads do
not establish a valid signed OTA installation. Historical RC3 installer results
do not validate this HEX path.

### External loader qualification candidate

`firmware/loader/` is the standalone `loader` workspace
package. Its unsafe hardware boundary is isolated from the applications, which
continue to forbid unsafe code. From the repository root (native target below
is for Apple silicon; use `x86_64-unknown-linux-gnu` on Linux):

```sh
cargo test -p loader --target aarch64-apple-darwin --locked
cargo clippy -p loader --target aarch64-apple-darwin --all-targets --locked -- -D warnings
cargo clippy -p loader --target thumbv7em-none-eabihf --release --locked -- -D warnings
cargo build -p loader --target thumbv7em-none-eabihf --release --locked
cp target/thumbv7em-none-eabihf/release/loader target/GIGA_AT25_READONLY_UNQUALIFIED.stldr
```

The package build script supplies loader-only linker flags on the Thumb target;
native binary tests exclude all hardware code and native builds have a no-op
entry point. The `.stldr` copy retains the ELF's symbols and load segments; do
not use a raw objcopy binary. Record `rustc -Vv` for reproducibility; stable is
a moving toolchain. These commands do not access hardware. Read-only board
qualification passed on 2026-11-26 with CubeProgrammer 2.24.0. Default builds
still refuse Write, SectorErase and MassErase. A separately opted-in `programming`
build exists for controlled qualification; it is not yet qualified for the complete
bootstrap HEX.

ABI provenance:

- Official [ST memory loaders](https://github.com/STMicroelectronics/stm32-memory-loaders/tree/c5e1ecba29d9a93cec5fbb76ff4bae8e46bea042),
  commit `c5e1ecba29d9a93cec5fbb76ff4bae8e46bea042`: H747I-DISCO
  `MT25TL01G` loader `Dev_Inf.h`, `Dev_Inf.c` and `Loader_Src.c` establish
  metadata layout and callback conventions. Native/linker checks enforce the
  200-byte metadata; `StorageInfo` is retained in `.Dev_Info`, with `Device_Info`
  as an alias. No ST implementation is copied.
- Official installed CubeProgrammer **2.24.0** SPI example
  `bin/ExternalLoader/M25P64_STM3210E-EVAL.stldr`, SHA-256
  `330444f0914f771397daf2f5a7aa1237a871cbe782a0bbefdeb6d1001a86249d`:
  DWARF identifies `int Read(uint32_t Address, uint32_t Size, uint8_t *buffer)`
  at `Sources/Loader_Files/Loader_Src.c:37`. Disassembly passes those arguments
  to SPI command `03`, decrements Size per byte stored, and returns 1 on success.
  This confirms the existing Rust Read signature and byte units.

**Tool allocation evidence is specific to the observed Cube run, not an API
contract.** The guarded candidate completed Init with the required JEDEC/status
checks. A subsequent 256-byte Read supplied output `0x24000E80` and MSP
`0x24000E50` with the first load at `0x24000004`, length `0xA50`; the former fixed
buffer arena rejected it. Read now requires output at least `__loader_code_end +
0x400`, a nonempty range ending at or below `0x24070000`, and sampled in-callback
MSP aligned to eight bytes, at least `__loader_code_end + 0x100` and at most
`output - 32`. Checked arithmetic rejects overflow. The linker derives
`__loader_code_end` from the first loaded segment, including a file-backed tail,
before `.Dev_Info`; the observed tool does not load that separate metadata segment.
A later CubeProgrammer 2.24 run with loaded end `0x24000B90` supplied output
`0x24000FC0`, entry MSP `0x24000F8C` and in-callback MSP `0x24000F54`, failing
only eight-byte alignment. Measured entry MSP is loaded end plus `0x3FC`.
The ELF-loaded tail aligns to eight bytes then appends a zero word, with a linker
assertion that loaded end is four modulo eight. This preserves the runtime
alignment check and aligns entry MSP for that measured placement, not a universal
tool allocation contract. The padded build subsequently passed board reads.
Read diagnostics retain address, length, output and sampled MSP. Allocation must
be rechecked for each build/tool; neither the observed spacing nor compiled stack
review is a formal stack bound. There is no startup,
heap or data/BSS initialization.

The hardware boundary was reviewed against `firmware/m7/installer/loader.rs`.
Init/Read first require privileged M7 thread/MSP before masking interrupts locally;
the caller's exact entry PRIMASK is restored on every return path. Before GPIO
access, the unchanged gate requires PRIMASK=1 under that scoped mask,
M4 never released (BOOT_C2=0 and current BCM4=0), HSI ready/DIV1 and selected,
canonical D1 dividers, caches/MPU disabled and QUADSPI clock gated. Caller must
establish a reviewed reset/halt procedure, valid stack, and exclusion of other
bus masters/watchdogs; checks alone do not establish these conditions. No safe
board-invocation command is supplied.

GPIO mode 0 uses PD11 MOSI, PD12 MISO, PE2/PF6 IO2/IO3 high, PF10 CLK and PG6 CS.
The read-only path uses `9F` (exact JEDEC `1f8901`), `05` (reject WIP/WEL) and
bounded `03` reads; CS releases every 256 bytes. JEDEC is not full part/SFDP
qualification. Verify/CheckSum are omitted; tool fallback support is unqualified.
Read-only qualification completed two
independent full-chip reads in 256-KiB requests; their 16-MiB outputs matched
byte-for-byte (SHA-256 `8bc0b9af538b99dd3b40b2969d4646d0865a568261bc83b65aad646a1b537460`).
A single 16-MiB callback request was rejected by the bounded buffer policy.
Internal 2-MiB readback and 46 displayed option fields were unchanged, then a
system reset returned control to existing firmware. Logs, snapshots and backups
are under `target/external-loader-qualification/`. These local snapshots do not
replace durable backups outside the repository before programming. Init failure
coverage, complete programming/erase, power-failure and OTA qualification remain
outstanding. The loader cannot authenticate HEX or enforce image ordering.

The tooling-only `programming` feature does not change application features or
unsafe boundaries. Run the same loader checks above with `--features programming`
to test/build the opt-in candidate. Preserve each exact ELF separately: both modes
overwrite the same Cargo binary output. MassErase refuses in every mode.

Programming is scoped to NOR offsets `0x200000..0x203000`, `0x220000..0x221000`
and dashboard A `0xF00000..0x1000000`; the host must further restrict writes to
verified image sectors. Write accepts at most 4096 bytes, preflights one-to-zero
transitions, splits on 256-byte pages and verifies readback. SectorErase accepts
one aligned 4-KiB sector, with the last argument either its base (observed Cube
2.24 convention) or its inclusive last byte. Multisector requests refuse.

Command/status masks follow Renesas AT25SF128A/AT25QF128A datasheet
DS-AT25SF/QF128A-168 Rev Q (2025-08-26), using only `06/04` WREN/WRDI, `02` page
program, `20` sector erase and `05/35/15` status. No status, OTP/security, chip-erase
or option-byte writes exist. DWT must advance before WREN; each busy wait has a
64-million-cycle deadline at gated 64-MHz HSI plus a 4096-poll limit. Timeout can
leave NOR busy or earlier pages written; do not blindly retry or resume firmware.

Hardware qualification programmed a 513-byte page-crossing pattern in the allocated
journal sector at `0x90200000`; independent full-sector readback matched exactly.
Cube's subsequent erase call used identical start/end sector bases and was safely
refused by the earlier envelope policy. The corrected policy passes native tests,
but the retry found no ST-LINK. **The test pattern has not been erased and no OTA
bootstrap has been installed.** Reconnect the ST-LINK before resuming erase/readback
qualification. The CMSIS-DAP probe currently detected by probe-rs is not the
CubeProgrammer ST-LINK transport. Verified backup copies are also retained outside
the repository at `../giga-usb-backups/ota-bootstrap-2026-11-26/`.

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

Supported FAT32 drives appear in a collapsible root file browser with file sizes
and read-only downloads (8.3 aliases, up to 32 entries, files up to 64 KiB).
Folders are shown but cannot be opened; files above the limit cannot be downloaded.
No writing or formatting is supported.
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
