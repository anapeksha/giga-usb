//! Translate a synchronous Embassy state transition into ordered NOR mutations.
//! Planning changes RAM only. The dedicated runtime owner must execute each
//! mutation, with readback, in order and never publish success before completion.
use crate::{
    ota_nor::Error,
    ota_state::{JournalState, PHYSICAL_SIZE},
};
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};

pub enum Mutation {
    Erase(u32),
    Write {
        offset: u32,
        length: usize,
        bytes: [u8; 128],
    },
}

pub struct Plan<'a> {
    pub bytes: &'a mut [u8; PHYSICAL_SIZE],
    pub mutations: heapless::Vec<Mutation, 8>,
}
impl<'a> Plan<'a> {
    pub fn new(bytes: &'a mut [u8; PHYSICAL_SIZE]) -> Self {
        Self {
            bytes,
            mutations: heapless::Vec::new(),
        }
    }
    pub fn transition(self, confirmed: bool) -> Result<Self, Error> {
        let mut journal = JournalState::open(self).map_err(|_| Error::Verify)?;
        let mut word = [0];
        {
            let mut state = embassy_boot::BlockingFirmwareState::new(&mut journal, &mut word);
            if confirmed {
                state.mark_booted()
            } else {
                state.mark_updated()
            }
            .map_err(|_| Error::Verify)?;
        }
        Ok(journal.into_inner())
    }
    pub fn boot_state(self) -> Result<embassy_boot::State, Error> {
        let journal = JournalState::open(self).map_err(|_| Error::Verify)?;
        let mut word = [0];
        embassy_boot::BlockingFirmwareState::new(journal, &mut word)
            .get_state()
            .map_err(|_| Error::Verify)
    }
}
impl ErrorType for Plan<'_> {
    type Error = Error;
}
impl ReadNorFlash for Plan<'_> {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        PHYSICAL_SIZE
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Error> {
        let from = offset as usize;
        let source = self
            .bytes
            .get(from..from + bytes.len())
            .ok_or(Error::Bounds)?;
        bytes.copy_from_slice(source);
        Ok(())
    }
}
impl NorFlash for Plan<'_> {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > 128 {
            return Err(Error::Bounds);
        }
        let from = offset as usize;
        let target = self
            .bytes
            .get_mut(from..from + bytes.len())
            .ok_or(Error::Bounds)?;
        if target.iter().zip(bytes).any(|(a, b)| a & b != *b) {
            return Err(Error::Verify);
        }
        let mut record = [0; 128];
        record[..bytes.len()].copy_from_slice(bytes);
        self.mutations
            .push(Mutation::Write {
                offset,
                length: bytes.len(),
                bytes: record,
            })
            .map_err(|_| Error::Bounds)?;
        target.copy_from_slice(bytes);
        Ok(())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Error> {
        if !from.is_multiple_of(4096) || to != from + 4096 {
            return Err(Error::Bounds);
        }
        let target = self
            .bytes
            .get_mut(from as usize..to as usize)
            .ok_or(Error::Bounds)?;
        self.mutations
            .push(Mutation::Erase(from))
            .map_err(|_| Error::Bounds)?;
        target.fill(0xff);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plan_never_needs_full_sector_payload_or_reorders_commit() {
        let mut bytes = [0xff; PHYSICAL_SIZE];
        bytes[..128].copy_from_slice(&crate::ota_state::seed_bytes());
        {
            let plan = Plan::new(&mut bytes).transition(false).unwrap();
            assert!(!plan.mutations.is_empty());
            assert!(plan.mutations.len() <= 8);
        }
        assert_eq!(
            Plan::new(&mut bytes).boot_state().unwrap(),
            embassy_boot::State::Swap
        );
        {
            let plan = Plan::new(&mut bytes).transition(true).unwrap();
            assert!(
                matches!(plan.mutations.last(), Some(Mutation::Write { length: 1, bytes, .. }) if bytes[0] == 0)
            );
        }
        assert_eq!(
            Plan::new(&mut bytes).boot_state().unwrap(),
            embassy_boot::State::Boot
        );
    }
}
