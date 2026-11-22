//! Read-only dashboard container contract. No flash partition is implicitly owned.
pub const OFFSET: u32 = 15 * 1024 * 1024;
pub const CAPACITY: usize = 1024 * 1024;
pub const HEADER_SIZE: usize = 32;
pub const MAGIC: &[u8; 8] = b"GIGAUI01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub length: usize,
    pub checksum: u32,
}

impl Header {
    pub fn etag(self) -> heapless::String<32> {
        use core::fmt::Write;
        let mut value = heapless::String::new();
        let _ = write!(value, "\"{:08x}-{:x}\"", self.checksum, self.length);
        value
    }

    pub fn matches_etag(self, value: &[u8]) -> bool {
        let etag = self.etag();
        value.split(|byte| *byte == b',').any(|part| {
            let part = part.trim_ascii();
            let part = part.strip_prefix(b"W/").unwrap_or(part);
            part == b"*" || part == etag.as_bytes()
        })
    }

    pub fn parse(bytes: &[u8; HEADER_SIZE]) -> Option<Self> {
        if &bytes[..8] != MAGIC || bytes[16..].iter().any(|byte| *byte != 0) {
            return None;
        }
        let length = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        let checksum = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        (18..=CAPACITY - HEADER_SIZE)
            .contains(&length)
            .then_some(Self { length, checksum })
    }
}

pub fn crc32(mut crc: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditional_requests_match_exact_weak_and_list_etags() {
        let header = Header {
            length: 32,
            checksum: 0xcbf4_3926,
        };
        assert_eq!(header.etag(), "\"cbf43926-20\"");
        assert!(header.matches_etag(b"\"other\", W/\"cbf43926-20\""));
        assert!(header.matches_etag(b"*"));
        assert!(!header.matches_etag(b"\"cbf43926-21\""));
    }

    #[test]
    fn checksum_vector_and_chunking() {
        assert_eq!(!crc32(!0, b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(crc32(!0, b"1234"), b"56789"), crc32(!0, b"123456789"));
    }

    #[test]
    fn rejects_missing_corrupt_and_out_of_partition_headers() {
        assert_eq!(Header::parse(&[255; HEADER_SIZE]), None);
        let mut bytes = [0; HEADER_SIZE];
        bytes[..8].copy_from_slice(MAGIC);
        for length in [0, 17, CAPACITY as u32, u32::MAX] {
            bytes[8..12].copy_from_slice(&length.to_le_bytes());
            assert_eq!(Header::parse(&bytes), None);
        }
        bytes[8..12].copy_from_slice(&18u32.to_le_bytes());
        assert_eq!(Header::parse(&bytes).unwrap().length, 18);
        bytes[31] = 1;
        assert_eq!(Header::parse(&bytes), None);
    }
}
