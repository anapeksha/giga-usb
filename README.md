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

Build the dashboard before M7, and rebuild M7 after UI changes. Build each core
separately; do not use `--workspace` or `--all-features` for firmware builds.
See [engineering checks](AGENTS.md#builds-and-tooling) for the full validation steps.

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

Verify programming and use a full system reset. Do not mass/chip erase: saved
Wi-Fi credentials must survive ordinary sector-scoped updates. Recovery and
CubeProgrammer instructions are in the [engineering guide](AGENTS.md#flash-log-and-debug).

## Use

1. Power the board normally. Without valid saved credentials, join **GigaUSB**
   from a separate phone or laptop and open **http://192.168.4.1/**.
2. Connect a supported peripheral to **USB-A**, then click **Start**.
   **Never connect USB-A to another computer.** Respect the board's power limits.
3. Inspect topology, input and traces. **Stop** disables the host;
   browser **Disconnect** only stops polling. Exports may contain sensitive input.
4. Optionally enter router credentials in the Wi-Fi card. After the setup AP
   closes, reconnect your client to the router and find the board's IP in its
   DHCP leases. There is no automatic redirect or mDNS.

Saved credentials are tried on boot; failed connections restore setup mode.
Use a separate client for provisioning; do not change the development Mac's Wi-Fi.
The AP is open, HTTP is unauthenticated and unencrypted, and stored credentials
are plaintext. Use only in a trusted environment.

For connection problems, LED meanings, support limits and dated validation
results, see [AGENTS.md](AGENTS.md). For dashboard development commands, see
[web/README.md](web/README.md).
