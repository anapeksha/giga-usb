//! AT25SF128A/AT25QF128A command engine. GPIO ownership lives in its caller.
//! Runtime owners must drive `poll_ready` cooperatively and retain this object
//! across HTTP cancellation. Only the reset-held boot path may block on WIP.
use embedded_storage::nor_flash::{NorFlashError, NorFlashErrorKind};

pub const CAPACITY: u32 = 0x1000000;
pub const SECTOR: u32 = 4096;
pub const PAGE: usize = 256;

pub trait Spi {
    fn select(&mut self, selected: bool);
    fn byte(&mut self, byte: u8) -> u8;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Identity,
    Protected,
    Busy,
    Bounds,
    Timeout,
    Verify,
    Uncertain,
}
impl NorFlashError for Error {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::Bounds => NorFlashErrorKind::OutOfBounds,
            _ => NorFlashErrorKind::Other,
        }
    }
}

pub struct Nor<S> {
    spi: S,
    pending: bool,
    fault: bool,
}

impl<S: Spi> Nor<S> {
    pub fn open(spi: S) -> Result<Self, Error> {
        let mut nor = Self {
            spi,
            pending: false,
            fault: false,
        };
        nor.spi.select(true);
        nor.spi.byte(0x9f);
        let id = [nor.spi.byte(0), nor.spi.byte(0), nor.spi.byte(0)];
        nor.spi.select(false);
        let mut sfdp = [0; 80];
        nor.command_read(0x5a, 0, &mut sfdp);
        if id != [0x1f, 0x89, 0x01]
            || &sfdp[..4] != b"SFDP"
            || sfdp[0x34..0x38] != 0x07ffffffu32.to_le_bytes()
            || sfdp[0x4c..0x50] != 0x520f200cu32.to_le_bytes()
        {
            return Err(Error::Identity);
        }
        let status = nor.status(5);
        if status & 1 != 0 {
            return Err(Error::Busy);
        }
        if status & 0x7c != 0 || nor.status(0x35) & 0xc4 != 0 {
            return Err(Error::Protected);
        }
        // MCU reset does not reset the NOR's WEL latch. A reset between WREN
        // and program/erase must not permanently strand an otherwise good boot.
        // WRDI (also used by the validated installer) changes no flash contents.
        if status & 2 != 0 {
            nor.spi.select(true);
            nor.spi.byte(4);
            nor.spi.select(false);
            if nor.status(5) & 0x7f != 0 {
                return Err(Error::Uncertain);
            }
        }
        Ok(nor)
    }

    fn address(&mut self, address: u32) {
        self.spi.byte((address >> 16) as u8);
        self.spi.byte((address >> 8) as u8);
        self.spi.byte(address as u8);
    }
    fn status(&mut self, command: u8) -> u8 {
        self.spi.select(true);
        self.spi.byte(command);
        let value = self.spi.byte(0);
        self.spi.select(false);
        value
    }
    fn command_read(&mut self, command: u8, address: u32, bytes: &mut [u8]) {
        self.spi.select(true);
        self.spi.byte(command);
        self.address(address);
        if command == 0x5a {
            self.spi.byte(0);
        }
        for byte in bytes {
            *byte = self.spi.byte(0);
        }
        self.spi.select(false);
    }
    fn bounds(address: u32, length: usize) -> Result<(), Error> {
        if address > CAPACITY || length > (CAPACITY - address) as usize {
            Err(Error::Bounds)
        } else {
            Ok(())
        }
    }
    fn available(&self) -> Result<(), Error> {
        if self.fault {
            Err(Error::Uncertain)
        } else if self.pending {
            Err(Error::Busy)
        } else {
            Ok(())
        }
    }
    pub fn read(&mut self, address: u32, bytes: &mut [u8]) -> Result<(), Error> {
        self.available()?;
        Self::bounds(address, bytes.len())?;
        // Bounded transactions also give the cooperative runtime owner yield points.
        for (index, chunk) in bytes.chunks_mut(16).enumerate() {
            self.command_read(3, address + (16 * index) as u32, chunk);
        }
        Ok(())
    }
    fn enable(&mut self) -> Result<(), Error> {
        self.available()?;
        if self.status(5) & 0x7f != 0 || self.status(0x35) & 0xc4 != 0 {
            return Err(Error::Protected);
        }
        self.spi.select(true);
        self.spi.byte(6);
        self.spi.select(false);
        if self.status(5) & 0x7f != 2 {
            return Err(Error::Protected);
        }
        Ok(())
    }
    pub fn start_erase(&mut self, address: u32) -> Result<(), Error> {
        Self::bounds(address, SECTOR as usize)?;
        if !address.is_multiple_of(SECTOR) {
            return Err(Error::Bounds);
        }
        self.enable()?;
        self.pending = true;
        self.spi.select(true);
        self.spi.byte(0x20);
        self.address(address);
        self.spi.select(false);
        Ok(())
    }
    pub fn start_program(&mut self, address: u32, bytes: &[u8]) -> Result<(), Error> {
        Self::bounds(address, bytes.len())?;
        if bytes.is_empty() || bytes.len() > PAGE - address as usize % PAGE {
            return Err(Error::Bounds);
        }
        self.enable()?;
        self.pending = true;
        self.spi.select(true);
        self.spi.byte(2);
        self.address(address);
        for byte in bytes {
            self.spi.byte(*byte);
        }
        self.spi.select(false);
        Ok(())
    }
    pub fn poll_ready(&mut self) -> Result<bool, Error> {
        if self.fault {
            return Err(Error::Uncertain);
        }
        let status = self.status(5);
        if status & 1 != 0 {
            return Ok(false);
        }
        if status & 2 != 0 {
            self.fault = true;
            return Err(Error::Uncertain);
        }
        self.pending = false;
        Ok(true)
    }
    /// Terminal latch: do not issue another write after an uncertain deadline.
    pub fn timeout(&mut self) -> Error {
        self.fault = true;
        Error::Timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{vec, vec::Vec};
    struct Bus {
        transaction: Vec<u8>,
        commands: Vec<Vec<u8>>,
        id: [u8; 3],
        sfdp: [u8; 80],
        wel: bool,
        busy: bool,
        protected: bool,
    }
    impl Bus {
        fn new() -> Self {
            let mut sfdp = [0; 80];
            sfdp[..4].copy_from_slice(b"SFDP");
            sfdp[0x34..0x38].copy_from_slice(&0x07ffffffu32.to_le_bytes());
            sfdp[0x4c..0x50].copy_from_slice(&0x520f200cu32.to_le_bytes());
            Self {
                transaction: vec![],
                commands: vec![],
                id: [0x1f, 0x89, 1],
                sfdp,
                wel: false,
                busy: false,
                protected: false,
            }
        }
    }
    impl Spi for Bus {
        fn select(&mut self, selected: bool) {
            if selected {
                assert!(self.transaction.is_empty());
            } else {
                match self.transaction[0] {
                    6 => self.wel = true,
                    4 => self.wel = false,
                    2 | 0x20 => {
                        assert!(self.wel);
                        self.wel = false;
                        self.busy = true;
                    }
                    _ => {}
                }
                self.commands.push(core::mem::take(&mut self.transaction));
            }
        }
        fn byte(&mut self, byte: u8) -> u8 {
            self.transaction.push(byte);
            let index = self.transaction.len() - 1;
            match (self.transaction[0], index) {
                (0x9f, 1..=3) => self.id[index - 1],
                (0x5a, 5..=84) => self.sfdp[index - 5],
                (5, 1) => {
                    u8::from(self.busy)
                        | (u8::from(self.wel) << 1)
                        | (u8::from(self.protected) << 2)
                }
                _ => 0,
            }
        }
    }
    #[test]
    fn fixed_identity_and_sfdp_gate_precedes_any_wren() {
        for field in 0..3 {
            let mut bus = Bus::new();
            match field {
                0 => bus.id[0] ^= 1,
                1 => bus.sfdp[0x34] ^= 1,
                _ => bus.sfdp[0x4c] ^= 1,
            }
            assert!(matches!(Nor::open(bus), Err(Error::Identity)));
        }
        let mut bus = Bus::new();
        bus.protected = true;
        assert!(matches!(Nor::open(bus), Err(Error::Protected)));
    }
    #[test]
    fn mcu_reset_after_wren_recovers_with_non_destructive_wrdi() {
        let mut bus = Bus::new();
        bus.wel = true;
        let nor = Nor::open(bus).unwrap();
        assert!(!nor.spi.wel);
        assert!(nor.spi.commands.iter().any(|command| command == &[4]));
        assert!(
            !nor.spi
                .commands
                .iter()
                .any(|command| matches!(command[0], 2 | 6 | 0x20))
        );
    }
    #[test]
    fn pending_operations_are_retained_and_timeout_is_terminal() {
        let mut nor = Nor::open(Bus::new()).unwrap();
        nor.start_erase(0x200000).unwrap();
        assert_eq!(nor.spi.commands.last().unwrap(), &[0x20, 0x20, 0, 0]);
        assert_eq!(nor.read(0, &mut [0; 1]), Err(Error::Busy));
        assert_eq!(nor.start_program(0, &[0]), Err(Error::Busy));
        assert!(!nor.poll_ready().unwrap());
        assert_eq!(nor.timeout(), Error::Timeout);
        nor.spi.busy = false;
        assert_eq!(nor.poll_ready(), Err(Error::Uncertain));
        assert_eq!(nor.start_erase(0), Err(Error::Uncertain));
    }
    #[test]
    fn page_boundary_bounds_and_exact_program_command() {
        let mut nor = Nor::open(Bus::new()).unwrap();
        let commands = nor.spi.commands.len();
        assert_eq!(nor.start_program(255, &[1, 2]), Err(Error::Bounds));
        assert_eq!(nor.start_program(CAPACITY, &[0]), Err(Error::Bounds));
        assert_eq!(nor.start_erase(1), Err(Error::Bounds));
        assert_eq!(nor.spi.commands.len(), commands);
        nor.start_program(0xe00000, &[0xaa, 0x55]).unwrap();
        assert_eq!(
            nor.spi.commands.last().unwrap(),
            &[2, 0xe0, 0, 0, 0xaa, 0x55]
        );
        nor.spi.busy = false;
        assert!(nor.poll_ready().unwrap());
        nor.start_erase(0xe00000).unwrap();
    }
}
