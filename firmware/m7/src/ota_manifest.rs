//! OTA v1 wire contract. All integers are little endian; no archive is accepted.
//!
//! File = 256-byte manifest || 0x1e0000-byte padded ACTIVE || dashboard container.
//! Signature = Ed25519(SHA256(DOMAIN || manifest[0..192])). The dashboard hash
//! covers exactly dashboard_length bytes, including its GIGAUI01 header and FF
//! sector padding. Its placement in A or B is deliberately not signed.
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

pub const BOOT_ORIGIN: u32 = 0x0800_0000;
pub const BOOT_LENGTH: u32 = 0x20000;
pub const ACTIVE_ORIGIN: u32 = 0x0802_0000;
pub const ACTIVE_LENGTH: u32 = 0x1e0000;
pub const M7_LENGTH: u32 = 0xe0000;
pub const M4_ORIGIN: u32 = 0x0810_0000;
pub const ACTIVE_END: u32 = 0x0820_0000;
pub const DFU_ORIGIN: u32 = 0;
pub const DFU_LENGTH: u32 = 0x200000;
pub const STATE_ORIGIN: u32 = 0x200000;
pub const STATE_LENGTH: u32 = 0x20000;
pub const STATE_JOURNAL_LENGTH: u32 = 8192;
pub const ALLOCATION_ORIGIN: u32 = 0x202000;
/// Installer-only allocation consent marker: magic, layout u32, zero bytes
/// 12..28, then CRC32/ISO-HDLC over 0..28. Runtime never creates this record.
pub fn allocation_record() -> [u8; 32] {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(b"GIGAAL01");
    bytes[8..12].copy_from_slice(&LAYOUT_ID.to_le_bytes());
    let crc = !crate::dashboard::crc32(!0, &bytes[..28]);
    bytes[28..].copy_from_slice(&crc.to_le_bytes());
    bytes
}
pub const METADATA_SLOTS: [u32; 2] = [0x220000, 0x230000];
pub const METADATA_SLOT_LENGTH: u32 = 0x10000;
pub const DASHBOARD_SLOTS: [u32; 2] = [0xf00000, 0xe00000];
pub const DASHBOARD_LENGTH: u32 = 0x100000;
pub const MANIFEST_LENGTH: usize = 256;
pub const SIGNED_LENGTH: usize = 192;
pub const BUILD_ID_OFFSET: usize = 120;
pub const BUILD_ID_LENGTH: usize = 48;
pub const RESERVED_OFFSET: usize = BUILD_ID_OFFSET + BUILD_ID_LENGTH;
pub const LAYOUT_ID: u32 = 1;
pub const PROTOCOL: u32 = 10;
pub const DOMAIN: &[u8] = b"GigaUSB OTA manifest v1\0";
// Cargo resolves OUT_DIR at compile time; generated constants are embedded in
// both binaries. Production firmware needs no environment or generated files.
// Without an override, build.rs pins the original RC3 public key.
include!(concat!(env!("OUT_DIR"), "/ota_build.rs"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Format,
    Layout,
    Vectors,
    Signature,
    Hash,
    Dashboard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    bytes: [u8; MANIFEST_LENGTH],
}

fn word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn parse_build_id(bytes: &[u8; MANIFEST_LENGTH]) -> Result<&str, Error> {
    let field = &bytes[BUILD_ID_OFFSET..RESERVED_OFFSET];
    let length = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(BUILD_ID_LENGTH);
    if length == 0
        || !field[..length]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
        || field[length..].iter().any(|byte| *byte != 0)
    {
        return Err(Error::Format);
    }
    core::str::from_utf8(&field[..length]).map_err(|_| Error::Format)
}

impl Manifest {
    /// Byte offsets: magic 0, version 8, header size 12, layout 16, protocol 20,
    /// ACTIVE base 24/length 28, M4 base 32, dashboard length 36, ACTIVE SHA256
    /// 40, dashboard SHA256 72, M7 MSP/reset 104/108, M4 MSP/reset 112/116,
    /// canonical zero-padded ASCII build ID 120..168 (1..=48 [A-Za-z0-9._-]),
    /// zero reserved 168..192, signature 192..256.
    pub fn verify(bytes: &[u8; MANIFEST_LENGTH], key: &[u8; 32]) -> Result<Self, Error> {
        if &bytes[..8] != b"GIGAOTA1"
            || word(bytes, 8) != 1
            || word(bytes, 12) != MANIFEST_LENGTH as u32
            || bytes[RESERVED_OFFSET..SIGNED_LENGTH]
                .iter()
                .any(|v| *v != 0)
        {
            return Err(Error::Format);
        }
        parse_build_id(bytes)?;
        if word(bytes, 16) != LAYOUT_ID
            || word(bytes, 20) != PROTOCOL
            || word(bytes, 24) != ACTIVE_ORIGIN
            || word(bytes, 28) != ACTIVE_LENGTH
            || word(bytes, 32) != M4_ORIGIN
            || !(4096..=DASHBOARD_LENGTH).contains(&word(bytes, 36))
            || !word(bytes, 36).is_multiple_of(4096)
        {
            return Err(Error::Layout);
        }
        if word(bytes, 104) != 0x2408_0000
            || word(bytes, 112) != 0x3002_0000
            || !valid_reset(word(bytes, 108), ACTIVE_ORIGIN, M4_ORIGIN)
            || !valid_reset(word(bytes, 116), M4_ORIGIN, ACTIVE_END)
        {
            return Err(Error::Vectors);
        }
        let digest = Self::signing_digest(bytes);
        VerifyingKey::from_bytes(key)
            .map_err(|_| Error::Signature)?
            .verify_strict(
                &digest,
                &Signature::from_bytes(bytes[SIGNED_LENGTH..].try_into().unwrap()),
            )
            .map_err(|_| Error::Signature)?;
        Ok(Self { bytes: *bytes })
    }

    pub fn signing_digest(bytes: &[u8; MANIFEST_LENGTH]) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(&bytes[..SIGNED_LENGTH]);
        hash.finalize().into()
    }

    pub fn build_id(&self) -> &str {
        parse_build_id(&self.bytes).expect("verified canonical build ID")
    }

    /// Application health gate only; persistent boot must accept future IDs.
    pub fn matches_running_build(&self) -> bool {
        self.build_id() == BUILD_ID
    }

    pub fn bytes(&self) -> &[u8; MANIFEST_LENGTH] {
        &self.bytes
    }
    pub fn active_hash(&self) -> &[u8] {
        &self.bytes[40..72]
    }
    pub fn dashboard_hash(&self) -> &[u8] {
        &self.bytes[72..104]
    }
    pub fn dashboard_length(&self) -> u32 {
        word(&self.bytes, 36)
    }
    pub fn file_length(&self) -> u32 {
        MANIFEST_LENGTH as u32 + ACTIVE_LENGTH + self.dashboard_length()
    }

    pub fn check_vectors(&self, m7: &[u8; 8], m4: &[u8; 8]) -> Result<(), Error> {
        if m7[..] != self.bytes[104..112] || m4[..] != self.bytes[112..120] {
            return Err(Error::Vectors);
        }
        Ok(())
    }
}

pub fn valid_reset(reset: u32, start: u32, end: u32) -> bool {
    reset & 1 == 1 && (start..end).contains(&(reset & !1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed() -> ([u8; MANIFEST_LENGTH], [u8; 32]) {
        let key = SigningKey::from_bytes(&[0x53; 32]);
        let mut bytes = [0; MANIFEST_LENGTH];
        bytes[..8].copy_from_slice(b"GIGAOTA1");
        for (offset, value) in [
            (8, 1),
            (12, 256),
            (16, LAYOUT_ID),
            (20, PROTOCOL),
            (24, ACTIVE_ORIGIN),
            (28, ACTIVE_LENGTH),
            (32, M4_ORIGIN),
            (36, 4096),
            (104, 0x24080000),
            (108, ACTIVE_ORIGIN + 0x401),
            (112, 0x30020000),
            (116, M4_ORIGIN + 0x401),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[BUILD_ID_OFFSET..BUILD_ID_OFFSET + 9].copy_from_slice(b"fixture-a");
        let signature = key.sign(&Manifest::signing_digest(&bytes));
        bytes[SIGNED_LENGTH..].copy_from_slice(&signature.to_bytes());
        (bytes, key.verifying_key().to_bytes())
    }

    #[test]
    fn signature_and_exact_layout() {
        let (bytes, key) = signed();
        let manifest = Manifest::verify(&bytes, &key).unwrap();
        assert_eq!(manifest.build_id(), "fixture-a");
        assert_ne!(manifest.build_id(), "fixture-b");
        assert_eq!(manifest.file_length(), 256 + ACTIVE_LENGTH + 4096);
        assert_eq!(ACTIVE_LENGTH / 0x20000, 15);
        assert_eq!(M4_ORIGIN - ACTIVE_ORIGIN, M7_LENGTH);
        assert_eq!(DFU_LENGTH - ACTIVE_LENGTH, BOOT_LENGTH);
        assert!(Manifest::verify(&bytes, &[0; 32]).is_err());
        for index in 0..MANIFEST_LENGTH {
            let mut corrupt = bytes;
            corrupt[index] ^= 1;
            assert!(Manifest::verify(&corrupt, &key).is_err(), "byte {index}");
        }
    }

    #[test]
    fn signed_build_ids_are_canonical_and_application_health_is_exact() {
        let (template, public) = signed();
        let key = SigningKey::from_bytes(&[0x53; 32]);
        let with_id = |label: &[u8]| {
            let mut bytes = template;
            bytes[BUILD_ID_OFFSET..RESERVED_OFFSET].fill(0);
            bytes[BUILD_ID_OFFSET..BUILD_ID_OFFSET + label.len()].copy_from_slice(label);
            let signature = key.sign(&Manifest::signing_digest(&bytes));
            bytes[SIGNED_LENGTH..].copy_from_slice(&signature.to_bytes());
            bytes
        };
        for label in [
            &b""[..],
            b"with space",
            b"slash/",
            b"a\0b",
            b"\xff",
            b"line\n",
        ] {
            assert_eq!(
                Manifest::verify(&with_id(label), &public),
                Err(Error::Format)
            );
        }
        for label in [
            b"fixture-a".as_slice(),
            &[b'a'; BUILD_ID_LENGTH],
            BUILD_ID.as_bytes(),
        ] {
            let manifest = Manifest::verify(&with_id(label), &public).unwrap();
            assert_eq!(manifest.build_id().as_bytes(), label);
            assert_eq!(
                manifest.matches_running_build(),
                label == BUILD_ID.as_bytes()
            );
        }
        let mut reserved = with_id(b"fixture-a");
        reserved[RESERVED_OFFSET] = 1;
        let signature = key.sign(&Manifest::signing_digest(&reserved));
        reserved[SIGNED_LENGTH..].copy_from_slice(&signature.to_bytes());
        assert_eq!(Manifest::verify(&reserved, &public), Err(Error::Format));
    }

    #[test]
    fn reset_vectors_are_core_specific_and_thumb() {
        assert!(!valid_reset(ACTIVE_ORIGIN, ACTIVE_ORIGIN, M4_ORIGIN));
        assert!(!valid_reset(M4_ORIGIN + 1, ACTIVE_ORIGIN, M4_ORIGIN));
        assert!(!valid_reset(ACTIVE_END + 1, M4_ORIGIN, ACTIVE_END));
        assert!(valid_reset(M4_ORIGIN + 1, M4_ORIGIN, ACTIVE_END));
    }
}
