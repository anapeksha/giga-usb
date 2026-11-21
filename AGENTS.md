# GigaUSB engineering guide

## Purpose

GigaUSB is a bare-metal Rust USB host and diagnostic tool for the Arduino GIGA
R1 WiFi (STM32H747XI). Build the host controller driver and USB protocol stack
here. Do not replace them with an existing USB host stack.

Capture means observing traffic initiated by this board. This hardware and
firmware cannot passively sniff the link between two external USB devices.
Do not describe completed host transactions as wire-level packet capture:
handshakes, CRCs, bus timing and arbitrary external traffic are not captured.

## Workspace and ownership

- Keep one Cargo workspace: `firmware/m7`, `firmware/m4`, `firmware/shared`.
- M7 owns USB-A host control, scheduling, capture, Wi-Fi and HTTP.
- M4 owns descriptor/HID processing and secondary USB-C CDC diagnostics.
- Shared contains only protocol, IPC, logging and Wi-Fi contracts, USB types,
  and shared HAL metadata. Its only feature is `defmt`; there are no `m7`/`m4`
  selectors.
- M7's native-testable library owns `firmware/m7/src/{status,network_bus,ipc_delivery}.rs`
  and `firmware/m7/src/usb/{host,host_logic,host_storage,storage}.rs`.
  `firmware/m7/src/boot.rs` owns M7-specific boot coordination and M4 validation.
- M4's native-testable library owns `firmware/m4/src/ui_bus.rs` and
  `firmware/m4/src/usb/{processor,hid}.rs`;
  `firmware/m4/src/boot.rs` owns M4-specific boot adoption.
- M7 owns USB_OTG_HS with its internal full-speed PHY, PB14/PB15, PA15 VBUS,
  SDMMC1, PB10, PC8–PC12/PD2, RGB LED PI12/PJ13/PE3 and TIM2.
- M4 owns USB_OTG_FS, PA11/PA12 and TIM5. Never access one controller from both cores.
- Embassy provides cooperative executors, timers, HAL support and USB device
  transport. `giga-r1` provides board Wi-Fi support, IPC queues and MPU setup.
- Both applications forbid unsafe code. M7 uses safe typed PAC access rather
  than raw MMIO. Keep peripheral ownership explicit; do not move core-specific
  hardware access into shared.
- Shared HAL metadata remains genuinely shared. Preserve its required linker
  attributes and the shared-memory reservations.

## Builds and tooling

Use stable Rust and `thumbv7em-none-eabihf`. Build/check each core separately;
Cargo cannot unify the CM7 and CM4 HAL chip features. Never use `--workspace`
or `--all-features` for firmware builds. Keep hardware dependencies behind safe
ownership boundaries; shared has no core-selector features.
`defmt` is the only optional application behavior. Logging is centralized in
`shared::logging` and calls a no-op when disabled.

Build the web bundle before M7; firmware uses `include_bytes!` on
`dist/index.html.gz`. Production firmware uses release builds; debug firmware
can exceed flash capacity. Preserve the single-file gzip build and serve it as
HTML with `Content-Encoding: gzip`. Keep JSON endpoints uncompressed unless an
explicit implementation and tests are added.

```sh
(cd web && npm ci && npm run check)
cargo fmt --all -- --check
cargo clippy -p m7 --release --features defmt --locked -- -D warnings
cargo clippy -p m4 --release --features defmt --locked -- -D warnings
cargo test -p shared --target aarch64-apple-darwin --lib --locked
cargo test -p m7 --target aarch64-apple-darwin --lib --locked
cargo test -p m4 --target aarch64-apple-darwin --lib --locked
cargo build -p m4 --release --features defmt --locked
cargo build -p m7 --release --features defmt --locked
```

The host test target above is for Apple silicon; use the actual host target on
other machines. Keep the lockfiles reproducible. Also check logging-disabled
firmware when changing dependencies or the logging facade.

Zed uses its managed rust-analyzer. Package-specific compiler checks are required;
full HAL semantic analysis still has a Cargo workspace metadata feature-unification
limitation. Do not hide diagnostics or silently split the workspace. Keep only
the cargo-embed server task; CodeLLDB attaches to its GDB endpoint. Cargo-embed
is a GDB server, not a DAP server.

### GitHub CI and signed releases

`.github/workflows/ci.yml` runs on pull requests and merge queues, without
signing secrets. It checks the complete dashboard (including Chromium fixtures),
Rust formatting, native libraries on Linux, and separate per-core release Clippy
and builds with and without `defmt`. In the default-branch ruleset, require the
**Validate** check and pull requests before merging. Workflow files alone do not
enable repository rulesets. These checks are software evidence, not board tests.

`.github/workflows/release.yml` runs on GitHub Release `created`, checks out
that release's tag, and builds matching logging-enabled images and dashboard.
It packages both ELFs, the gzip UI, checksums and build identity, then signs and
verifies the archive with `imgtool` 2.5.0. `softprops/action-gh-release@v3` uploads
only release assets, without replacing existing assets; no Actions artifacts are
uploaded. Actions use major-version references. Tags are not semver-validated:
asset names include a filename-safe tag (non-ASCII/non-alphanumeric characters
other than `.`, `_` and `-` become `-`), while signed `BUILD.txt` retains the exact
tag and commit. The non-bootable imgtool container uses version `0.0.0`; it is
not an application version or anti-rollback counter.

GitHub does not trigger `created` workflows for draft releases. Create a release
directly rather than saving a draft first; use `published` instead if draft-to-
published transitions must be covered. This post-creation asset upload is not
compatible with immutable releases: those require a draft-first upload workflow
before publication.

Before releasing, create the **release** GitHub environment with required
reviewers and deployment restrictions allowing only trusted release tags. Protect
release-tag creation/update/deletion with a repository tag ruleset. A release trigger
alone does not prove the commit passed PR checks: release only reviewed commits
from the protected default branch. Restrict who can modify workflows and approve
releases. Add these environment settings:

- Secret **FIRMWARE_SIGNING_KEY**: the complete, unencrypted Ed25519 private PEM.
- Secret **FIRMWARE_SIGNING_PUBLIC_KEY**: its public PEM, maintained independently
  so verification fails if the secret is replaced with a mismatched key.

Generate the production key offline on a trusted machine, never in CI or inside
the repository, and keep a secure backup. For example, with `imgtool` installed:

```sh
python3 -m venv signing-tools
signing-tools/bin/pip install imgtool==2.5.0
umask 077
signing-tools/bin/imgtool keygen --type ed25519 --key signing-private.pem
signing-tools/bin/imgtool getpub --key signing-private.pem --encoding pem --output signing-public.pem
```

In GitHub Settings → Environments → release → Environment secrets, add each
complete PEM file under the secret names above. Alternatively, from that trusted
machine with GitHub CLI authenticated:

```sh
gh secret set FIRMWARE_SIGNING_KEY --env release --repo anapeksha/giga-usb < signing-private.pem
gh secret set FIRMWARE_SIGNING_PUBLIC_KEY --env release --repo anapeksha/giga-usb < signing-public.pem
```

Both PEMs are stored in Secrets; only the public PEM is intentionally included
in release assets. Never upload the private PEM as a release asset or issue.
Publish the public-key fingerprint through an independently trusted channel;
trusting a public key downloaded alongside an untrusted release is insufficient.
The workflow removes its temporary private-key file on exit. Do not enable shell
tracing for signing steps or expose the release environment to PR jobs.

**This is distribution signing, not secure boot.** The board has no MCUboot
verifier, and its vector tables must stay at the existing flash bases. Arduino
CLI signing assumes an Arduino build/upload flow that this Rust project does not
use. Instead, `giga-usb-<tag>.signed.bin` is an explicitly non-bootable MCUboot-format
container around `giga-usb-<tag>.tar.gz`, with a 512-byte header. Its 64-MiB container
limit is not a flash slot or permission to flash it. Never flash the container:
verify it, compare its signed payload with the archive, then extract the original
ELFs and follow the M4-then-M7 SWD procedure. With an independently trusted public
key and all release assets in the current directory (replace `v1.0.0` with the
filename-safe tag from the downloaded asset):

```sh
asset=giga-usb-v1.0.0
imgtool verify --key trusted-signing-public.pem "$asset.signed.bin"
# Compare the archive to the exact payload covered by the verified signature.
dd if="$asset.signed.bin" bs=1 skip=512 count="$(wc -c < "$asset.tar.gz")" | cmp - "$asset.tar.gz"
mkdir firmware-release
tar -xzf "$asset.tar.gz" -C firmware-release
(cd firmware-release && sha256sum -c SHA256SUMS)
```

Stop if any verification fails. Checksums alone are not authentication. The
current boot chain does not reject unsigned or modified firmware programmed over
SWD; adding enforced secure boot would require a separate boot/memory-layout
project. Decode release RTT using the exact corresponding ELF.

## Boot, memory and flashing

- M7 flash: `0x08000000`, 1 MiB; private AXI SRAM: `0x24000000`, 512 KiB.
- M4 flash: `0x08100000`, 1 MiB, ending at `0x08200000`; private SRAM1:
  `0x30000000`, 128 KiB. M7's M4 boot validation uses this application range.
- There is no credential flash reservation or runtime credential store. Old
  plaintext credentials may remain from historical firmware; removing the feature
  does not securely erase them. Do not mass erase for feature cleanup.
- SRAM4 HAL metadata: `0x38000000`, 1 KiB.
- SRAM4 IPC: `0x38000400`, 1 KiB.
- Each core's `memory.x` defines shared NOLOAD sections directly. Do not add
  `shared.x` or insert shared sections after private BSS/uninitialized sections.
- Preserve linker assertions keeping runtime RAM boundaries private.
- BCM7=1, BCM4=0, boot bases M7=`0x08000000`, M4=`0x08100000`, SWAP_BANK=0.
  M7 initializes clocks, HAL metadata and IPC before releasing M4.
- Boot addresses reference vector tables, not entry functions. Use system resets,
  not core-only resets, after flashing. Match both firmware versions and schemas.
- M7 currently runs at 400 MHz; AHB/M4 at 200 MHz. Data cache is disabled;
  shared SRAM is non-cacheable. HSEM alone would not provide cache coherency.
- Flash M4 then M7, verify, and perform a full reset. Updating M7 overwrites the
  Arduino bootloader. Do not mass erase or change option bytes casually.
- Use the stock STM32H747XI probe-rs target. If attachment fails while M4 remains
  held, recover with CubeProgrammer connect-under-reset, not custom chip YAML.
- Only one debugger can own ST-LINK. Identify and release the existing session
  before flashing; do not kill unrelated processes.

## USB correctness and tracing

The current host is timer-polled, non-DMA, one root device and one active channel.
Enumeration selects the first configuration. Supported transfers include
control reads/no-data requests, up to four HID
interfaces and experimental read-only BOT/SCSI storage. Descriptor storage is
bounded to 1024 bytes. HID supports boot keyboard/mouse and a bounded non-boot
subset with report IDs and relative axes: 64 fields total per layout across at
most eight reports, not arbitrary HID interpretation.
Storage supports LUN 0, 512-byte sectors, FAT32 MBR/superfloppy, root 8.3 aliases,
at most 32 entries and files up to 64 KiB. Root scanning is capped at 32 sectors,
chain traversal at 128 distinct clusters. No writes or formatting. Historical
receiver evidence is recorded below; non-boot semantic input and real storage
remain NOT RUN. Historical evidence does not validate current images.
Hubs, general composite/class support, arbitrary HID/absolute-axis layouts,
writable storage, subdirectory traversal, LFN decoding, other filesystems,
multi-LUN, CDC-host and isochronous transfers remain future work.

- Start controls USB-A power; leave VBUS off until explicitly requested.
- Never connect the USB-A host port to another computer.
- Keep control SETUP/data/status sequencing and data toggles explicit.
- Drain RX status before treating completion as terminal. XFRC with pending RX
  is not a transaction error just because CHHLTD is also set.
- Configure directly attached low-speed channels using the root-port rules, not
  low-speed-behind-hub rules. Preserve W1C handling in HPRT and channel status.
- Every hardware wait must have a deadline and yield to other executor tasks.
- Trace successful packets with exact bytes and PID; do not invent captures or
  flood bounded queues with every NAK retry. Report drops instead of blocking USB.
- Diagnose blank topology from real enumeration events/errors before changing
  descriptor rendering. A connected browser does not prove USB enumeration.
- Add tests for parsers, malformed descriptors, packet lengths and channel outcomes.
- Preserve partial topology and stage/session/transfer diagnostics after failure;
  clear stale context on a new session. Trace filtering must not alter host work.
- Validate storage with known small and already fragmented files and checksums,
  using read-only workflows. Never destructively format a user's disk or modify
  it to manufacture fragmentation; unavailable specimens remain NOT RUN.

## IPC and contracts

Shared owns all commands/events. IPC uses bounded, serialized messages and
explicit backpressure; never expose references into shared queue storage.
Increment the protocol version when changing enum ordering or the wire schema,
update TypeScript guards/tests, and flash both images with the matching UI.

M4 forwards UI records in retried ordered chunks; publish only complete records.
Keep IPC frames within 288 bytes and UI records within their assembly limit.
Host-capture drops, M4 queue drops and browser history loss are different metrics;
do not overwrite or repeatedly sum unrelated counters.

Each core has one dedicated IPC task that exclusively owns its endpoint. Private
`embassy-sync` channels queue work independently of USB register polling and
serial writes; an in-flight frame is retained unchanged until the transport
accepts it. `Full` retries are not drops. The cross-core transport remains the
BSP's atomic SPSC queues in SRAM4. Shared `ipc_delivery` helpers use
`NoopRawMutex` only within one cooperative executor: never put these channels in
shared SRAM, access them from another core, or use them in interrupt handlers.

M7 executes HTTP host commands locally, without relaying them through M4. Its
host service handles at most four commands/messages per turn and alternates local
HTTP and incoming CDC work when both are queued, preserving FIFO within each
source. Ordering across independent sources is not transactional. HTTP Status
emits fresh host status and coalesces a separate M4 snapshot request; M4 handles
that request without echoing Status back. CDC Status snapshots on M4 and forwards
once to M7 for fresh host status. Snapshot requests and captures alternate on
outbound IPC when both are queued, preventing snapshot traffic from starving USB
records. Registers and USB scheduling remain exclusively owned by the host task.

IPC transport failures are generic platform diagnostics, not USB transaction
errors: do not increment USB error counters or invent channel snapshots. M7's
IPC task publishes cumulative transport failures at most once per second. Browser
notices surface those errors separately from USB counts and USB trace history.

M7 retains 64 capture frames plus one in flight, and four incoming messages plus
one held under backpressure. M4 independently retains 64 network and 64 serial
records, four commands, a 1024-byte JSON record and one in-flight IPC chunk.
M7 delivery state is bounded to 20 KiB and M4's UI bus to 24 KiB. At packet-safe
boundaries, M7 may yield up to 2 ms for backlog draining, without extending the
transfer deadline; command service continues. Never wait for queue capacity while
an active USB packet needs RX servicing. M4 polls the cross-core slot at bounded
25 µs intervals rather than imposing a 1 ms delay per frame; these are polling
intervals, not execution-time guarantees.

Sustained consumer stalls can still exhaust bounded queues. Preserve semantic
records by evicting the oldest queued raw record where possible, and count actual
loss when a record is abandoned. Keep serial and network losses separate; an
absent or slow serial client must not hold up HTTP. Native queue tests model
backpressure but do not establish hardware throughput or lossless capture.

M4 tracks each in-progress network record until its final chunk is accepted.
Abandoning a loss notification restores its original count, not one; an ordinary
abandoned record counts once. Serial serialization/write failures use the serial
counter only. DTR changes deliberately dispose of old serial-session queued
history; reconnect snapshots target serial only and must not trigger network
snapshot bursts.

## Wi-Fi and HTTP

Networking is AP-only: start the open `GigaUSB` AP at `192.168.4.1` with
leasehund DHCP. There is no station mode, router provisioning or credential
saving. The dashboard has no SSID/password form or Wi-Fi API.

The open AP and unauthenticated, unencrypted HTTP are development security
tradeoffs. Use a trusted environment; do not claim isolation or production security.

HTTP commands are queued: 202 is acceptance, not completion. Events use bounded,
cursor-based SSE rather than periodic browser requests. All browser requests need
deadlines, including stream reads, and cancellation must work while connecting.
Initial status failure must not prevent the event stream. Do not use
secure-context-only APIs for the HTTP UI.

## Web implementation

Application code and tests belong in `web/src` as TypeScript/TSX. Use SolidJS,
source-copied solid-ui components, Tailwind v4 via its Vite plugin, ESLint and
Prettier. No separate PostCSS pipeline or root JavaScript application modules.
Keep a consistent dark theme, accessible focus states and clear error messages.
Use Vitest for unit tests and Playwright for end-to-end tests under `web/src/e2e`.
Browser tests serve the built gzip bundle on loopback with intercepted API
fixtures; they are not board tests. Run `npm run check:all` for formatting, lint,
types, unit tests, a build and browser tests. `npm run test:e2e` builds before
running browsers independently. Install the matching Playwright Chromium when
setting up a test machine. Keep browser traces/reports out of version control.

Prefer concise user-facing labels. Do not expose implementation jargon such as
“same-origin HTTP” or protocol version labels in the normal dashboard. Keep
technical details in expandable trace records and documentation. Use real data,
never placeholders presented as captures. Render device-controlled text safely.

## Working style and validation

Use straightforward code, imports at the top, small understandable functions and
existing patterns. No speculative frameworks, redundant helpers, Python scripts,
extra editor tasks or comments that restate the code. Preserve user changes.
Implement only features that advance the USB host/diagnostic goal.

Use atomic, unscoped Conventional Commit subjects: `feat: ...`, `fix: ...`,
`chore: ...`, `test: ...`, `docs: ...` or `refactor: ...`. Do not use parenthesized
scopes such as `feat(ai): ...` or `fix(usb): ...`. Each commit should contain one
cohesive change and its directly related tests or documentation; leave unrelated
user edits out of it.

Do not change the development Mac's Wi-Fi connection: it disconnects the agent.
Use a separate client for AP/browser validation. Local browser fixtures can test
UI behavior but are not hardware tests. Keep temporary test outputs under target
or system temporary storage; do not add scripts just for one-off investigations.

State validation precisely: formatting, compilation, unit tests, flash verification,
HTTP transfer, browser rendering and actual USB enumeration are separate checks.
Never claim that everything works perfectly from compilation alone. Report the
remaining evidence needed and known limits honestly.

## Additional platform constraints

The USB-A internal PHY is full-speed, not a 480 Mb/s link. The RGB LED is
active-low: PI12 red, PJ13 green, PE3 blue. Keep SDMMC DMA buffers in M7 AXI SRAM.
M4 adopts initialized clock metadata rather than reconfiguring shared clocks;
single-core critical sections are not inter-core locks. There is no heap or
stack-overflow protection. The host engine and storage parsers belong to M7;
descriptor/HID processing belongs to M4, not shared.

HID output is limited to six non-modifier keys and signed 8-bit mouse values.
Keyboard values are HID usage codes, not translated characters.
Trace payloads are at most 64 bytes. Storage filesystem work has a 30-second
budget checked between complete BOT commands. LFN entries are skipped;
subdirectories can be listed but not opened. FAT16, exFAT and GPT are unsupported.

Logging is disabled by default. With `defmt`, decode RTT with the exact flashed
ELF. RTT is nonblocking; `panic-halt` does not emit a panic report.

## RGB status LED


| Indication | Meaning |
| --- | --- |
| Slow white pulse | Starting AP/network services while the USB host is stopped |
| Blue double pulse | GigaUSB AP available, USB host stopped |
| Short yellow pulse | Host enabled, waiting for a USB device |
| Fast yellow blink | USB attachment/enumeration |
| Steady green | USB ready |
| Brief cyan flash | A USB packet completed successfully while ready |
| Red triple pulse | USB fault or AP/network error |

Priority is fault, enumeration, USB activity, USB ready/waiting, then AP startup
or AP available with the host stopped. The running AP must not hide USB states:
ready returns to steady green after each 80 ms cyan activity indication; waiting
uses a 150 ms yellow pulse every second. Enumeration blinks yellow every 200 ms.
With the host stopped, AP availability uses two 100 ms blue pulses in a two-second
cycle; startup uses a 500 ms white pulse in that cycle. Faults use three 100 ms
red pulses in a two-second cycle. There are no station join/connected or credential
write patterns. AP availability is not a browser-connection or HTTP-success signal.
Patterns are on/off RGB combinations, not PWM fades. The LED is informational;
use dashboard/USB-C diagnostics for the exact state and failure reason.

## Hardware setup and debugging


Use an external ST-LINK, J-Link or equivalent. The GIGA has no onboard ST-LINK;
the SPI/ICSP header is not an SWD connector. Consult the board pinout for orientation.

| Probe | GIGA signal |
| --- | --- |
| SWDIO | PA13 |
| SWCLK | PA14 |
| GND | GND |
| VTref | Board 3.3 V reference |
| NRST | RESET / MCU NRST |

Use 3.3 V logic, not 5 V. Power the board normally; VTref is a sense connection.
Connect NRST for recovery and leave BOOT0 low for normal flash boot.

Inspect and save option bytes before making necessary changes. ELF programming
does not configure them:

| Option | Required value |
| --- | --- |
| `BCM7` | `1` |
| `BCM4` | `0`; M7 releases M4 after initialization |
| M7 ADD0 | `0x08000000` vector base; encoded field `0x0800` |
| M4 ADD0 | `0x08100000` vector base; encoded field `0x0810` |
| `SWAP_BANK` | `0` |

Encoded boot fields contain address bits `[31:16]`; tools may instead ask for the
full byte address. Keep ADD1/system-ROM options unchanged, readout protection at
level 0 and relevant sectors unprotected. Do not change unrelated/security options.
Apply/reload required changes and power-cycle. Incorrect options can block boot/debug.
M4 is not a standalone image: it requires M7 initialization.

### Flash, log and debug

**M7 overwrites the Arduino bootloader.** Arduino USB uploading and double-reset
recovery require restoring that bootloader over SWD.

Release any debugger holding the probe. With both images built, flash **M4 then
M7**, verify and perform a full reset. CubeProgrammer requires an `.elf` extension:

```sh
cp target/thumbv7em-none-eabihf/release/m4 target/thumbv7em-none-eabihf/release/m4.elf
cp target/thumbv7em-none-eabihf/release/m7 target/thumbv7em-none-eabihf/release/m7.elf
STM32_Programmer_CLI -c port=SWD mode=UR reset=HWrst -w target/thumbv7em-none-eabihf/release/m4.elf -v
STM32_Programmer_CLI -c port=SWD mode=UR reset=HWrst -w target/thumbv7em-none-eabihf/release/m7.elf -v -rst
```

ELF addresses determine placement. Use sector-scoped flashing; do not mass erase
or supply a raw-binary address. For J-Link, select H747 dual-core support, not H743.

For ordinary probe-rs updates:

```sh
probe-rs download --chip STM32H747XI --speed 1000 target/thumbv7em-none-eabihf/release/m4
probe-rs download --chip STM32H747XI --speed 1000 target/thumbv7em-none-eabihf/release/m7
probe-rs reset --chip STM32H747XI
```

The stock dual-core target can time out if M4 remains held because M7 is missing
or faulting. Recover with CubeProgrammer connect-under-reset and matching images,
not a custom chip YAML. Never run two probe owners at once.

With matching M4 already flashed, display M7 RTT logs using:

```sh
cargo run -p m7 --release --features defmt
```

**This `cargo run` workflow flashes/reboots M7 and interrupts the live connection.**
Prefer USB-C serial status when diagnosing the running board.

`Embed.toml` selects the chip and SWD at 8 MHz; reduce speed for unreliable wiring.
The default profile runs after reset. The `debug` profile halts and opens GDB at
`127.0.0.1:1337`; continue execution to run the application.

### Editor debugging

Zed uses its managed rust-analyzer, formatting on save and package-specific Clippy
and build-script commands. To check M4, change `--package m7` to `--package m4` in
both `.zed/settings.json` overrides, then restart the managed language server.
The latest reported diagnostics refresh showed zero Rust errors. The exact Cargo
JSON macro-artifact path and loading of the dynamic library providing
`main_cortex_m`/`task` were verified. These checks do not establish complete HAL
semantic analysis: Cargo workspace metadata still merges incompatible core HAL
features. Separate package CLI checks remain authoritative. If diagnostics are
stale, restart Zed's managed language server; do not install another rust-analyzer,
hide diagnostics or split the workspace.

Run **Debug: start cargo-embed server (M7)**, then **M7: attach to cargo-embed
server**. CodeLLDB attaches to the same GDB endpoint; cargo-embed is not a DAP
server. Disconnect CodeLLDB before stopping the server. The debug-attachment
workflow remains hardware-unvalidated.

## Network diagnostics and API

M7 runs picoserve with three bounded HTTP workers on port 80, eight network socket
slots, and admission for at most two SSE streams. This leaves one HTTP worker for
commands and ordinary requests; excess stream requests receive 503. Each worker
owns an 8-KiB page buffer pair plus bounded TCP/HTTP buffers. M4's USB-C CDC link
is independent; a missing serial client must not stop HTTP.
The open `GigaUSB` AP is `192.168.4.1/24`; leasehund assigns
`192.168.4.100`–`192.168.4.131` with one-hour leases. It is neither an Internet
router nor a captive portal. Open `http://192.168.4.1/` from a separate client.
Do not change the development Mac’s Wi-Fi.

Prefer USB-C status to inspect the running board without rebooting: open
**GigaUSB Diagnostics** at **115200 baud**, then send
`{"command":"status"}` followed by a newline. Collect actual status and errors;
a DHCP lease alone does not prove HTTP transfer or browser rendering works.

### SDIO bounds-panic backport

With explicit user approval, `vendor/cyw43` backports the upstream CMD53
length correction to registry version 0.7.0. Both read and write partitioning
use the actual slice length, not the padded `Aligned` allocation size.
The root Cargo patch applies to both the application and `giga-r1`.
See `vendor/cyw43/PATCH.md` for provenance and regression tests.
Upstream already fixed this in Embassy commit
`a52524892bb7dd6128952a38c046dadcb952c7e3`; current handling lives in `sdio`.
Additional regression coverage was submitted in
https://github.com/embassy-rs/sdio/pull/35 (base branch `master`).
Remove the local patch when a compatible fixed dependency release is adopted.
Both patched images were subsequently flashed and verified. USB-C status reported
AP mode at `192.168.4.1` without a network error. This is startup evidence only:
HTTP transfer, browser rendering and sustained AP traffic remain NOT RUN.
A later USB endpoint `0x82` controller fault was observed; a short Start/Stop
retry enumerated the receiver without a new error, but the original fault remains
unresolved. Evidence is under `target/red-led-diagnostics.jsonl` and
`target/usb-red-led-retry.jsonl`.

Images built from `3a1aa13`, including the solid-ui USB-only dashboard and the
halt-deadline state recheck, were subsequently flashed M4 then M7, verified and
system-reset. USB-C status reported AP mode at `192.168.4.1`, no network error,
zero USB errors and the host stopped. Evidence is in
`target/latest-flash-status.jsonl`. This startup check does not validate USB
activity, on-board HTTP transfer/browser rendering or resolve the earlier fault.

Images built from `26d776b`, including the virtualized trace, bounded event
batches, device identification and scrollable details, were flashed M4 then M7,
verified and system-reset. Frontend checks passed with 35 unit tests and 31
Playwright tests; both logging-enabled release builds passed. USB-C startup
status reported AP mode at `192.168.4.1`, no network error, zero USB errors and
the host stopped. Evidence is in `target/virtualized-ui-flash-status.jsonl`.
This does not establish on-board browser rendering, USB activity or resolution
of the earlier controller fault.

The AP-only LED update in `89b462d` was flashed M4 then M7, verified and
system-reset. All 63 native Rust tests passed, including LED timing/priority and
AP readiness/error tests. M7 Clippy and separate per-core release builds passed
with and without logging. USB-C startup status reported AP mode at `192.168.4.1`,
no network error, zero USB errors and the host stopped. Evidence is in
`target/ap-led-flash-status.jsonl`. Physical observation of every LED pattern,
USB activity and on-board browser validation were not performed in this run.

### Red-LED disconnect cleanup investigation — 2026-11-21

USB-C status from the running image reported session 6 in error, host disabled
but still attached, 205 completed transfers, one USB error and zero capture drops.
AP status remained `192.168.4.1` with no network error. The saved endpoint `0x83`
diagnostic had `elapsed_us=10220`, HCINT `0`, HCCHAR `0x805C9840`, HCTSIZ
`0x00080040` and HPRT `0x00021008`. HPRT connection and enable bits were clear;
overcurrent was not asserted. Evidence: `target/red-blink-status.jsonl`.
A bounded non-resetting RTT attachment yielded no retained diagnostic output.

The driver could replace a disconnect with a controller error when cleanup
could not halt the channel. Halt classification now recognizes a disconnected
active channel before its timeout, and teardown powers off and invalidates
initialization so the next activation RCC-resets HS before channel reuse.
Idle channels need no extra halt; connected halt deadlines and overcurrent errors
are preserved. Five native regressions include the actual saved registers and
a latched disconnect during fast reconnect. All 57 M7 tests, Rust formatting,
M7 release Clippy and builds with/without logging passed. This patch has not
been flashed or hardware-tested. The initiating disconnect/link loss and any
separate connected-port halt fault remain unproven; do not call them resolved.

### SSE software validation — 2026-11-21

The event/trace SSE implementation passed all 120 native tests (21 shared,
74 M7, 25 M4), frontend formatting/lint/types, 68 web unit tests and 52 Chromium
browser tests against the real gzip bundle and a loopback streaming fixture.
Separate release Clippy and builds passed for each core with and without `defmt`.
Coverage includes independent subscribers, admission/drop cleanup, cursor wrap,
server filtering, cancelled reads/backoff, stalled connections, reboot-style
continuity resets and a reset frame lost before acknowledgment.

The logging-enabled ELF load ranges remain within their current flash banks;
M7 ends at `0x080E33F0` and M4 at `0x0811AEF4`. M7 static RAM ends at
`0x24072C90`, leaving about 53 KiB below the initial stack pointer. This is a
layout check, not proof against stack overflow. No heap or memory boundary changed.
The SSE images have not been flashed. On-board stream delivery, sustained USB/AP
traffic, slow-client recovery and concurrent-client responsiveness remain NOT RUN.
These checks do not resolve the initiating USB disconnect investigated above.

### Routes

| Route | Request | Response |
| --- | --- | --- |
| `GET /` | — | Embedded HTML, `Content-Encoding: gzip`, `Cache-Control: no-cache`, ETag |
| `POST /api/command` | `{command}` | 202 queued, 503 queue full |
| `GET /api/events` | Resume tokens, independent event/trace cursors and optional trace filters | SSE: `reset`, `events`, optional `trace`; comment heartbeats |
| `GET /api/trace?after=0` | Cursor and optional trace filters | `{records:[{sequence,record}], cursor, dropped}` |

JSON and SSE responses are uncompressed. SSE uses `Content-Type: text/event-stream`
and picoserve's `Cache-Control: no-cache`; finite trace responses use `no-store`.
Malformed/oversized request JSON is rejected; clients must not depend on
picoserve’s diagnostic error-body text.

`/api/events` uses picoserve's built-in SSE transport. Query fields are `after`,
`trace_after` (wrapping `u32`, default zero), `trace` (default false), optional
`client`, `attempt`, `resume` (at most 32 bytes each), and the trace filters below.
Unknown or invalid fields return 400. `events` frames carry the existing
`{records,cursor,dropped}` envelope; `trace` frames carry the sequence-bearing
trace envelope. Both contain at most three records and 4096 JSON bytes. The initial
events page is sent even if empty. Trace is opt-in and never narrows canonical
events. The JSON `/api/trace` route remains available for finite requests.

Each browser manual connection creates a 32-hex client token; every fetch creates
a fresh 32-hex attempt. The browser echoes `resume` only after processing a
`reset` frame containing `{"resume":"<attempt>"}`. M7 remembers two client/proof
pairs in private RAM, independently of active subscriber ownership. A matching
acknowledged proof resumes both supplied cursors. A missing/forgotten client or
mismatched proof forces a reset and zero cursors. Lost reset frames must force
another reset, never silently accept old cursors. A reboot or inactive-token
eviction therefore clears stale browser state without requiring persistent
firmware storage. These identifiers are not authentication. Clients omitting
attempt/proof start fresh and receive `{"resume":null}`.

Every subscriber has an independent, coalesced Embassy channel notification.
Complete-record publication and assembly-loss changes wake all active subscribers;
producers never await a browser. Dropping the stream releases its admission slot.
M7 services both cursors fairly for at most eight turns per batch, then yields;
when caught up it waits for notification or a two-second comment heartbeat.
Each complete SSE frame/heartbeat has a five-second write deadline. Slow readers
can lose bounded history; stream delivery is not lossless capture.

`/api/trace` is an additive HTTP route; IPC/UI records and protocol version are
unchanged. It scans at most the retained 64 board records and returns at most
three USB matches in a 4096-byte response. Optional parameters are exact
`endpoint` address (`u8`), `direction=in|out`, `kind=control|interrupt|bulk`,
`errors=true|false`, and `q` (at most 96 decoded UTF-8 bytes, trimmed,
ASCII-case-insensitive). Invalid parameters return 400. Search includes record
summaries, typed diagnostic fields, SETUP request names and hex bytes. An empty
SETUP on an idle/non-control diagnostic is not decoded as a request.

Server filtering preserves immediately adjacent compatible context/payload pairs,
including across page boundaries. Each match carries its original wrapping `u32`
sequence; exclusions advance the cursor without counting as loss or inventing
new adjacency. Generic errors without endpoint/kind metadata cannot satisfy those
scoped filters. Filtered-query `dropped` is a separate latest loss snapshot of
unavailable board records, not necessarily matching USB records; never combine it
with host or canonical event-stream counters.

Commands use serde's externally tagged representation:

```json
{"command":"start"}
{"command":"stop"}
{"command":"reset"}
{"command":"status"}
{"command":"storage_list"}
{"command":{"storage_read":{"file":0,"offset":128}}}
```

**202 is queue acceptance, not completion.** HTTP commands go directly to the
M7 host's local bounded queue; CDC commands travel M4 → M7. Execution produces
asynchronous events. `status` requests fresh M7 host status and a separate retained
M4 processor snapshot. There is no M4 round trip for HTTP Start/Stop/Reset or
storage execution. Start powers USB-A; browser Disconnect does not stop it.
Invalid command names are rejected during deserialization.

Storage uses an index from the most recent listing, not a path. Read responses
contain up to 128 bytes and identify index, offset, size and EOF. Downloads must
validate these before continuing. Listings are shared between clients, not
transactionally isolated. No writes are supported. Unsupported media, excessive
files/size and invalid offsets return storage-error events, not file data.

### Event delivery and loss

Each record is `{timestamp_ms, event}`; `event.type` identifies the UI record.
`UiEvent::Host` contains an externally tagged `UsbEvent`. Exact fields live in
`firmware/shared/src/usb/types.rs`; browser guards live in `web/src/lib/protocol.ts`.

- IPC frames are at most 288 bytes. M4 serializes each UI record into ordered,
  retried chunks; M7 publishes only complete records up to 1024 bytes.
- M7 retains 64 records. Each SSE page contains at most three; the server drains
  available pages fairly, then waits for publication rather than polling.
- Cursors wrap as `u32`. A future cursor restarts at retained history. Automatic
  reconnects retain acknowledged cursors; manual Connect starts at zero. Delivery
  is non-destructive and a continuity reset clears both cursor spaces.
- `dropped` combines records missed since the supplied cursor with cumulative
  assembly overflow. It is a **latest loss snapshot**, not a delta or globally
  cumulative total; do not sum repeated values.
- Host-capture drops, M4 queue drops and browser history eviction are separate.
  Under pressure, semantic records take priority over raw transfer/context
  records. Loss is counted; bounded queues do not promise lossless capture.

Snapshots retain topology, lifecycle, latest errors/diagnostics and storage
capability. File listings and file-data replies are live-only, preventing stale
chunks from satisfying a new read. Relist after reconnecting.

Transfer context precedes its raw packet; association requires adjacent compatible
records. PID is STM32 DPID encoding: `0=DATA0`, `2=DATA1`, `3=SETUP`. Diagnostics
retain pre-cleanup registers and include elapsed time/retries. Transfer ID zero
with empty SETUP denotes controller/idle context, not a real EP0 request.
These records omit handshakes, CRCs and wire timing.

All browser requests need deadlines covering body reads and external cancellation.
Initial status failure must not prevent event streaming; non-idempotent commands
must not be automatically retried. Trace filtering must not change USB scheduling.

## Browser state and trace behavior


`src/main.tsx` provides the QueryClient. TanStack retries and window-focus
refetching are disabled; only the idempotent SSE transport reconnects itself.

One long-lived event query owns a fetch-based SSE stream, not an EventSource or
an infinite-query page cache. TanStack's query AbortSignal is forwarded through
linked controllers to fetch, body reads and retry waits; a separate session
controller also cancels commands and downloads. Requests have eight-second header
and per-read idle deadlines, not an eight-second total stream lifetime. The parser
bounds each frame to 8192 encoded bytes, supports fragmented UTF-8/CRLF, and
validates records before delivering them or committing cursors. Transient failures
and EOF reconnect with 250-ms exponential backoff capped at five seconds; malformed
frames/pages or incompatible protocol fail explicitly. Reconnect resumes only
processed event/trace cursors and an acknowledged reset proof. A valid events page,
not just response headers, establishes Connected. Initial status failure does not
block the stream. Cancellation aborts reads, pending connections and backoff;
obsolete session/filter callbacks cannot mutate state. Queries and mutations use
`networkMode: 'always'` so browser Internet hints do not pause the local AP.

Dashboard data regions use Solid Suspense with a finite, manual-session readiness
resource, resolved after the first validated canonical events page (including an
empty page), or settled on cancellation/fatal disconnect. Do not read the
never-ending SSE query's `data` to drive Suspense: it would never resolve.
Host status, counters, topology/HID, diagnostic/storage and trace regions have
loading fallbacks. Connection/cancel controls, command controls and error notices
stay outside these boundaries. A loaded session retains its display during
transient reconnects; obsolete sessions cannot settle a newer readiness resource.

Storage listeners run immediately on validated canonical records. Actual stream
interruptions, continuity resets or increased canonical loss cancel downloads;
deliberate filter restarts preserve the canonical cursor/listeners. A reset clears
stale device/trace/loss state while preserving pause and filter choices. Histories
stay bounded outside the query cache. No browser event/trace polling timer remains.

Commands are mutations: HTTP **202 means queued, not executed**. Completion must
be observed in events. Queue-full/rejected commands report an error and are never
automatically retried. Contracts and queue limits are specified above; use the protocol version in shared.

### Trace and device state

- USB trace history retains the latest **500 validated USB records**: transfers,
  transfer contexts, diagnostics and typed host errors. Network, status, lifecycle
  and semantic updates still update dashboard state but do not enter trace history
  or exports. Browser-local arrival sequence preserves original context/payload
  adjacency across omitted updates. Display is newest-first; JSON exports are
  oldest-first. Exports can contain sensitive input.
- The USB trace follows solid-ui's data-table pattern using
  `@tanstack/solid-table` over the normal table components. Enable only relevant
  column visibility controls: Time, Endpoint and Transfer may be hidden; Details
  remains required. Keep server-side filtering and chronological order; do not
  add client sorting/filtering, pagination or selection without a concrete need.
  Other descriptor/storage tables use the ordinary table components.
- Trace rows use `@tanstack/solid-virtual` with dynamic height measurement and
  stable sequence/transfer keys. Preserve the visible record offset when streaming
  prepends new records; fall back to a retained neighbor if that record is evicted.
  Expansion state is bounded by retained history. Split large transfer groups
  into bounded render rows without changing context association or exports.
- Device details are height-bounded and keyboard-scrollable. Device identification
  is an offline VID/PID/class inference or reported storage inquiry text, not
  authenticated make/model detection. Label guesses and retain raw descriptors.
- **Pause display** stops rendering, not reads or capture. **Clear trace** clears
  browser history only. Disconnect leaves history exportable; reconnect starts a
  fresh session and replays available board history.
- Endpoints belong to an interface/alternate setting. Partial topology remains
  inspectable after failure; a new device session clears stale state.
- Filters and search run on M7, not by matching browser history. One SSE stream
  multiplexes unfiltered `events` for state/storage/full exports and optional
  server-selected `trace` frames with an independent cursor. Filter changes
  debounce for 200 ms, reset only filtered history/cursor, and replace the stream
  while preserving canonical progress and the reset proof. Filtered history is
  bounded to 500 records; server batches use the same eight-turn fairness budget.
  Searches cover retained board history, not older browser-only records. Filters
  are disabled while paused or disconnected.
- Endpoint filters match the full decimal address exactly (`0` differs from
  `128`); IN is device-to-host, OUT is host-to-device. Control, interrupt and bulk
  filter the transfer kind when context provides it; missing context is not
  guessed. Only originally adjacent compatible context/payload records associate,
  including wrapping server sequence IDs, then group by session/transfer ID.
- SETUP decoding covers standard and HID/MSC requests. Expanded records show raw
  bytes, PID, timing/retries and failure snapshots. These are completed host-side
  transfers, not passive wire capture, handshakes, CRCs or bus timing.
- Host capture drops, M4 processing drops and the latest transport-loss snapshot
  are separate metrics. Transport loss is not repeatedly summed. Increased loss
  requests a bounded/rate-limited status refresh; it does not recover lost trace.

### Read-only storage

The storage card appears after a real capability event. List the FAT32 root,
then download a regular file up to **64 KiB**. Larger files are rejected by firmware,
not silently truncated. Only 8.3 aliases, up to 32 entries and no recursion/writes
are supported; see USB correctness and tracing above for remaining bounds.

One download runs at a time. Each request asks for at most 128 bytes, matched by
file index, offset and total size. The listener is installed before submission
because data may arrive before the HTTP acceptance response. Both acceptance and
valid data are required. Chunks have a ten-second response deadline; disconnect,
USB session changes, Stop or errors fail the operation. Concurrent clients share
one listing/index space; do not relist while another client downloads.

## Recorded validation evidence

These are historical results from 2026-11-20, not validation of later edits.
Compilation, flash verification, HTTP transfer, browser rendering and USB operation
are separate checks. These records include removed station/persistence features
and the former flash reservation; they are historical evidence, not active policy.

### Full flash erase and frontend test migration — 2026-11-20

- **PASS, erase:** explicitly requested full internal-flash erase completed through
  CubeProgrammer. Read back `0x08000000..0x08200000`; all 2 MiB were `0xFF`.
  The Arduino bootloader, applications and old credentials in main flash are gone.
  No firmware was reflashed afterward; no option-byte changes were requested.
- **PASS, software:** 25 Vitest unit tests and seven Playwright Chromium tests,
  including axe accessibility checks, against the built gzip dashboard with local
  API fixtures. Formatting, ESLint, TypeScript and production build passed.
- **CONFIRMED defect:** the frozen board's saved stack resolved to the cyw43
  CMD53 read bounds check. `size_of_val` includes `Aligned` padding, so block
  partitioning can exceed the underlying slice length. The earlier committed and
  current lockfiles both use registry cyw43 0.7.0 with the same checksum. Earlier
  HTTP success does not establish that all packet-length cases were safe.
- **NOT FIXED:** AP-only mode removes provisioning but does not remove this SDIO
  defect. No vendored dependency or registry edits remain. An upstream-compatible
  fix and real AP/HTTP revalidation are still required.

### Ownership and accessibility refactor — 2026-11-20

- **PASS, software:** 69 native tests (20 shared, 33 M7, 16 M4), 35 frontend
  tests, formatting, lint/type checks, separate release Clippy with/without
  logging, and both release builds. Neither ELF overlaps credential flash.
- **PASS, flash:** matching refactored images programmed, verified and reset.
- **PASS, limited hardware:** the receiver enumerated four interfaces/endpoints,
  configured address 1, and produced a boot-keyboard report: 41 completed packets,
  zero USB errors. Raw trace/context loss: 29 records. Stop returned disabled /
  detached without an erroneous cancellation diagnostic. Evidence is in
  `target/usb-refactor-validation.jsonl`. No lossless capture claim.
- **PASS, local accessibility audit:** axe-core 4.14.0 found no violations in
  tested empty, form-error, populated trace/storage and malformed-response states.
  Keyboard focus and accessible button names were checked in isolated Chrome.
  Some scrolled trace contrast checks were inconclusive and need manual review;
  this is not full WCAG certification or hardware-browser validation.
- **NOT RUN:** broader device compatibility, real storage integrity, current-image
  Wi-Fi handoff/persistence, and visual verification of every RGB pattern.

Earlier sections below retain their original counts and module layout. They do
not validate subsequent edits.

### Persistence integration — 2026-11-20

- **PASS, software:** 67 shared Rust tests, 30 frontend tests, formatting, ESLint,
  TypeScript, separate release Clippy with/without logging, and release builds.
  Simulated flash tests cover torn writes/erases, page rollover and subsequent
  recovery/saves; they are not hardware power-loss or ECC evidence.
- **PASS, layout:** inspected both ELF load ranges; none intersects
  `0x081C0000..0x08200000`. M4 application flash ends at `0x081C0000` (768 KiB).
- **PASS, flash/startup:** both matching images programmed and verified without
  mass erase. After reset, USB-C diagnostics reported setup mode at
  `192.168.4.1`, runtime MAC `DC:FE:23:68:94:E1`. This differs from the
  configured NVRAM fallback in that historical build.
- **NOT RUN:** successful real credential write, saved-network boot without AP,
  wrong/unavailable-network fallback, unchanged-write suppression on hardware,
  update preservation of an existing record, and concurrent USB/network continuity
  during bank-2 writes/erase. These require a separate provisioning client and
  known test router; the development Mac's Wi-Fi was not changed.

### Recorded v8 results — 2026-11-20

- **PASS, software:** 57 Rust tests, 28 frontend tests, Rust/Prettier formatting,
  ESLint, TypeScript, per-core release Clippy with and without logging, and both
  release builds with logging.
- **PASS, flash:** both matching v8 images programmed, verified and system-reset.
- **PASS, limited hardware:** repeated the receiver enumeration through USB-C:
  four interfaces, four associated endpoints, five complete report-descriptor
  chunks, configuration at address 1, boot-keyboard decoding and 41 packets.
  USB errors: 0. Lost raw trace/context records: 20; no lossless claim.
- **PASS, stopped state:** Stop followed by Status reported `enabled:false`,
  `attached:false`. No electrical VBUS measurement.
- Evidence: `target/usb-v8-validation.jsonl`. v8 browser rendering, actual failed
  transfers, real storage, non-boot semantic input and network handoff/recovery
  scenarios remain NOT RUN. Historical v7 browser fixtures below are not v8
  browser evidence.

### Recorded v7 results — 2026-11-20

- **PASS, software:** frontend formatting/lint/type checking, 28 frontend tests,
  Rust formatting, per-core release Clippy, 57 shared Rust tests, and both release
  builds. Release checks with logging disabled also passed during integration.
- **PASS, flash:** matching protocol-v7 M4 and M7 images programmed and verified
  with CubeProgrammer, followed by a full system reset.
- **PASS, limited hardware:** USB-C commands exercised the real four-interface
  Logitech receiver `046D:C548`. It enumerated full-speed, configured address 1 /
  configuration 1, published all four interface/endpoint associations, complete
  54-byte and 429-byte `0x22` report descriptors for interfaces 2/3, and an eight-byte
  boot-keyboard report. Final status: 41 completed packets, zero USB errors.
- **LIMIT observed:** the enumeration burst lost 22 expendable raw trace/context
  records. Semantic records survived priority queueing. This is not lossless
  capture. Evidence from this local run is under `target/usb-v7-validation.jsonl`.
- **PASS, stopped state:** a subsequent status query reported `enabled:false`,
  `attached:false`; USB-A was left stopped after testing. No electrical VBUS
  measurement was made.
- **PASS, local browser fixture only (v7):** the actual gzip bundle rendered dark in
  isolated Chrome; queries, interface grouping, decoded trace filters, partial
  topology after failure, and a 301-byte download assembled as 128/128/45 bytes
  worked. This is not on-board HTTP or real-media validation.
- **NOT RUN:** real keystroke/mouse transitions, directly attached low-speed
  mouse, non-boot semantic input, physical disconnect/recovery and STALL
  injection, real mass-storage/checksum tests, and current-image Wi-Fi handoff,
  DHCP failure, link-loss recovery and multiple-client scenarios.

## Hardware regression procedures — NOT RUN

Record commit, protocol, bundle/image identifiers, board/power/probe, peripheral
VID/PID/model/speed, client/browser, steps, expected and observed results,
elapsed time and sanitized evidence. Mark each check PASS/FAIL/NOT RUN and keep
host, M4 and browser loss counters separate. Never retain sensitive input. Historical partial checks do not complete these procedures.

Before flashing, run the build checks above and logging-disabled release builds
for each core separately. Inspect both ELF load ranges and linker maps against
the current memory layout. Retain verification output and use the flashing
procedure above.

### Safe hardware setup


- Use a separate phone/laptop for AP and browser tests. **Do not change the
  development Mac's Wi-Fi.** Never connect USB-A to another computer.
- Respect board/peripheral power limits; use non-sensitive test keystrokes.
- Boot without Start: expect USB-A VBUS off. Record observation/measurement.
- Save real event exports and diagnostics, not fixture data presented as captures.

### Real-device matrix — each row NOT RUN

| Device | Exercise | Expected evidence |
| --- | --- | --- |
| Full-speed keyboard | Start, press/release keys and modifiers | Speed, descriptors, address/configuration, correct transitions without stuck keys |
| Direct low-speed mouse | Move both directions, click, wheel if supported | Low-speed enumeration, signed relative axes/buttons, no hub-specific configuration |
| Composite keyboard/mouse receiver | Exercise each supported interface and report ID | Up to four HID interfaces polled; independent input state and correct report routing |
| Unsupported device/layout | Attach a hub, unsupported class or out-of-subset HID | Explicit unsupported/error evidence; no invented input or controller hang; partial topology retained |
| FAT32 flash drive | List root and read known files | BOT capacity, FAT32 mount, bounded listing, exact bytes/length/checksum; see below |

Limits to test, not broad compatibility promises: one directly attached low/full-
speed root device, one active host channel, configuration/report descriptors up
to 1024 bytes. HID supports boot keyboard/mouse and a bounded non-boot keyboard/
mouse subset with report IDs and relative axes: at most four HID interfaces,
64 fields total per parsed layout across at most eight reports. Arbitrary HID,
absolute axes, general composites and hubs are not supported.

### Lifecycle, traces and diagnostics — NOT RUN

1. Start with no device; attach, exercise input, unplug during activity, reattach.
   Repeat Stop/Start and Reset while active and idle. Expected: bounded recovery,
   correct lifecycle/session state, no stale input or storage selection, VBUS off
   after Stop, and a fresh enumeration after restart/reset.
2. Disconnect/reconnect only the browser. Expected: board operation continues;
   cursor polling resumes with explicit bounded-history loss where applicable.
3. Exercise each available trace filter with known control, interrupt and bulk
   activity; clear filters and export. Expected: matching visible records, no
   fabricated packets, and no change to USB scheduling from display filtering.
   Verify session/transfer/endpoint correlation, stage, elapsed time and retries;
   successful packet bytes/PID must agree with the transfer, at most 64 bytes per
   record. NAK retries must not flood queues. This is not handshake/CRC/timing capture.
4. For a real unsupported/failing enumeration, save the last successful descriptor
   and diagnostic: error, SETUP bytes, session/transfer ID, endpoint, Setup/Data/
   Status stage, `elapsed_us`, `retries` and controller snapshots. Expected: partial topology remains
   inspectable after failure rather than disappearing; a new session clears stale
   context. Do not induce electrical faults to manufacture a failure.
5. Poll slowly and use multiple clients under input/storage traffic. Expected:
   responsive commands, bounded queues and distinct loss metrics; 202 indicates
   acceptance only. Retain completion/error records to establish execution.

### Read-only storage — NOT RUN

Use an existing, backed-up, non-sensitive FAT32 test drive and ordinary test data.
**Never format a user's disk or modify it to force fragmentation.** No automated
installed scripts are required. Choose a known small file and an already
fragmented file (record independently verified cluster layout if available).
If no fragmented specimen exists, leave that case NOT RUN.

Supported: BOT/SCSI LUN 0, 512-byte sectors, MBR FAT32 or FAT32 superfloppy,
root 8.3 aliases, at most 32 entries, files at most 64 KiB. LFN entries are skipped;
subdirectories can be listed but not opened. Root scan is capped at 32 sectors,
chains at 128 distinct clusters; filesystem work has a 30-second budget checked
between complete BOT commands. No writes, formatting, exFAT, FAT16, GPT,
subdirectory traversal or multi-LUN support is promised.

1. Before attachment, record names, byte lengths and SHA-256 of known files using
   ordinary read-only host tools (for example `shasum -a 256 /path/to/SMALL.TXT`).
   Keep the manifest outside the test drive; avoid host tools that write metadata.
2. Start, retain capacity/mount events, request root listing, then read the small
   and fragmented files. Reassemble returned data in offset order, retain exact
   bytes outside the drive, compare lengths and SHA-256 with the manifest.
   Expected: byte-for-byte agreement, explicit completion, no missing/duplicate
   chunks silently accepted. Test empty and 64-KiB files if already available.
3. With existing suitable media/data, request a directory, file above 64 KiB,
   invalid selection/offset, and unsupported volume. Expected: explicit bounded
   errors, no hang or falsely successful partial file. Boundary cases without
   suitable safe media remain NOT RUN; malformed-media fixtures are host tests.
4. Unplug or Stop during a read, then reconnect and list again. Expected: bounded
   cancellation/error and fresh selection; no stale file chunks treated as new.
5. Inspect SCSI command evidence: only inquiry, readiness, request-sense, capacity
   and READ(10), plus BOT recovery as needed; no WRITE/FORMAT commands. Recheck
   original file checksums after testing with a read-only host workflow. Unchanged
   files alone do not prove no writes; retain command evidence too.

### AP/HTTP with a separate client — NOT RUN

Use a separate phone/laptop; do not change the development Mac’s Wi-Fi.
The open AP and unauthenticated HTTP expose board control and trace data.

- Cold boot: confirm `GigaUSB`, a DHCP lease and access to `192.168.4.1`.
- Verify `/` is HTML with `Content-Encoding: gzip`; JSON is uncompressed.
  Record HTTP transfer and browser rendering separately.
- Confirm event polling starts after initial status failure, requests have
  deadlines, and connecting can be cancelled.
- With two clients, check independent cursors, bounded loss reporting and
  responsive commands; do not assume client isolation.

Current-image AP/HTTP hardware validation, broader HID interoperability and
real-media storage integrity remain NOT RUN. Historical fixtures and receiver
results do not validate the current images.
