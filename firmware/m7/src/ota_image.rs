//! Streaming authentication shared by boot and the runtime staging coordinator.
use crate::{
    dashboard::{HEADER_SIZE, Header, crc32},
    ota_manifest::{self as layout, Error, Manifest},
};
use embedded_storage::nor_flash::ReadNorFlash;
use sha2::{Digest, Sha256};

pub struct Selected {
    pub metadata_slot: u32,
    pub dashboard_slot: u32,
    pub manifest: Manifest,
}

pub fn hash<F: ReadNorFlash>(flash: &mut F, offset: u32, length: u32) -> Result<[u8; 32], Error> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 256];
    for index in (0..length).step_by(buffer.len()) {
        let size = buffer.len().min((length - index) as usize);
        flash
            .read(offset + index, &mut buffer[..size])
            .map_err(|_| Error::Hash)?;
        digest.update(&buffer[..size]);
    }
    Ok(digest.finalize().into())
}

pub fn dashboard<F: ReadNorFlash>(
    flash: &mut F,
    slot: u32,
    manifest: &Manifest,
) -> Result<(), Error> {
    if !layout::DASHBOARD_SLOTS.contains(&slot) {
        return Err(Error::Layout);
    }
    let mut bytes = [0; HEADER_SIZE];
    flash.read(slot, &mut bytes).map_err(|_| Error::Dashboard)?;
    let header = Header::parse(&bytes).ok_or(Error::Dashboard)?;
    if (HEADER_SIZE + header.length).next_multiple_of(4096) != manifest.dashboard_length() as usize
    {
        return Err(Error::Dashboard);
    }
    let mut digest = Sha256::new();
    digest.update(bytes);
    let mut buffer = [0; 256];
    let mut crc = !0;
    let payload_end = HEADER_SIZE + header.length;
    for offset in (HEADER_SIZE..manifest.dashboard_length() as usize).step_by(buffer.len()) {
        let size = buffer
            .len()
            .min(manifest.dashboard_length() as usize - offset);
        flash
            .read(slot + offset as u32, &mut buffer[..size])
            .map_err(|_| Error::Dashboard)?;
        if offset == HEADER_SIZE && buffer[..3] != [0x1f, 0x8b, 8] {
            return Err(Error::Dashboard);
        }
        let payload = payload_end.saturating_sub(offset).min(size);
        crc = crc32(crc, &buffer[..payload]);
        if buffer[payload..size].iter().any(|b| *b != 0xff) {
            return Err(Error::Dashboard);
        }
        digest.update(&buffer[..size]);
    }
    if !crc != header.checksum || digest.finalize().as_slice() != manifest.dashboard_hash() {
        return Err(Error::Dashboard);
    }
    Ok(())
}

/// Choose by the complete ACTIVE hash, never metadata recency. Old metadata and
/// old dashboard remain valid during trial and after a combined-core rollback.
pub fn select<A: ReadNorFlash, E: ReadNorFlash>(
    active: &mut A,
    external: &mut E,
) -> Result<Selected, Error> {
    select_with_key(active, external, &layout::TRUSTED_KEY)
}

fn select_with_key<A: ReadNorFlash, E: ReadNorFlash>(
    active: &mut A,
    external: &mut E,
    key: &[u8; 32],
) -> Result<Selected, Error> {
    let active_hash = hash(active, 0, layout::ACTIVE_LENGTH)?;
    let mut m7 = [0; 8];
    let mut m4 = [0; 8];
    active.read(0, &mut m7).map_err(|_| Error::Vectors)?;
    active
        .read(layout::M4_ORIGIN - layout::ACTIVE_ORIGIN, &mut m4)
        .map_err(|_| Error::Vectors)?;
    for metadata_slot in layout::METADATA_SLOTS {
        let mut bytes = [0; layout::MANIFEST_LENGTH];
        external
            .read(metadata_slot, &mut bytes)
            .map_err(|_| Error::Hash)?;
        let Ok(manifest) = Manifest::verify(&bytes, key) else {
            continue;
        };
        if manifest.active_hash() != active_hash {
            continue;
        }
        manifest.check_vectors(&m7, &m4)?;
        for dashboard_slot in layout::DASHBOARD_SLOTS {
            if dashboard(external, dashboard_slot, &manifest).is_ok() {
                return Ok(Selected {
                    metadata_slot,
                    dashboard_slot,
                    manifest,
                });
            }
        }
    }
    Err(Error::Hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use embedded_storage::nor_flash::ErrorType;
    use std::{vec, vec::Vec};

    struct Memory(Vec<u8>);
    impl ErrorType for Memory {
        type Error = crate::ota_nor::Error;
    }
    impl ReadNorFlash for Memory {
        const READ_SIZE: usize = 1;
        fn capacity(&self) -> usize {
            self.0.len()
        }
        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let source = self
                .0
                .get(offset as usize..offset as usize + bytes.len())
                .ok_or(crate::ota_nor::Error::Bounds)?;
            bytes.copy_from_slice(source);
            Ok(())
        }
    }
    fn image(value: u8) -> Memory {
        let mut bytes = vec![value; layout::ACTIVE_LENGTH as usize];
        bytes[..4].copy_from_slice(&0x24080000u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&(layout::ACTIVE_ORIGIN + 0x401).to_le_bytes());
        let m4 = layout::M7_LENGTH as usize;
        bytes[m4..m4 + 4].copy_from_slice(&0x30020000u32.to_le_bytes());
        bytes[m4 + 4..m4 + 8].copy_from_slice(&(layout::M4_ORIGIN + 0x401).to_le_bytes());
        Memory(bytes)
    }
    fn container(value: u8) -> [u8; 4096] {
        let mut bytes = [0xff; 4096];
        bytes[..32].fill(0);
        bytes[..8].copy_from_slice(b"GIGAUI01");
        bytes[8..12].copy_from_slice(&18u32.to_le_bytes());
        bytes[32..50].fill(value);
        bytes[32..35].copy_from_slice(&[0x1f, 0x8b, 8]);
        let crc = !crc32(!0, &bytes[32..50]);
        bytes[12..16].copy_from_slice(&crc.to_le_bytes());
        bytes
    }
    fn manifest(image: &Memory, dashboard: &[u8], key: &SigningKey) -> [u8; 256] {
        let mut bytes = [0; 256];
        bytes[..8].copy_from_slice(b"GIGAOTA1");
        for (offset, word) in [
            (8, 1u32),
            (12, 256),
            (16, layout::LAYOUT_ID),
            (20, layout::PROTOCOL),
            (24, layout::ACTIVE_ORIGIN),
            (28, layout::ACTIVE_LENGTH),
            (32, layout::M4_ORIGIN),
            (36, dashboard.len() as u32),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
        bytes[40..72].copy_from_slice(&Sha256::digest(&image.0));
        bytes[72..104].copy_from_slice(&Sha256::digest(dashboard));
        bytes[104..112].copy_from_slice(&image.0[..8]);
        bytes[112..120]
            .copy_from_slice(&image.0[layout::M7_LENGTH as usize..layout::M7_LENGTH as usize + 8]);
        bytes[layout::BUILD_ID_OFFSET..layout::BUILD_ID_OFFSET + 9].copy_from_slice(b"fixture-a");
        let signature = key.sign(&Manifest::signing_digest(&bytes));
        bytes[192..].copy_from_slice(&signature.to_bytes());
        bytes
    }
    #[test]
    fn actual_engine_trial_rollback_and_confirmation_keep_signed_core_asset_pairs() {
        let key = SigningKey::from_bytes(&[0x28; 32]);
        let old = image(0x11);
        let new = image(0x22);
        let mut external = Memory(vec![0xff; 0x1000000]);
        for (active, value, metadata, slot) in [
            (&old, 1, 0x220000usize, 0xf00000usize),
            (&new, 2, 0x230000, 0xe00000),
        ] {
            let dashboard = container(value);
            external.0[metadata..metadata + 256]
                .copy_from_slice(&manifest(active, &dashboard, &key));
            external.0[slot..slot + 4096].copy_from_slice(&dashboard);
        }
        let public = key.verifying_key().to_bytes();
        for confirm in [false, true] {
            let fixture = crate::ota_swap_tests::Fixture::stage(&old.0, &new.0);
            fixture.prepare().unwrap();
            assert_eq!(
                select_with_key(&mut fixture.active(), &mut external, &public)
                    .unwrap()
                    .dashboard_slot,
                0xe00000
            );
            if confirm {
                fixture.confirm();
            }
            fixture.prepare().unwrap();
            let selected = select_with_key(&mut fixture.active(), &mut external, &public).unwrap();
            assert_eq!(
                selected.dashboard_slot,
                if confirm { 0xe00000 } else { 0xf00000 }
            );
            assert_eq!(
                selected.metadata_slot,
                if confirm { 0x230000 } else { 0x220000 }
            );
        }
    }

    #[test]
    fn active_hash_selects_matching_old_or_new_dashboard_not_latest_metadata() {
        let key = SigningKey::from_bytes(&[0x28; 32]);
        let mut old = image(0x11);
        let mut new = image(0x22);
        let mut external = Memory(vec![0xff; 0x1000000]);
        for (active, value, metadata, slot) in [
            (&old, 1, 0x220000usize, 0xf00000usize),
            (&new, 2, 0x230000, 0xe00000),
        ] {
            let dashboard = container(value);
            let manifest = manifest(active, &dashboard, &key);
            external.0[metadata..metadata + 256].copy_from_slice(&manifest);
            external.0[slot..slot + 4096].copy_from_slice(&dashboard);
        }
        let public = key.verifying_key().to_bytes();
        let selected = select_with_key(&mut new, &mut external, &public).unwrap();
        assert_eq!(selected.metadata_slot, 0x230000);
        assert_eq!(selected.dashboard_slot, 0xe00000);
        let selected = select_with_key(&mut old, &mut external, &public).unwrap();
        assert_eq!(selected.metadata_slot, 0x220000);
        assert_eq!(selected.dashboard_slot, 0xf00000);
        external.0[0xf00000 + 50] = 0; // Signed FF padding is authenticated too.
        assert!(select_with_key(&mut old, &mut external, &public).is_err());
        new.0[0x100001] ^= 1; // M4 bytes are covered by the same combined hash.
        assert!(select_with_key(&mut new, &mut external, &public).is_err());
    }
}
