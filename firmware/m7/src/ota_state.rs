//! Atomic logical STATE over two independently erasable external NOR sectors.
//!
//! The owner must supply an exclusive, offset-zero 8192-byte partition inside
//! reserved OTA STATE (external 0x200000..0x220000), never the whole chip.
//! Backend operations must have deadlines. No operation is retried here. After
//! an uncertain mutation this instance is poisoned; reopen before further use.
//!
//! ABI v1 (all integers little endian): record bytes 0..8 = `GIGAST01`,
//! 8..12 = version 1, 12..16 = wrapping generation, 16..80 = logical STATE,
//! 80..123 = zero reserved bytes, 123..127 = CRC-32/ISO-HDLC over 0..123,
//! 127 = commit byte 0x00 (0xff until committed). Records are 128 bytes,
//! 32 per bank, two 4096-byte banks. Factory image is seed_bytes() followed
//! by 8064 bytes of 0xff. CRC is corruption detection, not authentication.
//! Interrupted erase/program must affect only the requested physical range.
//! Arbitrary adversarial corruption, CRC collisions and faulty isolation are
//! outside this power-loss model. Never format as an automatic open fallback.

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

pub const STATE_CAPACITY: usize = 64;
pub const PHYSICAL_SIZE: usize = 8192;
pub const BANK_SIZE: usize = 4096;
pub const RECORD_SIZE: usize = 128;
pub const FORMAT_VERSION: u32 = 1;
pub const RECORD_MAGIC: [u8; 8] = *b"GIGAST01";
pub const VERSION_OFFSET: usize = 8;
pub const GENERATION_OFFSET: usize = 12;
pub const STATE_OFFSET: usize = 16;
pub const RESERVED_OFFSET: usize = STATE_OFFSET + STATE_CAPACITY;
pub const CRC_OFFSET: usize = 123;
pub const COMMIT_OFFSET: usize = 127;
const RECORDS: usize = PHYSICAL_SIZE / RECORD_SIZE;
const PER_BANK: usize = BANK_SIZE / RECORD_SIZE;

#[derive(Debug, PartialEq, Eq)]
pub enum StateError<E> {
    Flash(E),
    Geometry,
    Uninitialized,
    NotErased,
    Ambiguous,
    Verification,
    OutOfBounds,
    NotAligned,
    NeedsErase,
    Poisoned,
}

impl<E: NorFlashError> NorFlashError for StateError<E> {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Flash(e) => e.kind(),
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
            _ => NorFlashErrorKind::Other,
        }
    }
}

const fn crc(bytes: &[u8], end: usize) -> u32 {
    let mut value = !0u32;
    let mut i = 0;
    while i < end {
        value ^= bytes[i] as u32;
        let mut bit = 0;
        while bit < 8 {
            value = (value >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(value & 1)));
            bit += 1;
        }
        i += 1;
    }
    !value
}

const fn encode(state: &[u8; STATE_CAPACITY], generation: u32) -> [u8; RECORD_SIZE] {
    let mut record = [0; RECORD_SIZE];
    let mut i = 0;
    while i < 8 {
        record[i] = RECORD_MAGIC[i];
        i += 1;
    }
    let version = FORMAT_VERSION.to_le_bytes();
    let generation = generation.to_le_bytes();
    i = 0;
    while i < 4 {
        record[VERSION_OFFSET + i] = version[i];
        record[GENERATION_OFFSET + i] = generation[i];
        i += 1;
    }
    i = 0;
    while i < STATE_CAPACITY {
        record[STATE_OFFSET + i] = state[i];
        i += 1;
    }
    let checksum = crc(&record, CRC_OFFSET).to_le_bytes();
    i = 0;
    while i < 4 {
        record[CRC_OFFSET + i] = checksum[i];
        i += 1;
    }
    record
}

/// Exact committed generation-zero factory record; remainder of partition is FF.
pub const fn seed_bytes() -> [u8; RECORD_SIZE] {
    encode(&[0xff; STATE_CAPACITY], 0)
}

fn word(record: &[u8; RECORD_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes([
        record[offset],
        record[offset + 1],
        record[offset + 2],
        record[offset + 3],
    ])
}

fn valid(record: &[u8; RECORD_SIZE]) -> bool {
    record[..8] == RECORD_MAGIC
        && word(record, VERSION_OFFSET) == FORMAT_VERSION
        && record[RESERVED_OFFSET..CRC_OFFSET].iter().all(|b| *b == 0)
        && record[COMMIT_OFFSET] == 0
        && word(record, CRC_OFFSET) == crc(record, CRC_OFFSET)
}

pub struct JournalState<F: NorFlash> {
    flash: F,
    state: [u8; STATE_CAPACITY],
    generation: u32,
    bank: usize,
    next: [usize; 2],
    poisoned: bool,
}

impl<F: NorFlash> JournalState<F> {
    fn geometry(flash: &F) -> Result<(), StateError<F::Error>> {
        if flash.capacity() != PHYSICAL_SIZE
            || F::ERASE_SIZE != BANK_SIZE
            || F::WRITE_SIZE != 1
            || F::READ_SIZE != 1
        {
            return Err(StateError::Geometry);
        }
        Ok(())
    }

    pub fn open(mut flash: F) -> Result<Self, StateError<F::Error>> {
        Self::geometry(&flash)?;
        let mut generations = [None; RECORDS];
        let mut next = [0; 2];
        let mut best: Option<(usize, u32, [u8; STATE_CAPACITY])> = None;
        for index in 0..RECORDS {
            let mut record = [0; RECORD_SIZE];
            flash
                .read((index * RECORD_SIZE) as u32, &mut record)
                .map_err(StateError::Flash)?;
            if record.iter().any(|b| *b != 0xff) {
                next[index / PER_BANK] = index % PER_BANK + 1;
            }
            if !valid(&record) {
                continue;
            }
            let generation = word(&record, GENERATION_OFFSET);
            // A legitimate live window spans at most 63 generations. Pairwise
            // checking rejects duplicate/split-brain and nontransitive serial order.
            for previous in generations.iter().flatten() {
                let distance = generation.wrapping_sub(*previous);
                if distance == 0
                    || (distance >= RECORDS as u32 && distance <= u32::MAX - (RECORDS as u32 - 1))
                {
                    return Err(StateError::Ambiguous);
                }
            }
            generations[index] = Some(generation);
            if best
                .as_ref()
                .is_none_or(|(_, old, _)| generation.wrapping_sub(*old) < RECORDS as u32)
            {
                let mut state = [0; STATE_CAPACITY];
                state.copy_from_slice(&record[STATE_OFFSET..STATE_OFFSET + STATE_CAPACITY]);
                best = Some((index / PER_BANK, generation, state));
            }
        }
        let (bank, generation, state) = best.ok_or(StateError::Uninitialized)?;
        Ok(Self {
            flash,
            state,
            generation,
            bank,
            next,
            poisoned: false,
        })
    }

    /// Only accepts a completely erased partition. Does not perform an erase.
    pub fn format_erased(mut flash: F) -> Result<Self, StateError<F::Error>> {
        Self::geometry(&flash)?;
        Self::verify_erased(&mut flash, 0, PHYSICAL_SIZE)?;
        let mut journal = Self {
            flash,
            state: [0xff; STATE_CAPACITY],
            generation: u32::MAX,
            bank: 0,
            next: [0; 2],
            poisoned: false,
        };
        journal.persist([0xff; STATE_CAPACITY])?;
        Ok(journal)
    }

    pub fn into_inner(self) -> F {
        self.flash
    }

    fn verify_erased(flash: &mut F, start: usize, end: usize) -> Result<(), StateError<F::Error>> {
        for offset in (start..end).step_by(RECORD_SIZE) {
            let mut bytes = [0; RECORD_SIZE];
            flash
                .read(offset as u32, &mut bytes)
                .map_err(StateError::Flash)?;
            if bytes.iter().any(|b| *b != 0xff) {
                return Err(StateError::NotErased);
            }
        }
        Ok(())
    }

    fn ready(&self) -> Result<(), StateError<F::Error>> {
        if self.poisoned {
            Err(StateError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn persist(&mut self, state: [u8; STATE_CAPACITY]) -> Result<(), StateError<F::Error>> {
        self.ready()?;
        self.poisoned = true;
        let mut bank = self.bank;
        if self.next[bank] == PER_BANK {
            bank ^= 1;
            let start = bank * BANK_SIZE;
            self.flash
                .erase(start as u32, (start + BANK_SIZE) as u32)
                .map_err(StateError::Flash)?;
            Self::verify_erased(&mut self.flash, start, start + BANK_SIZE)?;
            self.next[bank] = 0;
        }
        let offset = bank * BANK_SIZE + self.next[bank] * RECORD_SIZE;
        Self::verify_erased(&mut self.flash, offset, offset + RECORD_SIZE)?;
        let generation = self.generation.wrapping_add(1);
        let record = encode(&state, generation);
        self.flash
            .write(offset as u32, &record[..COMMIT_OFFSET])
            .map_err(StateError::Flash)?;
        let mut check = [0xff; RECORD_SIZE];
        self.flash
            .read(offset as u32, &mut check)
            .map_err(StateError::Flash)?;
        if check[..COMMIT_OFFSET] != record[..COMMIT_OFFSET]
            || check[COMMIT_OFFSET] != 0xff
            || word(&check, CRC_OFFSET) != crc(&check, CRC_OFFSET)
        {
            return Err(StateError::Verification);
        }
        self.flash
            .write((offset + COMMIT_OFFSET) as u32, &[0])
            .map_err(StateError::Flash)?;
        self.flash
            .read(offset as u32, &mut check)
            .map_err(StateError::Flash)?;
        if check != record {
            return Err(StateError::Verification);
        }
        self.state = state;
        self.generation = generation;
        self.bank = bank;
        self.next[bank] += 1;
        self.poisoned = false;
        Ok(())
    }

    fn range(offset: u32, len: usize) -> Result<core::ops::Range<usize>, StateError<F::Error>> {
        let start = offset as usize;
        let end = start.checked_add(len).ok_or(StateError::OutOfBounds)?;
        if end > STATE_CAPACITY {
            return Err(StateError::OutOfBounds);
        }
        Ok(start..end)
    }
}

impl<F: NorFlash> ErrorType for JournalState<F> {
    type Error = StateError<F::Error>;
}
impl<F: NorFlash> ReadNorFlash for JournalState<F> {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        STATE_CAPACITY
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.ready()?;
        bytes.copy_from_slice(&self.state[Self::range(offset, bytes.len())?]);
        Ok(())
    }
}
impl<F: NorFlash> NorFlash for JournalState<F> {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = STATE_CAPACITY;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.ready()?;
        let range = Self::range(offset, bytes.len())?;
        let mut state = self.state;
        for (old, new) in state[range].iter_mut().zip(bytes) {
            if *old & *new != *new {
                return Err(StateError::NeedsErase);
            }
            *old = *new;
        }
        if state == self.state {
            return Ok(());
        }
        self.persist(state)
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.ready()?;
        if from > to || to as usize > STATE_CAPACITY {
            return Err(StateError::OutOfBounds);
        }
        if !(from as usize).is_multiple_of(STATE_CAPACITY)
            || !(to as usize).is_multiple_of(STATE_CAPACITY)
        {
            return Err(StateError::NotAligned);
        }
        if from == to || self.state == [0xff; STATE_CAPACITY] {
            return Ok(());
        }
        self.persist([0xff; STATE_CAPACITY])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

    #[derive(Debug, PartialEq, Eq)]
    struct Cut;
    impl NorFlashError for Cut {
        fn kind(&self) -> NorFlashErrorKind {
            NorFlashErrorKind::Other
        }
    }
    #[derive(Clone)]
    struct Ram<const ERASE: usize = BANK_SIZE> {
        bytes: Rc<RefCell<Vec<u8>>>,
        control: Rc<RefCell<(usize, Option<usize>, usize)>>,
    }
    impl<const ERASE: usize> Ram<ERASE> {
        fn new(size: usize) -> Self {
            Self {
                bytes: Rc::new(RefCell::new(vec![0xff; size])),
                control: Rc::new(RefCell::new((0, None, 0))),
            }
        }
        fn arm(&self, call: Option<usize>, mode: usize) {
            *self.control.borrow_mut() = (0, call, mode);
        }
        fn call(&self) -> (bool, usize) {
            let mut c = self.control.borrow_mut();
            c.0 += 1;
            (c.1 == Some(c.0), c.2)
        }
        fn selected(index: usize, len: usize, mode: usize) -> bool {
            match mode {
                0 => false,
                1 => index < len / 2,
                2 => index >= len / 2,
                3 => index % 3 == 1,
                4 => index % 5 != 2,
                _ => true,
            }
        }
    }
    impl<const ERASE: usize> ErrorType for Ram<ERASE> {
        type Error = Cut;
    }
    impl<const ERASE: usize> ReadNorFlash for Ram<ERASE> {
        const READ_SIZE: usize = 1;
        fn capacity(&self) -> usize {
            self.bytes.borrow().len()
        }
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Cut> {
            if self.capacity() == PHYSICAL_SIZE {
                assert!(bytes.len() <= 128, "bounded backend read");
            }
            let (cut, mode) = self.call();
            let source = self.bytes.borrow();
            for (i, b) in bytes.iter_mut().enumerate() {
                if !cut || Self::selected(i, source.len(), mode) {
                    *b = source[offset as usize + i];
                }
            }
            if cut { Err(Cut) } else { Ok(()) }
        }
    }
    impl<const ERASE: usize> NorFlash for Ram<ERASE> {
        const WRITE_SIZE: usize = 1;
        const ERASE_SIZE: usize = ERASE;
        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Cut> {
            if self.capacity() == PHYSICAL_SIZE {
                assert!(bytes.len() <= 127);
            }
            let (cut, mode) = self.call();
            let mut data = self.bytes.borrow_mut();
            for (i, b) in bytes.iter().enumerate() {
                let old = &mut data[offset as usize + i];
                assert_eq!(*old & *b, *b);
                if !cut || Self::selected(i, bytes.len(), mode) {
                    *old &= *b;
                }
            }
            if cut { Err(Cut) } else { Ok(()) }
        }
        fn erase(&mut self, from: u32, to: u32) -> Result<(), Cut> {
            assert_eq!(from as usize % ERASE, 0);
            assert_eq!(to as usize % ERASE, 0);
            if self.capacity() == PHYSICAL_SIZE {
                assert_eq!(to as usize - from as usize, ERASE);
            }
            let (cut, mode) = self.call();
            for (i, b) in self.bytes.borrow_mut()[from as usize..to as usize]
                .iter_mut()
                .enumerate()
            {
                if !cut || Self::selected(i, (to - from) as usize, mode) {
                    *b = 0xff;
                }
            }
            if cut { Err(Cut) } else { Ok(()) }
        }
    }
    fn snapshot(journal: &mut JournalState<Ram>) -> [u8; STATE_CAPACITY] {
        let mut state = [0; STATE_CAPACITY];
        journal.read(0, &mut state).unwrap();
        state
    }
    fn mutation(journal: &mut JournalState<Ram>, step: usize) -> Result<(), StateError<Cut>> {
        if step % 2 == 1 {
            journal.erase(0, 64)
        } else {
            journal.write(0, &[step as u8; 64])
        }
    }

    #[test]
    fn exhaustive_backend_cuts_across_100_mutations_and_rollovers() {
        let flash = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
        let mut journal = JournalState::format_erased(flash.clone()).unwrap();
        for step in 0..100 {
            let before = flash.bytes.borrow().clone();
            let old = snapshot(&mut journal);
            let old_generation = journal.generation;
            flash.arm(None, 0);
            mutation(&mut journal, step).unwrap();
            let calls = flash.control.borrow().0;
            let new = snapshot(&mut journal);
            let after = flash.bytes.borrow().clone();
            for call in 1..=calls {
                for mode in 0..6 {
                    *flash.bytes.borrow_mut() = before.clone();
                    flash.arm(None, 0);
                    let mut attempt = JournalState::open(flash.clone()).unwrap();
                    flash.arm(Some(call), mode);
                    assert_eq!(mutation(&mut attempt, step), Err(StateError::Flash(Cut)));
                    assert_eq!(attempt.read(0, &mut [0]), Err(StateError::Poisoned));
                    flash.arm(None, 0);
                    let mut recovered = JournalState::open(flash.clone()).unwrap();
                    let actual = snapshot(&mut recovered);
                    assert!(
                        actual == old || actual == new,
                        "step {step} call {call} mode {mode}"
                    );
                    assert_eq!(
                        recovered.generation,
                        if actual == old {
                            old_generation
                        } else {
                            old_generation.wrapping_add(1)
                        }
                    );
                    // Recovery must remain writable, including after scattered erases.
                    mutation(&mut recovered, step).unwrap();
                    assert_eq!(snapshot(&mut recovered), new);
                }
            }
            *flash.bytes.borrow_mut() = after;
            flash.arm(None, 0);
            journal = JournalState::open(flash.clone()).unwrap();
        }
    }

    #[test]
    fn factory_seed_crc_and_fail_closed_initialization() {
        assert_eq!(crc(b"123456789", 9), 0xcbf43926);
        // Independently matched with Node 24 node:zlib.crc32 for the ABI port.
        assert_eq!(word(&seed_bytes(), CRC_OFFSET), 0x5453a227);
        let flash = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
        assert!(matches!(
            JournalState::open(flash.clone()),
            Err(StateError::Uninitialized)
        ));
        flash.bytes.borrow_mut()[..128].copy_from_slice(&seed_bytes());
        let mut journal = JournalState::open(flash.clone()).unwrap();
        assert_eq!(snapshot(&mut journal), [0xff; 64]);
        assert!(matches!(
            JournalState::format_erased(flash.clone()),
            Err(StateError::NotErased)
        ));
        flash.bytes.borrow_mut().fill(0x5a);
        assert!(matches!(
            JournalState::open(flash.clone()),
            Err(StateError::Uninitialized)
        ));
        assert!(matches!(
            JournalState::format_erased(flash),
            Err(StateError::NotErased)
        ));
        assert!(matches!(
            JournalState::open(Ram::<2048>::new(PHYSICAL_SIZE)),
            Err(StateError::Geometry)
        ));
        assert!(matches!(
            JournalState::open(Ram::<4096>::new(PHYSICAL_SIZE + 1)),
            Err(StateError::Geometry)
        ));
    }

    #[test]
    fn wrap_corruption_and_split_brain() {
        let flash = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
        flash.bytes.borrow_mut()[..128].copy_from_slice(&encode(&[0xfe; 64], u32::MAX));
        flash.bytes.borrow_mut()[128..256].copy_from_slice(&encode(&[0xfc; 64], 0));
        let mut journal = JournalState::open(flash.clone()).unwrap();
        assert_eq!(snapshot(&mut journal), [0xfc; 64]);
        flash.bytes.borrow_mut()[STATE_OFFSET + 128] ^= 1;
        assert_eq!(
            snapshot(&mut JournalState::open(flash.clone()).unwrap()),
            [0xfe; 64]
        );
        for generation in [u32::MAX, 0x7fffffff, 64, 1000] {
            flash.bytes.borrow_mut()[128..256].copy_from_slice(&encode(&[0xfc; 64], generation));
            assert!(matches!(
                JournalState::open(flash.clone()),
                Err(StateError::Ambiguous)
            ));
        }
    }

    #[test]
    fn rejects_unknown_abi_reserved_and_uncommitted_records() {
        for offset in [0, VERSION_OFFSET, RESERVED_OFFSET, COMMIT_OFFSET] {
            let flash = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
            let mut record = seed_bytes();
            record[offset] ^= 1;
            let checksum = crc(&record, CRC_OFFSET).to_le_bytes();
            record[CRC_OFFSET..COMMIT_OFFSET].copy_from_slice(&checksum);
            flash.bytes.borrow_mut()[..RECORD_SIZE].copy_from_slice(&record);
            assert!(matches!(
                JournalState::open(flash),
                Err(StateError::Uninitialized)
            ));
        }
    }

    #[test]
    fn logical_nor_bounds_and_noop_semantics() {
        let flash = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
        let mut j = JournalState::format_erased(flash.clone()).unwrap();
        j.write(63, &[0]).unwrap();
        flash.arm(None, 0);
        assert_eq!(j.write(63, &[1]), Err(StateError::NeedsErase));
        assert_eq!(j.write(64, &[0]), Err(StateError::OutOfBounds));
        assert_eq!(j.read(u32::MAX, &mut [0]), Err(StateError::OutOfBounds));
        assert_eq!(j.erase(1, 64), Err(StateError::NotAligned));
        assert_eq!(j.erase(64, 0), Err(StateError::OutOfBounds));
        j.write(63, &[0]).unwrap();
        j.write(64, &[]).unwrap();
        j.erase(64, 64).unwrap();
        assert_eq!(flash.control.borrow().0, 0);
        j.erase(0, 64).unwrap();
        assert_eq!(snapshot(&mut j), [0xff; 64]);
    }

    #[test]
    fn every_factory_call_cut_is_uninitialized_or_complete() {
        let reference = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
        JournalState::format_erased(reference.clone()).unwrap();
        let calls = reference.control.borrow().0;
        for call in 1..=calls {
            for mode in 0..6 {
                let flash = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
                flash.arm(Some(call), mode);
                assert!(matches!(
                    JournalState::format_erased(flash.clone()),
                    Err(StateError::Flash(Cut))
                ));
                flash.arm(None, 0);
                match JournalState::open(flash) {
                    Ok(mut j) => {
                        assert_eq!(snapshot(&mut j), [0xff; 64]);
                        assert_eq!(j.generation, 0);
                    }
                    Err(StateError::Uninitialized) => (),
                    _ => panic!("unexpected factory recovery"),
                }
            }
        }
    }

    #[test]
    fn actual_embassy_15_page_swap_rollback_and_confirmation() {
        use embassy_boot::{BlockingFirmwareState, BootLoader, BootLoaderConfig};
        const PAGE: usize = 128 * 1024;
        const ACTIVE: usize = 15 * PAGE;
        for confirm in [false, true] {
            let active = Ram::<PAGE>::new(ACTIVE);
            let dfu = Ram::<4096>::new(ACTIVE + PAGE);
            let physical = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
            let old: Vec<u8> = (0..ACTIVE).map(|i| 0x10 + (i / PAGE) as u8).collect();
            let new: Vec<u8> = (0..ACTIVE).map(|i| 0x80 + (i / PAGE) as u8).collect();
            active.bytes.borrow_mut().copy_from_slice(&old);
            dfu.bytes.borrow_mut()[..ACTIVE].copy_from_slice(&new);
            let journal = JournalState::format_erased(physical.clone()).unwrap();
            BlockingFirmwareState::new(journal, &mut [0])
                .mark_updated()
                .unwrap();
            let boot = || {
                let mut engine = BootLoader::new(BootLoaderConfig {
                    active: active.clone(),
                    dfu: dfu.clone(),
                    state: JournalState::open(physical.clone()).unwrap(),
                });
                #[repr(align(32))]
                struct Buffer([u8; 4096]);
                engine.prepare_boot(&mut Buffer([0; 4096]).0).unwrap();
            };
            boot();
            assert_eq!(*active.bytes.borrow(), new);
            if confirm {
                BlockingFirmwareState::new(JournalState::open(physical.clone()).unwrap(), &mut [0])
                    .mark_booted()
                    .unwrap();
            }
            boot();
            assert_eq!(*active.bytes.borrow(), if confirm { new } else { old });
        }
    }

    #[test]
    fn actual_confirmation_every_call_cut_with_torn_bank_reclaim() {
        use embassy_boot::{BlockingFirmwareState, BootLoader, BootLoaderConfig};
        const PAGE: usize = 128 * 1024;
        const ACTIVE: usize = 15 * PAGE;
        let active = Ram::<PAGE>::new(ACTIVE);
        let dfu = Ram::<4096>::new(ACTIVE + PAGE);
        let physical = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
        let old: Vec<u8> = (0..ACTIVE).map(|i| 0x10 + (i / PAGE) as u8).collect();
        let new: Vec<u8> = (0..ACTIVE).map(|i| 0x80 + (i / PAGE) as u8).collect();
        active.bytes.borrow_mut().copy_from_slice(&old);
        dfu.bytes.borrow_mut()[..ACTIVE].copy_from_slice(&new);
        BlockingFirmwareState::new(
            JournalState::format_erased(physical.clone()).unwrap(),
            &mut [0],
        )
        .mark_updated()
        .unwrap();
        let boot = || {
            let mut engine = BootLoader::new(BootLoaderConfig {
                active: active.clone(),
                dfu: dfu.clone(),
                state: JournalState::open(physical.clone()).unwrap(),
            });
            #[repr(align(32))]
            struct Buffer([u8; 4096]);
            engine.prepare_boot(&mut Buffer([0; 4096]).0).unwrap();
        };
        boot();
        assert_eq!(*active.bytes.borrow(), new);
        let state = snapshot(&mut JournalState::open(physical.clone()).unwrap());
        // Force the next confirmation mutation to reclaim an occupied inactive
        // bank, while retaining the engine's exact post-swap logical state.
        for index in 0..RECORDS {
            physical.bytes.borrow_mut()[index * RECORD_SIZE..(index + 1) * RECORD_SIZE]
                .copy_from_slice(&encode(&state, 100 + index as u32));
        }
        let before = physical.bytes.borrow().clone();
        let dfu_before = dfu.bytes.borrow().clone();
        let confirm = || {
            let journal = JournalState::open(physical.clone()).unwrap();
            physical.arm(None, 0);
            BlockingFirmwareState::new(journal, &mut [0]).mark_booted()
        };
        confirm().unwrap();
        let calls = physical.control.borrow().0;
        for call in 1..=calls {
            for mode in 0..6 {
                *physical.bytes.borrow_mut() = before.clone();
                active.bytes.borrow_mut().copy_from_slice(&new);
                *dfu.bytes.borrow_mut() = dfu_before.clone();
                physical.arm(None, 0);
                let journal = JournalState::open(physical.clone()).unwrap();
                physical.arm(Some(call), mode);
                assert!(
                    BlockingFirmwareState::new(journal, &mut [0])
                        .mark_booted()
                        .is_err()
                );
                physical.arm(None, 0);
                boot();
                let result = active.bytes.borrow();
                assert!(
                    *result == old || *result == new,
                    "confirmation call {call} mode {mode}"
                );
            }
        }
    }

    #[test]
    fn every_open_read_failure_is_exposed() {
        let flash = Ram::<BANK_SIZE>::new(PHYSICAL_SIZE);
        JournalState::format_erased(flash.clone()).unwrap();
        for call in 1..=RECORDS {
            flash.arm(Some(call), 5);
            assert!(matches!(
                JournalState::open(flash.clone()),
                Err(StateError::Flash(Cut))
            ));
        }
    }
}
