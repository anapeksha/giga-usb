# Changelog

All notable changes to GigaUSB are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and releases follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/anapeksha/giga-usb/compare/v0.1.0-rc2...HEAD
[v0.1.0-rc2]: https://github.com/anapeksha/giga-usb/compare/v0.1.0-rc1...v0.1.0-rc2
[v0.1.0-rc1]: https://github.com/anapeksha/giga-usb/releases/tag/v0.1.0-rc1
