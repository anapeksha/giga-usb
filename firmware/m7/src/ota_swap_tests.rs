//! Physical-operation power cuts around the real 15-page Embassy swap/revert.
use crate::{ota_nor::Error, ota_state::JournalState};
use core::cell::RefCell;
use embassy_boot::{BlockingFirmwareState, BootLoader, BootLoaderConfig};
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use std::{rc::Rc, vec, vec::Vec};

const PAGE: usize = 0x20000;
const ACTIVE: usize = PAGE * 15;
#[derive(Default)]
struct Fault {
    call: usize,
    cut: Option<usize>,
    selected: Vec<usize>,
}
#[derive(Clone)]
struct Flash<const ERASE: usize, const WRITE: usize> {
    data: Rc<RefCell<Vec<u8>>>,
    fault: Rc<RefCell<Fault>>,
}
impl<const E: usize, const W: usize> Flash<E, W> {
    fn mutation(&self, offset: usize, length: usize, erase: bool) -> bool {
        let mut fault = self.fault.borrow_mut();
        fault.call += 1;
        let call = fault.call;
        let state = self.data.borrow().len() == 8192;
        if (state && (erase || offset % 128 == 0 || offset % 128 == 127))
            || (!state && (erase || offset % PAGE == 0 || (offset + length).is_multiple_of(PAGE)))
        {
            fault.selected.push(call);
        }
        fault.cut == Some(call)
    }
}
impl<const E: usize, const W: usize> ErrorType for Flash<E, W> {
    type Error = Error;
}
impl<const E: usize, const W: usize> ReadNorFlash for Flash<E, W> {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.data.borrow().len()
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Error> {
        bytes.copy_from_slice(&self.data.borrow()[offset as usize..offset as usize + bytes.len()]);
        Ok(())
    }
}
impl<const E: usize, const W: usize> NorFlash for Flash<E, W> {
    const ERASE_SIZE: usize = E;
    const WRITE_SIZE: usize = W;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Error> {
        let offset = offset as usize;
        assert!(offset.is_multiple_of(W));
        assert!(bytes.len().is_multiple_of(W));
        let cut = self.mutation(offset, bytes.len(), false);
        let size = if cut { bytes.len() / 2 } else { bytes.len() };
        for (dst, src) in self.data.borrow_mut()[offset..offset + size]
            .iter_mut()
            .zip(bytes)
        {
            assert_eq!(*dst & src, *src);
            *dst &= src;
        }
        if cut { Err(Error::Uncertain) } else { Ok(()) }
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Error> {
        let from = from as usize;
        let to = to as usize;
        assert!(from.is_multiple_of(E) && to.is_multiple_of(E));
        let cut = self.mutation(from, to - from, true);
        let end = if cut { from + (to - from) / 2 } else { to };
        self.data.borrow_mut()[from..end].fill(0xff);
        if cut { Err(Error::Uncertain) } else { Ok(()) }
    }
}
pub(crate) struct Fixture {
    active: Flash<PAGE, 32>,
    dfu: Flash<4096, 1>,
    state: Flash<4096, 1>,
    fault: Rc<RefCell<Fault>>,
    old: Vec<u8>,
    new: Vec<u8>,
}
impl Fixture {
    fn new() -> Self {
        let fault = Rc::new(RefCell::new(Fault::default()));
        let mut old = vec![0; ACTIVE];
        let mut new = vec![0; ACTIVE];
        for page in 0..15 {
            old[page * PAGE..(page + 1) * PAGE].fill(0x10 + page as u8);
            new[page * PAGE..(page + 1) * PAGE].fill(0x80 + page as u8);
        }
        let active = Flash {
            data: Rc::new(RefCell::new(old.clone())),
            fault: fault.clone(),
        };
        let mut dfu_bytes = new.clone();
        dfu_bytes.resize(ACTIVE + PAGE, 0xff);
        let dfu = Flash {
            data: Rc::new(RefCell::new(dfu_bytes)),
            fault: fault.clone(),
        };
        let mut state_bytes = vec![0xff; 8192];
        state_bytes[..128].copy_from_slice(&crate::ota_state::seed_bytes());
        let state = Flash {
            data: Rc::new(RefCell::new(state_bytes)),
            fault: fault.clone(),
        };
        let journal = JournalState::open(state.clone()).unwrap();
        BlockingFirmwareState::new(journal, &mut [0])
            .mark_updated()
            .unwrap();
        *fault.borrow_mut() = Fault::default();
        Self {
            active,
            dfu,
            state,
            fault,
            old,
            new,
        }
    }
    pub(crate) fn stage(old: &[u8], new: &[u8]) -> Self {
        let mut fixture = Self::new();
        fixture.old.copy_from_slice(old);
        fixture.new.copy_from_slice(new);
        fixture.active.data.borrow_mut().copy_from_slice(old);
        fixture.dfu.data.borrow_mut()[..ACTIVE].copy_from_slice(new);
        fixture
    }
    pub(crate) fn active(&self) -> impl ReadNorFlash {
        self.active.clone()
    }
    pub(crate) fn confirm(&self) {
        let journal = JournalState::open(self.state.clone()).unwrap();
        BlockingFirmwareState::new(journal, &mut [0])
            .mark_booted()
            .unwrap();
    }
    pub(crate) fn prepare(&self) -> Result<(), ()> {
        let journal = JournalState::open(self.state.clone()).map_err(|_| ())?;
        let mut engine = BootLoader::new(BootLoaderConfig {
            active: self.active.clone(),
            dfu: self.dfu.clone(),
            state: journal,
        });
        #[repr(align(32))]
        struct Buffer([u8; 4096]);
        engine
            .prepare_boot(&mut Buffer([0; 4096]).0)
            .map(|_| ())
            .map_err(|_| ())
    }
}

#[test]
fn real_engine_resumes_torn_page_and_journal_operations_across_all_fifteen_pages() {
    let baseline = Fixture::new();
    baseline.prepare().unwrap();
    let swap_cuts = baseline.fault.borrow().selected.clone();
    *baseline.fault.borrow_mut() = Fault::default();
    baseline.prepare().unwrap();
    let revert_cuts = baseline.fault.borrow().selected.clone();
    assert!(swap_cuts.len() > 60 && revert_cuts.len() > 60);
    for (revert, cuts) in [(false, swap_cuts), (true, revert_cuts)] {
        for cut in cuts {
            let f = Fixture::new();
            if revert {
                f.prepare().unwrap();
            }
            *f.fault.borrow_mut() = Fault {
                cut: Some(cut),
                ..Fault::default()
            };
            assert!(f.prepare().is_err(), "cut {cut}, revert {revert}");
            *f.fault.borrow_mut() = Fault::default();
            f.prepare().unwrap();
            if revert {
                assert_eq!(*f.active.data.borrow(), f.old, "revert cut {cut}");
                f.prepare().unwrap();
                assert_eq!(*f.active.data.borrow(), f.old);
            } else {
                assert_eq!(*f.active.data.borrow(), f.new, "swap cut {cut}");
                f.prepare().unwrap();
                assert_eq!(*f.active.data.borrow(), f.old);
            }
        }
    }
}
