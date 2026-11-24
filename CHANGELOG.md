# Changelog

All notable changes to GigaUSB are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and releases follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [v0.1.0-rc3] - 2026-11-24

Third release candidate, adding opt-in boot-HID forwarding and a single verified
installer. This remains experimental firmware. Forwarding is disabled at boot;
installer verification alone does not access the board.

### Added

- Explicitly enabled active boot-keyboard and boot-mouse proxying through USB-C,
  alongside CDC diagnostics. M7 polls the physical USB-A device; M4 validates
  and forwards bounded reports through generic emulated HID interfaces.
- Dedicated ordered proxy IPC with consumption acknowledgments, generation-bound
  enable requests, heartbeats, release-status reporting and fail-closed overflow.
- Keyboard Num/Caps/Scroll LED forwarding using M7-owned control data-OUT transfers.
- Dashboard support/status and Enable/Disable controls; no automatic forwarding.
- A single offline, verified Node.js installer containing the signed firmware,
  dashboard and bounded part-specific RAM loader. Explicit installation performs
  backup, header-last QSPI provisioning, M4/M7 programming and exact readback
  without changing option bytes or protection/security settings.
- Strict interrupted-header recovery using trusted prior backups and fresh reads;
  failed or uncertain writes are not automatically retried or blindly booted.

### Changed

- Protocol 10 adds proxy commands and events; both core images and the external
  dashboard must be deployed together. RC1 and RC2 use protocol 9.
- Shortened proxy guidance in the dashboard; actionable board-reset instructions
  appear only when input release fails.
- Upgraded Vite to 8.3.8. TypeScript remains on 5.9 because stable
  `typescript-eslint` does not yet support TypeScript 7; no peer checks are bypassed.
- Uncertain proxy output latches USB-C HID and CDC off until physical board reset
  or power cycle. This avoids reusing uncertain endpoint state; HTTP remains
  available. Software tests do not establish actual PC forwarding or recovery.
- Release packaging now includes the authenticated installer runtime/loader and a
  self-contained `.installer.mjs` asset, verification-only tested in CI. Production
  firmware remains logging-disabled. Current-board normal installation and staged
  header recovery passed using test-signed development images; actual HID input
  and on-board HTTP/browser checks remain separate.

### Validation and known limitations

- Passed 208 web unit tests, 59 Chromium fixtures, 165 application native tests,
  seven standalone loader tests, formatting/lint/type checks, and separate
  per-core release Clippy/builds with and without logging.
- The installer completed normal 97-sector replacement and interrupted-header
  recovery on the current board, preserving unrelated flash bytes and option
  settings. These checks used test-signed development images, not the RC3 archive.
- CDC protocol 10 and composite keyboard/mouse enumeration passed. Actual input
  forwarding, key/button releases, keyboard LEDs, output-fault recovery and
  sustained USB/AP coexistence remain hardware-unverified.
- On-board HTTP transfer/browser rendering and real-media storage checks remain
  outstanding. Software fixtures and successful startup do not establish them.

### Installation and verification

- Use the single `giga-usb-v0.1.0-rc3.installer.mjs` from a trusted source with an
  independently trusted Ed25519 public key. It requires Node.js 24+ and an explicit
  STM32CubeProgrammer executable, probe serial and durable backup directory on
  macOS/Linux. No Rust compiler, imgtool or runtime Internet access is needed.
- Run with `--key trusted-public.pem` for verification only. Add `--install` and
  the required programmer/probe/backup arguments to install; see `README.md`.
- The installer verifies the signed archive before board access, provisions the
  dashboard header last, flashes M4 then M7, and verifies readback and preservation
  before system reset. Existing unrelated data, option bytes and OTP/security
  areas are not modified. Flashing M7 overwrites the Arduino bootloader.
- **Never flash the signed container or installer itself as MCU firmware.** This
  is host-side distribution verification, not enforced secure boot.

## [v0.1.0-rc2] - 2026-11-22

Second release candidate. This supersedes rc1 for distribution without logging;
USB functionality, dashboard assets and known hardware-validation limits are
unchanged. It remains experimental firmware, not a stable release.

### Changed

- Release archives now contain logging-disabled firmware, built without `defmt`.
  Packaging rejects ELFs containing defmt metadata or an RTT control block;
  development CI continues checking both logging modes.

### Installation and verification

- Verify `giga-usb-v0.1.0-rc2.signed.bin` with an independently trusted public key
  and compare its signed payload with `giga-usb-v0.1.0-rc2.tar.gz` before extraction.
  Check the extracted checksums and confirm `BUILD.txt` reports `defmt disabled`.
- **Never flash the signed container.** Install the extracted `m4.elf`, then
  `m7.elf`, verify both and system-reset. The matching external dashboard assets
  are required; an identical, already verified container need not be rewritten.
- Routine installation does not require changing option bytes, readout or write
  protection, OTP or security areas. Distribution signing is not secure boot.
- On-board HTTP/browser confirmation, sustained USB/AP traffic and real-media
  storage validation remain outstanding, as described in rc1 below.

## [v0.1.0-rc1] - 2026-11-22

First release candidate for the Arduino GIGA R1 WiFi. This is experimental
firmware, not a stable release. USB compatibility and hardware validation remain
limited; review the known limitations and installation requirements below.

### Added

- Project-owned bare-metal Rust USB host with explicit M7/M4 peripheral ownership.
- Enumeration and descriptor inspection for one directly attached low/full-speed
  USB device, with bounded keyboard/mouse HID processing across four interfaces.
- Host-side transfer records with exact bytes, PID, context and failure diagnostics.
- Experimental read-only BOT/SCSI storage with FAT32 root listings, 8.3 aliases
  and file downloads up to 64 KiB.
- Open `GigaUSB` access point at `192.168.4.1`, DHCP and a dark SolidJS dashboard
  with device details, HID state and a virtualized trace data table.
- Complete pinned offline USB ID registry: 3,427 vendors and 20,537 products from
  the 2026.06.26 snapshot, vendor-scoped identification and bundled BSD notices.
- Server-side trace filters/search and cancellable SSE delivery with bounded
  history, reconnect cursors, acknowledged continuity resets and timeouts.
- Suspense loading states that keep connection cancellation accessible.
- Queued IPC delivery, independent USB-C CDC diagnostics and RGB status patterns.
- External dashboard container generation and backup-checked preparation tooling.
- Pull-request validation and signed release archives containing matching M4/M7
  ELFs, dashboard assets, provisioning guidance, build identity and checksums.

### Changed

- Dashboard assets are stored in external flash rather than embedded in M7.
  M7 validates the container and streams gzip HTML using bounded, cooperative,
  read-only GPIO SPI access. Missing or invalid assets return a small 503 notice.
- HTTP responses use borrowed worker buffers and in-place SSE page construction,
  retaining three HTTP workers, two SSE admissions and eight network socket slots.

### Fixed

- Backported the upstream CYW43 SDIO transfer-length correction so aligned buffer
  padding is not treated as transferable data.
- Prevented disconnected-channel cleanup from replacing a USB disconnect with a
  controller timeout. The initiating historical link-loss fault remains unresolved.
- Removed an HTTP request-poll stack frame larger than M7's entire available
  stack. The largest identified frame is now 18,304 bytes, with 281,292 bytes
  between static RAM and the initial stack pointer in the logging-enabled image.
  This is not a formal worst-case stack bound.

### Validation and known limitations

- Software validation passed: 125 native Rust tests, 73 web unit tests, 55 Chromium
  fixture tests, formatting/lint/type checks, and separate per-core release
  Clippy/builds with and without logging. Both images fit their flash banks.
- The offline dashboard was programmed and read back successfully. Corrected
  firmware was flashed and verified; startup validated the assets and reported
  AP availability without a network error. A receiver completed enumeration and
  41 transfers on the preceding image with the same assets, without USB errors
  or host-capture drops; this is not USB validation of the final HTTP correction.
- On-board HTTP transfer, gzip/ETag/304 behavior, browser rendering, sustained or
  concurrent USB/AP traffic, and real-media storage integrity remain unverified.
  Local browser fixtures are not board tests. The reported dashboard failure has
  not been confirmed resolved by a separate AP client.
- Records describe transactions initiated by this board, not passive USB wire
  capture. Handshakes, CRCs and bus timing are not recorded. Bounded queues and
  history do not guarantee lossless delivery.
- Hubs, arbitrary HID layouts, writable storage and general USB class support
  are unsupported. Offline identification is an inference, not authentication.
- The open AP and unauthenticated, unencrypted HTTP require a trusted development
  environment; they are not production security.

### Installation and verification

- Verify `giga-usb-v0.1.0-rc1.signed.bin` using an independently trusted public key,
  then compare its signed payload with `giga-usb-v0.1.0-rc1.tar.gz` before extraction.
  Follow the verification and provisioning instructions in `README.md` and `AGENTS.md`.
- **Never flash the signed container.** Signing authenticates the distribution;
  it does not provide secure boot.
- Provision the matching external dashboard assets using a validated part-specific
  loader, then flash `m4.elf` followed by `m7.elf`, verify and system-reset using
  an external SWD probe. Installing M7 alone does not install the dashboard.
- **Flashing M7 overwrites the Arduino bootloader.** Ordinary updates do not
  require mass erase or option-byte changes. Preserve existing external data
  unless its owner explicitly authorizes discarding it.

[Unreleased]: https://github.com/anapeksha/giga-usb/compare/v0.1.0-rc3...HEAD
[v0.1.0-rc3]: https://github.com/anapeksha/giga-usb/compare/v0.1.0-rc2...v0.1.0-rc3
[v0.1.0-rc2]: https://github.com/anapeksha/giga-usb/compare/v0.1.0-rc1...v0.1.0-rc2
[v0.1.0-rc1]: https://github.com/anapeksha/giga-usb/releases/tag/v0.1.0-rc1
