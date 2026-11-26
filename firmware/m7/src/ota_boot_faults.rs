//! Qualification of the unmodified Embassy engine before enabling persistent OTA.
//! A passing defect-reproduction test is not a power-loss safety qualification.

use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

use embassy_boot::{BlockingFirmwareState, BootLoader, BootLoaderConfig};
use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

const PAGE: usize = 128 * 1024;
const ACTIVE: usize = 15 * PAGE;
const DFU: usize = ACTIVE + PAGE;
const STATE: usize = PAGE;

#[derive(Clone, Copy, Debug)]
struct PowerLoss;

impl NorFlashError for PowerLoss {
    fn kind(&self) -> NorFlashErrorKind {
        NorFlashErrorKind::Other
    }
}

#[derive(Clone)]
struct Flash<const ERASE: usize, const WRITE: usize> {
    bytes: Rc<RefCell<Vec<u8>>>,
    tear_erase: Rc<RefCell<bool>>,
}

impl<const ERASE: usize, const WRITE: usize> Flash<ERASE, WRITE> {
    fn new(size: usize) -> Self {
        Self {
            bytes: Rc::new(RefCell::new(vec![0xff; size])),
            tear_erase: Rc::new(RefCell::new(false)),
        }
    }
}

impl<const ERASE: usize, const WRITE: usize> ErrorType for Flash<ERASE, WRITE> {
    type Error = PowerLoss;
}

impl<const ERASE: usize, const WRITE: usize> ReadNorFlash for Flash<ERASE, WRITE> {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        let offset = offset as usize;
        bytes.copy_from_slice(&self.bytes.borrow()[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.bytes.borrow().len()
    }
}

impl<const ERASE: usize, const WRITE: usize> NorFlash for Flash<ERASE, WRITE> {
    const WRITE_SIZE: usize = WRITE;
    const ERASE_SIZE: usize = ERASE;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let offset = offset as usize;
        assert_eq!(offset % WRITE, 0);
        assert_eq!(bytes.len() % WRITE, 0);
        for (dst, src) in self.bytes.borrow_mut()[offset..offset + bytes.len()]
            .iter_mut()
            .zip(bytes)
        {
            assert_eq!(*dst & src, *src, "NOR programming cannot set bits");
            *dst &= src;
        }
        Ok(())
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let from = from as usize;
        let to = to as usize;
        assert_eq!(from % ERASE, 0);
        assert_eq!(to % ERASE, 0);
        if *self.tear_erase.borrow() {
            *self.tear_erase.borrow_mut() = false;
            // Interrupted sector erase: validity/progress erase, magic survives.
            // No atomic-sector-erase guarantee is part of NorFlash's contract.
            self.bytes.borrow_mut()[from + 1..to].fill(0xff);
            return Err(PowerLoss);
        }
        self.bytes.borrow_mut()[from..to].fill(0xff);
        Ok(())
    }
}

struct Fixture {
    active: Flash<PAGE, 32>,
    dfu: Flash<4096, 1>,
    state: Flash<4096, 1>,
    old: Vec<u8>,
    new: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let active = Flash::new(ACTIVE);
        let dfu = Flash::new(DFU);
        let state = Flash::new(STATE);
        let mut old = vec![0; ACTIVE];
        let mut new = vec![0; ACTIVE];
        for page in 0..15 {
            old[page * PAGE..(page + 1) * PAGE].fill(0x10 + page as u8);
            new[page * PAGE..(page + 1) * PAGE].fill(0x80 + page as u8);
        }
        active.bytes.borrow_mut().copy_from_slice(&old);
        dfu.bytes.borrow_mut()[..ACTIVE].copy_from_slice(&new);
        let mut word = [0];
        BlockingFirmwareState::new(state.clone(), &mut word)
            .mark_updated()
            .unwrap();
        Self {
            active,
            dfu,
            state,
            old,
            new,
        }
    }

    fn boot(&self) {
        let mut engine = BootLoader::new(BootLoaderConfig {
            active: self.active.clone(),
            dfu: self.dfu.clone(),
            state: self.state.clone(),
        });
        #[repr(align(32))]
        struct Buffer([u8; 4096]);
        engine.prepare_boot(&mut Buffer([0; 4096]).0).unwrap();
    }
}

#[test]
fn actual_engine_swaps_and_rolls_back_both_cores_as_fifteen_pages() {
    let fixture = Fixture::new();
    fixture.boot();
    assert_eq!(*fixture.active.bytes.borrow(), fixture.new);
    fixture.boot();
    assert_eq!(*fixture.active.bytes.borrow(), fixture.old);
}

#[test]
fn actual_engine_confirmed_trial_survives_next_boot() {
    let fixture = Fixture::new();
    fixture.boot();
    let mut word = [0];
    BlockingFirmwareState::new(fixture.state.clone(), &mut word)
        .mark_booted()
        .unwrap();
    fixture.boot();
    assert_eq!(*fixture.active.bytes.borrow(), fixture.new);
}

#[test]
fn reproduces_torn_confirmation_erase_destroying_combined_image() {
    let fixture = Fixture::new();
    fixture.boot();
    assert_eq!(*fixture.active.bytes.borrow(), fixture.new);
    *fixture.state.tear_erase.borrow_mut() = true;
    let mut word = [0];
    assert!(
        BlockingFirmwareState::new(fixture.state.clone(), &mut word)
            .mark_booted()
            .is_err()
    );
    fixture.boot();
    let result = fixture.active.bytes.borrow();
    assert_ne!(*result, fixture.old);
    assert_ne!(*result, fixture.new);
    // M7 occupies seven pages; M4 occupies eight. Neither core remains coherent.
    assert_ne!(&result[..7 * PAGE], &fixture.old[..7 * PAGE]);
    assert_ne!(&result[..7 * PAGE], &fixture.new[..7 * PAGE]);
    assert_ne!(&result[7 * PAGE..], &fixture.old[7 * PAGE..]);
    assert_ne!(&result[7 * PAGE..], &fixture.new[7 * PAGE..]);
}
