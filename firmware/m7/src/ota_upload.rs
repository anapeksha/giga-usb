//! Sequential upload addressing and last-durable-chunk idempotency. Calling
//! `acknowledge` is the owner's assertion that erase/program/readback completed.
use crate::ota_manifest as layout;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Next(u32),
    Duplicate(u32),
}

pub struct Progress {
    offset: u32,
    total: u32,
    dashboard: u32,
    last: Option<(u32, usize, [u8; 32])>,
}
impl Progress {
    pub fn new(total: u32, dashboard: u32) -> Result<Self, &'static str> {
        let payload = total
            .checked_sub(256 + layout::ACTIVE_LENGTH)
            .ok_or("invalid upload bounds")?;
        if !(4096..=layout::DASHBOARD_LENGTH).contains(&payload)
            || !payload.is_multiple_of(4096)
            || !layout::DASHBOARD_SLOTS.contains(&dashboard)
        {
            return Err("invalid upload bounds");
        }
        Ok(Self {
            offset: 256,
            total,
            dashboard,
            last: None,
        })
    }
    pub fn offset(&self) -> u32 {
        self.offset
    }
    pub fn complete(&self) -> bool {
        self.offset == self.total
    }
    pub fn classify(&self, offset: u32, bytes: &[u8]) -> Result<Admission, &'static str> {
        if let Some((last_offset, length, hash)) = self.last
            && offset == last_offset
            && bytes.len() == length
            && <[u8; 32]>::from(Sha256::digest(bytes)) == hash
        {
            return Ok(Admission::Duplicate(self.address(offset)));
        }
        if offset != self.offset
            || bytes.is_empty()
            || bytes.len() > 4096
            || bytes.len() as u32 > self.total - self.offset
            || (bytes.len() != 4096 && self.offset + bytes.len() as u32 != self.total)
        {
            return Err("chunk offset or length is not the next complete sector");
        }
        Ok(Admission::Next(self.address(offset)))
    }
    fn address(&self, offset: u32) -> u32 {
        let payload = offset - 256;
        if payload < layout::ACTIVE_LENGTH {
            payload
        } else {
            self.dashboard + payload - layout::ACTIVE_LENGTH
        }
    }
    pub fn acknowledge(&mut self, offset: u32, bytes: &[u8]) -> Result<(), &'static str> {
        match self.classify(offset, bytes)? {
            Admission::Duplicate(_) => Ok(()),
            Admission::Next(_) => {
                self.last = Some((offset, bytes.len(), Sha256::digest(bytes).into()));
                self.offset += bytes.len() as u32;
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_is_durable_only_after_ack_and_maps_inactive_assets_exactly() {
        let total = 256 + layout::ACTIVE_LENGTH + 4096;
        let mut progress = Progress::new(total, 0xe00000).unwrap();
        let bytes = [0x55; 4096];
        assert_eq!(progress.classify(256, &bytes), Ok(Admission::Next(0)));
        assert_eq!(progress.offset(), 256); // A failed write cannot advance it.
        progress.acknowledge(256, &bytes).unwrap();
        assert_eq!(progress.classify(256, &bytes), Ok(Admission::Duplicate(0)));
        assert!(progress.classify(256, &[0x56; 4096]).is_err());
        assert!(progress.classify(0, &bytes).is_err());
        assert!(progress.classify(u32::MAX, &bytes).is_err());
        while progress.offset() < 256 + layout::ACTIVE_LENGTH {
            let offset = progress.offset();
            progress.acknowledge(offset, &bytes).unwrap();
        }
        let offset = progress.offset();
        assert_eq!(
            progress.classify(offset, &bytes),
            Ok(Admission::Next(0xe00000))
        );
        progress.acknowledge(offset, &bytes).unwrap();
        assert!(progress.complete());
        assert_eq!(
            progress.classify(offset, &bytes),
            Ok(Admission::Duplicate(0xe00000))
        );
        assert!(progress.classify(total, &bytes).is_err());
        assert!(progress.classify(256, &bytes).is_err()); // Not arbitrary replay.
    }
    #[test]
    fn malformed_geometry_and_partial_sectors_never_get_addresses() {
        assert!(Progress::new(u32::MAX, 0xe00000).is_err());
        assert!(Progress::new(256 + layout::ACTIVE_LENGTH, 0xe00000).is_err());
        assert!(Progress::new(256 + layout::ACTIVE_LENGTH + 4096, 0).is_err());
        let progress = Progress::new(256 + layout::ACTIVE_LENGTH + 4096, 0xf00000).unwrap();
        assert!(progress.classify(256, &[]).is_err());
        assert!(progress.classify(256, &[0; 256]).is_err());
        assert!(progress.classify(257, &[0; 4096]).is_err());
    }
}
