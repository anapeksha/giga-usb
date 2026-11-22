# Changelog

Release notes are maintained here. Each release uses its matching tagged entry.

## [Unreleased]

## [v0.1.0] - 2026-11-21

Initial experimental release of GigaUSB for the Arduino GIGA R1 WiFi.

### Added

- Project-owned, bare-metal Rust USB host with explicit M7/M4 peripheral ownership.
- Enumeration of one directly attached low/full-speed USB device, descriptor
  inspection, and bounded keyboard/mouse HID processing across up to four interfaces.
- Host-side transfer records with bytes, PID, context and failure diagnostics.
- Experimental read-only USB storage: BOT/SCSI LUN 0, 512-byte sectors, FAT32 root
  listings using 8.3 aliases, and downloads up to 64 KiB.
- Open `GigaUSB` Wi-Fi access point at `192.168.4.1`, DHCP, and an embedded dark
  dashboard with device details, HID state and a virtualized trace table.
- Server-side trace filters/search and cancellable SSE delivery with bounded
  history, reconnect cursors, acknowledged continuity resets and timeouts.
- Suspense loading states that keep connection cancellation accessible.
- Independent queued IPC delivery and USB-C CDC diagnostics, plus RGB status patterns.
- PR validation and signed release archives containing matching M4/M7 ELFs,
  the compressed dashboard, build identity and checksums.

### Fixed

- Backported the upstream CYW43 SDIO transfer-length correction to avoid aligned
  buffer padding being treated as transferable data.
- Prevented disconnected-channel cleanup from replacing a USB disconnect with a
  controller timeout. Hardware confirmation of this fix remains outstanding.

### Validation and known limits

- Software checks passed: 120 native Rust tests, 68 web unit tests, 54 Chromium
  fixture tests, formatting/lint/type checks, and separate per-core release
  Clippy/builds with and without logging. Both images fit their flash banks.
- Current SSE/Suspense images have not been hardware-validated. On-board browser
  rendering, sustained USB/AP traffic and real-media storage integrity remain
  unverified. The initiating USB disconnect/link-loss fault remains unresolved.
- This observes transactions initiated by this board; it is not passive USB wire
  capture and does not record handshakes, CRCs or bus timing. Queues and history
  are bounded, so delivery is not guaranteed lossless.
- Hubs, arbitrary HID layouts, writable storage and general USB class support are
  not supported. The open AP and unauthenticated HTTP are for trusted development
  environments, not production security.

### Installation and verification

- Verify `giga-usb-v0.1.0.signed.bin` with an independently trusted public key,
  then compare its signed payload with `giga-usb-v0.1.0.tar.gz` before extraction.
  See `AGENTS.md` for the exact verification and flashing procedure.
- **Never flash the signed container.** Signing authenticates the distribution;
  it does not provide secure boot. Flash the extracted `m4.elf` first, then
  `m7.elf`, verify both, and perform a full system reset using an external SWD probe.
- **M7 flashing overwrites the Arduino bootloader.** No mass erase or option-byte
  changes are required for an ordinary update.
