//! Plaintext Wi-Fi persistence in the last two bank-2 sectors.
//!
//! Integration requires reserving 0x081c0000..0x08200000 in the M4 linker map
//! (768 KiB application flash). Never use this backend with an overlapping image.
//! M7 must be the sole owner of FLASH, including while M4 runs. Bank-2 program/
//! erase stalls M4 instruction/data fetches from that bank; async does not remove
//! this hardware stall. Coordinate latency-sensitive M4 work before saving.
//! Save only after successful association and DHCP. Keep the request alive until
//! save completes, then drop it. Flash is unencrypted; logical replacement does
//! not securely erase old credentials. No secret values may be logged.
//! Sequential-storage v8 recovery is best-effort, not a guarantee against every
//! possible power-loss/erase pattern or hardware ECC fault. The tests cover
//! selected operation boundaries and partial-erase prefixes, not real H7 faults.

use embedded_storage_async::nor_flash::NorFlash;
use heapless::String;
use sequential_storage::{
    cache::{Cache, Uncached},
    map::{MapConfig, MapStorage},
};
use serde::{Deserialize, Serialize};

use shared::wifi::WifiRequest;

pub const FLASH_START: u32 = shared::M4_FLASH_END;
pub const FLASH_END: u32 = 0x0820_0000;
pub const REGION_SIZE: usize = (FLASH_END - FLASH_START) as usize;
pub const ERASE_SIZE: usize = 128 * 1024;
const KEY: u8 = 1;
const BUFFER_SIZE: usize = 160;

#[derive(Serialize, Deserialize)]
struct StoredCredentials {
    schema: u8,
    ssid: String<32>,
    password: String<64>,
}

impl Drop for StoredCredentials {
    fn drop(&mut self) {
        let mut bytes = core::mem::take(&mut self.password).into_bytes();
        bytes.as_mut_slice().fill(0);
        core::hint::black_box(&bytes);
    }
}

/// Errors contain no credential payloads.
#[derive(Debug)]
pub enum StoreError<E> {
    Storage(sequential_storage::Error<E>),
    InvalidCredentials,
    Encoding,
}

/// Owns a region-relative flash device. Construction and empty loads do not
/// format flash. The library may repair damaged metadata within this region.
/// RAM clearing is best-effort, not guaranteed compiler/driver zeroization.
pub struct CredentialStore<F: NorFlash> {
    map: MapStorage<u8, F, Cache<Uncached, Uncached, Uncached, u8>>,
}

impl<F: NorFlash> CredentialStore<F> {
    /// The device must expose exactly two erase pages. Geometry is checked before
    /// accepting ownership; for hardware use only `prepare_m7`'s bounded device.
    pub fn new(flash: F) -> Result<Self, F> {
        if F::READ_SIZE != 1
            || F::WRITE_SIZE != 32
            || F::ERASE_SIZE != ERASE_SIZE
            || flash.capacity() != REGION_SIZE
        {
            return Err(flash);
        }
        Ok(Self {
            map: MapStorage::new(
                flash,
                MapConfig::new(0..REGION_SIZE as u32),
                Cache::new_uncached(),
            ),
        })
    }

    /// Return the latest valid schema-1 request, ignoring malformed/unknown
    /// payloads among records still retained in the log. Garbage collection may
    /// discard older versions; fallback is not a permanent backup history.
    /// CRC-damaged and interrupted items are handled by the library.
    /// An I/O or unrecoverable metadata error is returned, not silently formatted.
    pub async fn load(&mut self) -> Result<Option<WifiRequest>, StoreError<F::Error>> {
        let mut buffer = [0u8; BUFFER_SIZE];
        let result = async {
            let mut iter = self
                .map
                .fetch_all_items(&mut buffer)
                .await
                .map_err(StoreError::Storage)?;
            let mut latest = None;
            while let Some((key, bytes)) = iter
                .next::<&[u8]>(&mut buffer)
                .await
                .map_err(StoreError::Storage)?
            {
                if key != KEY {
                    continue;
                }
                let Ok(record) = postcard::from_bytes::<StoredCredentials>(bytes) else {
                    continue;
                };
                if record.schema != 1 {
                    continue;
                }
                let request = WifiRequest {
                    ssid: record.ssid.clone(),
                    password: record.password.clone(),
                };
                if request.validate().is_ok() {
                    latest = Some(request);
                }
            }
            Ok(latest)
        }
        .await;
        buffer.fill(0);
        core::hint::black_box(&buffer);
        result
    }

    pub async fn save(&mut self, request: &WifiRequest) -> Result<(), StoreError<F::Error>> {
        request
            .validate()
            .map_err(|_| StoreError::InvalidCredentials)?;
        let record = StoredCredentials {
            schema: 1,
            ssid: request.ssid.clone(),
            password: request.password.clone(),
        };
        let mut encoded = [0u8; BUFFER_SIZE];
        let mut scratch = [0u8; BUFFER_SIZE];
        let result = async {
            let bytes =
                postcard::to_slice(&record, &mut encoded).map_err(|_| StoreError::Encoding)?;
            self.map
                .store_item(&mut scratch, &KEY, &&*bytes)
                .await
                .map_err(StoreError::Storage)
        }
        .await;
        encoded.fill(0);
        scratch.fill(0);
        core::hint::black_box((&encoded, &scratch));
        result
    }

    pub fn into_inner(self) -> F {
        self.map.destroy().0
    }
}

#[cfg(target_os = "none")]
mod hardware {
    use super::*;
    use embassy_stm32::{
        Peri,
        flash::{self, Flash},
        peripherals::FLASH,
    };
    use embedded_storage_async::nor_flash::{ErrorType, ReadNorFlash};

    embassy_stm32::bind_interrupts!(struct Irqs { FLASH => flash::InterruptHandler; });

    /// Private inner driver prevents bypassing region checks. Dropping this value
    /// drops the HAL driver/token; it does not return a reusable FLASH token.
    pub struct CredentialFlash<'d> {
        flash: Flash<'d>,
    }

    /// Consume the sole M7 FLASH token at boot. The caller must first reserve the
    /// region in both images and must never create another FLASH owner on M4.
    pub fn prepare_m7(token: Peri<'_, FLASH>) -> CredentialFlash<'_> {
        CredentialFlash {
            flash: Flash::new(token, Irqs),
        }
    }

    fn offset(address: u32, len: usize) -> Result<u32, flash::Error> {
        let len = u32::try_from(len).map_err(|_| flash::Error::Size)?;
        if address
            .checked_add(len)
            .is_none_or(|end| end > REGION_SIZE as u32)
        {
            return Err(flash::Error::Size);
        }
        Ok(FLASH_START - flash::FLASH_BASE as u32 + address)
    }

    impl ErrorType for CredentialFlash<'_> {
        type Error = flash::Error;
    }
    impl ReadNorFlash for CredentialFlash<'_> {
        const READ_SIZE: usize = 1;
        async fn read(&mut self, address: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            self.flash
                .blocking_read(offset(address, bytes.len())?, bytes)
        }
        fn capacity(&self) -> usize {
            REGION_SIZE
        }
    }
    impl NorFlash for CredentialFlash<'_> {
        const WRITE_SIZE: usize = 32;
        const ERASE_SIZE: usize = ERASE_SIZE;
        async fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            let translated = offset(address, bytes.len())?;
            self.flash.write(translated, bytes).await
        }
        async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            let len = to.checked_sub(from).ok_or(flash::Error::Size)?;
            let translated = offset(from, len as usize)?;
            self.flash.erase(translated, translated + len).await
        }
    }
}
#[cfg(target_os = "none")]
pub use hardware::{CredentialFlash, prepare_m7};

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use embedded_storage_async::nor_flash::{ErrorType, NorFlashErrorKind, ReadNorFlash};
    use std::{
        boxed::Box,
        future::Future,
        task::{Context, Poll, Waker},
        vec,
        vec::Vec,
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Operation {
        Read,
        Write(u32),
        Erase(u32, u32),
    }

    #[derive(Clone)]
    struct Mock {
        bytes: Vec<u8>,
        writes: usize,
        erases: usize,
        fail_after: Option<usize>,
        operations: usize,
        fail_operation: Option<usize>,
        trace: Option<Vec<Operation>>,
        partial_erase: usize,
    }
    impl Mock {
        fn new() -> Self {
            Self {
                bytes: vec![255; REGION_SIZE],
                writes: 0,
                erases: 0,
                fail_after: None,
                operations: 0,
                fail_operation: None,
                trace: None,
                partial_erase: ERASE_SIZE / 2,
            }
        }
    }
    impl Mock {
        fn operation(&mut self, operation: Operation) -> bool {
            if let Some(trace) = &mut self.trace {
                trace.push(operation);
            }
            let failed = self
                .fail_operation
                .is_some_and(|cut| self.operations >= cut);
            self.operations += 1;
            failed
        }

        fn reset_operations(&mut self) {
            self.operations = 0;
            self.fail_operation = None;
            self.trace = None;
        }
    }
    impl ErrorType for Mock {
        type Error = NorFlashErrorKind;
    }
    impl ReadNorFlash for Mock {
        const READ_SIZE: usize = 1;
        async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            if self.operation(Operation::Read) {
                return Err(NorFlashErrorKind::Other);
            }
            let src = self
                .bytes
                .get(offset as usize..offset as usize + bytes.len())
                .ok_or(NorFlashErrorKind::OutOfBounds)?;
            bytes.copy_from_slice(src);
            Ok(())
        }
        fn capacity(&self) -> usize {
            self.bytes.len()
        }
    }
    impl NorFlash for Mock {
        const WRITE_SIZE: usize = 32;
        const ERASE_SIZE: usize = ERASE_SIZE;
        async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            assert_eq!(offset % 32, 0);
            assert_eq!(bytes.len() % 32, 0);
            for (word, chunk) in bytes.chunks(32).enumerate() {
                if self.operation(Operation::Write(offset + word as u32 * 32))
                    || self.fail_after == Some(self.writes)
                {
                    // A torn word can contain only some programmed bits.
                    self.bytes[offset as usize + word * 32] &= chunk[0];
                    return Err(NorFlashErrorKind::Other);
                }
                let start = offset as usize + word * 32;
                for (old, new) in self.bytes[start..start + 32].iter_mut().zip(chunk) {
                    assert_eq!(*old & new, *new);
                    *old &= new;
                }
                self.writes += 1;
            }
            Ok(())
        }
        async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            assert_eq!(from as usize % ERASE_SIZE, 0);
            assert_eq!(to as usize % ERASE_SIZE, 0);
            if self.operation(Operation::Erase(from, to)) {
                // Model loss of power during erase: only a prefix reaches 0xff.
                let end = from as usize + self.partial_erase.min((to - from) as usize);
                self.bytes[from as usize..end].fill(255);
                return Err(NorFlashErrorKind::Other);
            }
            self.bytes[from as usize..to as usize].fill(255);
            self.erases += 1;
            Ok(())
        }
    }
    fn run<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        let mut context = Context::from_waker(Waker::noop());
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("mock flash must complete immediately"),
        }
    }
    fn store(mock: Mock) -> CredentialStore<Mock> {
        CredentialStore::new(mock).unwrap_or_else(|_| panic!("invalid geometry"))
    }
    fn request(ssid: &str) -> WifiRequest {
        WifiRequest {
            ssid: String::try_from(ssid).unwrap(),
            password: String::try_from("secret123").unwrap(),
        }
    }
    #[test]
    fn empty_load_is_read_only() {
        let mut s = store(Mock::new());
        assert!(run(s.load()).unwrap().is_none());
        let flash = s.into_inner();
        assert_eq!((flash.writes, flash.erases), (0, 0));
    }
    #[test]
    fn read_failure_and_invalid_payload_do_not_format() {
        let mut flash = Mock::new();
        flash.fail_operation = Some(0);
        let mut s = store(flash);
        assert!(matches!(run(s.load()), Err(StoreError::Storage(_))));
        let mut flash = s.into_inner();
        assert_eq!((flash.writes, flash.erases), (0, 0));
        flash.reset_operations();
        let mut s = store(flash);
        let mut scratch = [0; BUFFER_SIZE];
        run(s.map.store_item(&mut scratch, &KEY, &&b"invalid"[..])).unwrap();
        let flash = s.into_inner();
        let counts = (flash.writes, flash.erases);
        let mut s = store(flash);
        assert!(run(s.load()).unwrap().is_none());
        let flash = s.into_inner();
        assert_eq!((flash.writes, flash.erases), counts);
    }

    #[test]
    fn erase_failure_changes_only_a_prefix() {
        let mut flash = Mock::new();
        flash.bytes[ERASE_SIZE..].fill(0);
        flash.partial_erase = 32;
        flash.fail_operation = Some(0);
        assert!(run(flash.erase(ERASE_SIZE as u32, REGION_SIZE as u32)).is_err());
        assert!(
            flash.bytes[ERASE_SIZE..ERASE_SIZE + 32]
                .iter()
                .all(|b| *b == 255)
        );
        assert!(flash.bytes[ERASE_SIZE + 32..].iter().all(|b| *b == 0));
        assert_eq!(flash.erases, 0);
    }

    #[test]
    fn repeated_saves_and_reopen_use_latest() {
        let mut s = store(Mock::new());
        for name in ["first", "second", "third"] {
            run(s.save(&request(name))).unwrap();
        }
        let mut s = store(s.into_inner());
        assert_eq!(run(s.load()).unwrap().unwrap().ssid, "third");
    }
    #[test]
    fn rollover_power_loss_recovers_and_can_save_again() {
        // Fill once, retaining only the snapshot immediately before the second
        // rollover, when GC erases the final page rather than the first page.
        let mut flash = Mock::new();
        let mut replacement = request("even");
        replacement.password =
            String::try_from("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        let mut prefix = (0..3000)
            .find_map(|i| {
                let before = flash.clone();
                replacement.ssid =
                    String::try_from(if i % 2 == 0 { "even" } else { "odd" }).unwrap();
                let mut s = store(flash.clone());
                run(s.save(&replacement)).unwrap();
                flash = s.into_inner();
                (flash.erases == 2).then_some(before)
            })
            .expect("must reach final-page GC");
        // Compact obsolete history in the fixture without changing occupied
        // space: sequential-storage v8's zero data-CRC marks an erased item.
        // Keep the final committed record intact. This is test-only fixture
        // construction, not a claim that the H7 supports multiwrite deletion.
        // It avoids quadratic uncached latest-key scans on every replay.
        let mut headers = Vec::new();
        let mut address = ERASE_SIZE + 32;
        while prefix.bytes[address..address + 32]
            .iter()
            .any(|b| *b != 255)
        {
            headers.push(address);
            let length =
                u16::from_le_bytes(prefix.bytes[address + 4..address + 6].try_into().unwrap())
                    as usize;
            address += 32 + length.next_multiple_of(32);
            if address >= REGION_SIZE - 32 {
                break;
            }
        }
        assert!(headers.len() > 2);
        for &header in &headers[..headers.len() - 1] {
            prefix.bytes[header..header + 4].fill(0);
        }
        let mut s = store(prefix.clone());
        let old = run(s.load()).unwrap().unwrap();

        // Trace a successful rollover, then replay only the short suffix from
        // this snapshot. Cut every programmed word/erase and their neighboring
        // reads, plus the first and last operation; not every prefix read.
        let mut baseline = prefix.clone();
        baseline.reset_operations();
        baseline.trace = Some(Vec::new());
        let mut s = store(baseline);
        run(s.save(&replacement)).unwrap();
        let baseline = s.into_inner();
        let trace = baseline.trace.unwrap();
        assert!(trace.contains(&Operation::Erase(ERASE_SIZE as u32, REGION_SIZE as u32)));
        for address in [0, 32, 64, REGION_SIZE as u32 - 32] {
            assert!(
                trace.contains(&Operation::Write(address)),
                "missing page-state/migration word at {address}"
            );
        }
        assert!(
            trace
                .iter()
                .filter(|op| matches!(op, Operation::Write(_)))
                .count()
                >= 6,
            "rollover must include page-state, migration and new-record writes"
        );
        let mut cuts = vec![0, trace.len() - 1];
        for (index, op) in trace.iter().enumerate() {
            if !matches!(op, Operation::Read) {
                cuts.extend(index.saturating_sub(1)..=(index + 1).min(trace.len() - 1));
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for cut in cuts {
            let erase_prefixes: &[usize] = if matches!(trace[cut], Operation::Erase(..)) {
                &[1, 32, ERASE_SIZE / 2, ERASE_SIZE - 32]
            } else {
                &[ERASE_SIZE / 2]
            };
            for &partial_erase in erase_prefixes {
                let mut faulty = prefix.clone();
                faulty.reset_operations();
                faulty.fail_operation = Some(cut);
                faulty.partial_erase = partial_erase;
                let mut s = store(faulty);
                assert!(
                    run(s.save(&replacement)).is_err(),
                    "cut {cut}: {:?}",
                    trace[cut]
                );
                let mut interrupted = s.into_inner();
                assert!(interrupted.operations > cut);
                interrupted.reset_operations();
                let mut reopened = store(interrupted);
                let loaded = run(reopened.load())
                    .unwrap_or_else(|error| {
                        panic!(
                            "cut {cut} {:?}, erase prefix {partial_erase}: {error:?}",
                            trace[cut]
                        )
                    })
                    .expect("GC interruption must retain a valid credential");
                assert!(loaded.ssid == old.ssid || loaded.ssid == replacement.ssid);
                assert_eq!(loaded.password, replacement.password);
                assert!(loaded.validate().is_ok());
                run(reopened.save(&request("after-recovery"))).unwrap();
                let mut reopened = store(reopened.into_inner());
                assert_eq!(
                    run(reopened.load()).unwrap().unwrap().ssid,
                    "after-recovery"
                );
            }
        }
    }

    #[test]
    fn interrupted_writes_recover_committed_value() {
        for cut in 0..5 {
            let mut s = store(Mock::new());
            run(s.save(&request("committed"))).unwrap();
            let mut flash = s.into_inner();
            flash.fail_after = Some(flash.writes + cut);
            let mut s = store(flash);
            let saved = run(s.save(&request("replacement"))).is_ok();
            let mut flash = s.into_inner();
            flash.fail_after = None;
            let mut s = store(flash);
            let loaded = run(s.load()).unwrap().unwrap();
            assert_eq!(loaded.ssid, if saved { "replacement" } else { "committed" });
        }
    }
    #[test]
    fn malformed_unknown_and_crc_corrupt_records_fall_back() {
        let mut s = store(Mock::new());
        run(s.save(&request("valid"))).unwrap();
        let mut scratch = [0; BUFFER_SIZE];
        run(s
            .map
            .store_item(&mut scratch, &KEY, &&b"invalid postcard"[..]))
        .unwrap();
        let unknown = StoredCredentials {
            schema: 2,
            ssid: String::try_from("future").unwrap(),
            password: String::new(),
        };
        let mut encoded = [0; BUFFER_SIZE];
        let bytes = postcard::to_slice(&unknown, &mut encoded).unwrap();
        run(s.map.store_item(&mut scratch, &KEY, &&*bytes)).unwrap();
        assert_eq!(run(s.load()).unwrap().unwrap().ssid, "valid");
        run(s.save(&request("corrupt-me"))).unwrap();
        let mut flash = s.into_inner();
        let pos = flash
            .bytes
            .windows(10)
            .position(|w| w == b"corrupt-me")
            .unwrap();
        flash.bytes[pos] ^= 1;
        let mut s = store(flash);
        assert_eq!(run(s.load()).unwrap().unwrap().ssid, "valid");
    }
}
