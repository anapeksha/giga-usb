# Local cyw43 0.7.0 patch

## Provenance and scope

Copied from the crates.io `cyw43` 0.7.0 registry package. Its registry VCS metadata
identifies embassy-rs/embassy commit
`50c6aac5b9b3b0af18f5a7d61725cc8e4d1f7acb`, path `cyw43`.
The package remains licensed `MIT OR Apache-2.0`; existing license declarations
and source notices are preserved.

Upstream already has the corresponding SDIO actual-length fix:
https://github.com/embassy-rs/embassy/commit/a52524892bb7dd6128952a38c046dadcb952c7e3

This minimal vendored patch keeps the project's 0.7.0 dependency baseline while
correcting CMD53 read/write splitting: `size_of_val` on `Aligned<A4, [u8]>`
includes alignment padding, whereas `buf.len()` is the actual transfer length.
Both partition calculations, raw block slice counts and size logging use the
actual length. Write failure messages also identify writes correctly. No new
runtime dependencies are added.

## Host regression tests

`src/sdio_tests.rs` invokes the real private `cmd53_read` and `cmd53_write`
functions through a mock `SdioBusCyw43`. Each direction covers every length
`0..=2048`, checking command count/order, direction, function, block/byte mode,
incrementing addresses, encoded counts and actual bus slice lengths. Reads check
patterned data and prefix/suffix guards; writes check exact bytes and unchanged
source storage. Futures must complete on a single poll with a core noop waker.

`no_main` is disabled only for tests. Dev-only `embassy-time/std` and
`critical-section/std` support native linking; runtime features are unchanged.

Validation on aarch64 macOS:

```sh
cargo test --manifest-path vendor/cyw43/Cargo.toml --target aarch64-apple-darwin --lib
# Also passed separately with --features log and --features defmt.
```

The parent workspace must exclude this crate (parent integration is outside this
patch). For local validation before that exclusion existed, an empty `[workspace]`
was temporarily added to this manifest and removed afterward. Both tests were
first run against the original length calculations: both failed at length 61
with a 64-byte slice-bounds panic. After applying the fix, both tests passed.
These are host mock tests, not hardware, AP/HTTP or SDIO electrical validation.

Registry-only `.cargo-ok`, `.cargo_vcs_info.json`, `Cargo.toml.orig` and the
library's copied/generated `Cargo.lock` are omitted. Provenance is retained above;
the integrating application's lockfile controls dependency resolution.
