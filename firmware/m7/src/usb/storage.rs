#![forbid(unsafe_code)]
//! Read-only BOT/SCSI wire validation and bounded FAT32 parsing.
//!
//! Only 512-byte sectors, MBR FAT32 partitions or a superfloppy, root directory
//! short-name aliases, and files <=64 KiB are supported. LFN records are skipped;
//! directory entries are listed but cannot be opened. Each chain traversal is
//! capped at 128 distinct clusters; no malformed medium can cause an endless walk.

use heapless::{String, Vec};

pub const SECTOR_SIZE: usize = 512;
pub const MAX_FILES: usize = 32;
pub const MAX_FILE_SIZE: u32 = 65536;

/// Protocol/format rejection or a bounded-reader limit, distinct from device I/O errors.
/// `Limit` means a resource budget was exceeded, not necessarily a corrupt medium.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidBot,
    CommandFailed,
    InvalidCapacity,
    InvalidPartition,
    InvalidBpb,
    InvalidChain,
    Limit,
    InvalidFile,
}

fn le16(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes([b[p], b[p + 1]])
}
fn le32(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes([b[p], b[p + 1], b[p + 2], b[p + 3]])
}

/// The only accepted commands are non-mutating SCSI commands for LUN zero.
pub fn cbw(tag: u32, cdb: &[u8], length: u32) -> Result<[u8; 31], Error> {
    let expected = match cdb {
        [0, 0, 0, 0, 0, 0] => (6, 0),
        [3, 0, 0, 0, 18, 0] => (6, 18),
        [0x12, 0, 0, 0, 36, 0] => (6, 36),
        [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0] => (10, 8),
        [0x28, 0, _, _, _, _, 0, 0, 1, 0] => (10, 512),
        _ => return Err(Error::InvalidBot),
    };
    if cdb.len() != expected.0 || length != expected.1 {
        return Err(Error::InvalidBot);
    }
    let mut bytes = [0; 31];
    bytes[..4].copy_from_slice(b"USBC");
    bytes[4..8].copy_from_slice(&tag.to_le_bytes());
    bytes[8..12].copy_from_slice(&length.to_le_bytes());
    bytes[12] = if length == 0 { 0 } else { 0x80 };
    bytes[14] = cdb.len() as u8;
    bytes[15..15 + cdb.len()].copy_from_slice(cdb);
    Ok(bytes)
}

/// A data-IN STALL ends the data stage, not the BOT command. Preserve the
/// acknowledged byte count for CSW validation and clear the halt before CSW.
pub(super) fn data_in_completion(
    result: Result<usize, (super::types::UsbError, usize)>,
) -> Result<(usize, bool), super::types::UsbError> {
    match result {
        Ok(count) => Ok((count, false)),
        Err((super::types::UsbError::Stall, count)) => Ok((count, true)),
        Err((error, _)) => Err(error),
    }
}

/// Validate exact CSW size/signature/tag, legal status, and processed-data bounds.
pub fn csw(bytes: &[u8], tag: u32, requested: u32, received: u32) -> Result<(), Error> {
    if bytes.len() != 13
        || &bytes[..4] != b"USBS"
        || le32(bytes, 4) != tag
        || received > requested
        // Residue counts unprocessed bytes, not necessarily bytes absent from USB.
        || le32(bytes, 8) > requested
        || le32(bytes, 8) < requested - received
        || bytes[12] > 1
    {
        return Err(Error::InvalidBot);
    }
    if bytes[12] == 1 {
        Err(Error::CommandFailed)
    } else {
        Ok(())
    }
}

/// Decode READ CAPACITY(10) as (block count, block size); require 512-byte blocks.
pub fn capacity(bytes: &[u8]) -> Result<(u32, u32), Error> {
    if bytes.len() != 8 {
        return Err(Error::InvalidCapacity);
    }
    let last = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let size = u32::from_be_bytes(bytes[4..].try_into().unwrap());
    if last == u32::MAX || size != 512 {
        return Err(Error::InvalidCapacity);
    }
    Ok((last + 1, size))
}

/// Decode the sense key and additional sense codes, without interpreting vendor data.
pub fn sense(bytes: &[u8]) -> Result<(u8, u8, u8), Error> {
    match bytes.first().map(|b| b & 0x7f) {
        Some(0x70 | 0x71) if bytes.len() >= 14 && bytes[7] >= 6 => {
            Ok((bytes[2] & 15, bytes[12], bytes[13]))
        }
        Some(0x72 | 0x73) if bytes.len() >= 8 => Ok((bytes[1] & 15, bytes[2], bytes[3])),
        _ => Err(Error::InvalidBot),
    }
}

pub fn ascii<const N: usize>(bytes: &[u8]) -> String<N> {
    let mut text = String::new();
    for &byte in bytes.iter().take(N) {
        let _ = text.push(if (32..=126).contains(&byte) {
            char::from(byte)
        } else {
            '?'
        });
    }
    while text.ends_with(' ') {
        text.pop();
    }
    text
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Partition {
    pub start: u32,
    pub sectors: u32,
}

/// Select exactly one FAT32 MBR partition wholly within `blocks` device sectors.
/// Superfloppy detection is handled by [`mount`], not this parser.
pub fn partition(sector: &[u8; 512], blocks: u32) -> Result<Partition, Error> {
    if sector[510..] != [0x55, 0xaa] {
        return Err(Error::InvalidPartition);
    }
    let mut selected = None;
    for p in (446..510).step_by(16) {
        if !matches!(sector[p + 4], 0x0b | 0x0c | 0x1b | 0x1c) {
            continue;
        }
        let start = le32(sector, p + 8);
        let sectors = le32(sector, p + 12);
        if !matches!(sector[p], 0 | 0x80)
            || start == 0
            || sectors == 0
            || start.checked_add(sectors).is_none_or(|end| end > blocks)
            || selected.is_some()
        {
            return Err(Error::InvalidPartition);
        }
        selected = Some(Partition { start, sectors });
    }
    selected.ok_or(Error::InvalidPartition)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Fat32 {
    pub partition: Partition,
    pub fat_start: u32,
    pub data_start: u32,
    pub sectors_per_cluster: u32,
    pub clusters: u32,
    pub root: u32,
}
impl Fat32 {
    /// Validate a FAT32 boot sector against its containing partition's bounds.
    pub fn parse(b: &[u8; 512], partition: Partition) -> Result<Self, Error> {
        let spc = u32::from(b[13]);
        let reserved = u32::from(le16(b, 14));
        let fats = u32::from(b[16]);
        let total = le32(b, 32);
        let fat_size = le32(b, 36);
        let flags = le16(b, 40);
        let root = le32(b, 44);
        if b[510..] != [0x55, 0xaa]
            || !matches!(b[0], 0xeb | 0xe9)
            || le16(b, 11) != 512
            || !spc.is_power_of_two()
            || spc > 128
            || reserved == 0
            || !(1..=2).contains(&fats)
            || le16(b, 17) != 0
            || le16(b, 19) != 0
            || le16(b, 22) != 0
            || le16(b, 42) != 0
            || total == 0
            || total > partition.sectors
            || fat_size == 0
            || flags & !0x008f != 0
            || partition.start.checked_add(total).is_none()
        {
            return Err(Error::InvalidBpb);
        }
        let overhead = fat_size
            .checked_mul(fats)
            .and_then(|n| n.checked_add(reserved))
            .ok_or(Error::InvalidBpb)?;
        let clusters = total.checked_sub(overhead).ok_or(Error::InvalidBpb)? / spc;
        let active = if flags & 0x80 == 0 {
            0
        } else {
            u32::from(flags & 15)
        };
        if !(65525..=0x0fff_ffed).contains(&clusters)
            || active >= fats
            || u64::from(fat_size) * 128 < u64::from(clusters) + 2
            || root < 2
            || root >= clusters + 2
        {
            return Err(Error::InvalidBpb);
        }
        Ok(Self {
            partition,
            fat_start: partition.start + reserved + active * fat_size,
            data_start: partition.start + overhead,
            sectors_per_cluster: spc,
            clusters,
            root,
        })
    }
    pub fn cluster_sector(&self, cluster: u32) -> Result<u32, Error> {
        if cluster < 2 || cluster >= self.clusters + 2 {
            return Err(Error::InvalidChain);
        }
        Ok(self.data_start + (cluster - 2) * self.sectors_per_cluster)
    }
    pub fn fat_location(&self, cluster: u32) -> Result<(u32, usize), Error> {
        self.cluster_sector(cluster)?;
        Ok((self.fat_start + cluster / 128, (cluster % 128) as usize * 4))
    }
    pub fn next(&self, sector: &[u8; 512], offset: usize) -> Result<Option<u32>, Error> {
        if offset > 508 || !offset.is_multiple_of(4) {
            return Err(Error::InvalidChain);
        }
        let value = le32(sector, offset) & 0x0fff_ffff;
        if value >= 0x0fff_fff8 {
            return Ok(None);
        }
        self.cluster_sector(value)?;
        Ok(Some(value))
    }
}

#[derive(Default)]
pub struct Chain {
    seen: Vec<u32, 128>,
}
impl Chain {
    /// Reject repeated clusters and walks beyond 128 entries; does not validate cluster range.
    pub fn visit(&mut self, cluster: u32) -> Result<(), Error> {
        if self.seen.contains(&cluster) {
            return Err(Error::InvalidChain);
        }
        self.seen.push(cluster).map_err(|_| Error::Limit)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct File {
    pub name: String<13>,
    pub cluster: u32,
    pub size: u32,
    pub directory: bool,
}

/// None skips deleted, LFN, volume-label and dot entries. Caller handles 0x00 end.
pub fn directory_entry(b: &[u8]) -> Result<Option<File>, Error> {
    if b.len() != 32 {
        return Err(Error::InvalidFile);
    }
    if matches!(b[0], 0 | 0xe5 | b'.') || b[11] & 0x0f == 0x0f || b[11] & 8 != 0 {
        return Ok(None);
    }
    if b[11] & 0xc0 != 0 {
        return Err(Error::InvalidFile);
    }
    let mut name = String::new();
    for (i, part) in [&b[..8], &b[8..11]].iter().enumerate() {
        let end = part.iter().rposition(|&c| c != b' ').map_or(0, |p| p + 1);
        if i == 0 && end == 0 {
            return Err(Error::InvalidFile);
        }
        if i == 1 && end != 0 {
            let _ = name.push('.');
        }
        for &c in &part[..end] {
            // Deliberately reject OEM/non-ASCII names rather than misdecode them.
            if !(33..=126).contains(&c) || b"\"*+,/:;<=>?[\\]|".contains(&c) {
                return Err(Error::InvalidFile);
            }
            let _ = name.push(char::from(c));
        }
    }
    let cluster = (u32::from(le16(b, 20)) << 16) | u32::from(le16(b, 26));
    if cluster & 0xf000_0000 != 0 {
        return Err(Error::InvalidFile);
    }
    Ok(Some(File {
        name,
        cluster,
        size: le32(b, 28),
        directory: b[11] & 16 != 0,
    }))
}

/// A read-only sector source. Implementations must bound and yield hardware waits.
pub trait BlockDevice {
    /// Preserve transport failures while accepting parser and traversal errors.
    type Error: From<Error>;
    /// Fill one complete sector at a device-relative LBA on success.
    /// Callers must not consume the buffer after an error.
    fn read_sector(
        &mut self,
        lba: u32,
        bytes: &mut [u8; 512],
    ) -> impl core::future::Future<Output = Result<(), Self::Error>>;
}

/// Try a FAT32 superfloppy first, then one MBR partition, without modifying media.
/// `blocks` is the device capacity in 512-byte sectors; I/O errors propagate unchanged.
pub async fn mount<B: BlockDevice>(device: &mut B, blocks: u32) -> Result<Fat32, B::Error> {
    let mut bytes = [0; 512];
    device.read_sector(0, &mut bytes).await?;
    if let Ok(volume) = Fat32::parse(
        &bytes,
        Partition {
            start: 0,
            sectors: blocks,
        },
    ) {
        return Ok(volume);
    }
    let part = partition(&bytes, blocks).map_err(B::Error::from)?;
    device.read_sector(part.start, &mut bytes).await?;
    Fat32::parse(&bytes, part).map_err(B::Error::from)
}

async fn next_cluster<B: BlockDevice>(
    device: &mut B,
    volume: Fat32,
    cluster: u32,
) -> Result<Option<u32>, B::Error> {
    let (lba, offset) = volume.fat_location(cluster).map_err(B::Error::from)?;
    let mut bytes = [0; 512];
    device.read_sector(lba, &mut bytes).await?;
    volume.next(&bytes, offset).map_err(B::Error::from)
}

/// Return a complete root 8.3 listing or an error; never publish a partial index mapping.
/// Reject listings beyond 32 entries or 32 directory sectors with [`Error::Limit`].
pub async fn list<B: BlockDevice>(
    device: &mut B,
    volume: Fat32,
) -> Result<Vec<File, MAX_FILES>, B::Error> {
    let mut files = Vec::new();
    let mut cluster = volume.root;
    let mut chain = Chain::default();
    let mut sectors = 0;
    let mut bytes = [0; 512];
    loop {
        chain.visit(cluster).map_err(B::Error::from)?;
        let start = volume.cluster_sector(cluster).map_err(B::Error::from)?;
        for n in 0..volume.sectors_per_cluster {
            if sectors == 32 {
                return Err(Error::Limit.into());
            }
            sectors += 1;
            device.read_sector(start + n, &mut bytes).await?;
            for entry in bytes.as_chunks::<32>().0 {
                if entry[0] == 0 {
                    return Ok(files);
                }
                if let Some(file) = directory_entry(entry).map_err(B::Error::from)? {
                    if file.directory || file.size != 0 {
                        volume
                            .cluster_sector(file.cluster)
                            .map_err(B::Error::from)?;
                    }
                    files.push(file).map_err(|_| B::Error::from(Error::Limit))?;
                }
            }
        }
        match next_cluster(device, volume, cluster).await? {
            Some(next) => cluster = next,
            None => return Ok(files),
        }
    }
}

/// Read at most 128 bytes, including unaligned sector/cluster boundary crossings.
/// Re-traverse from the start for each request; memory never scales with file size.
/// Reject directories, files over 64 KiB, and offsets past EOF; EOF itself returns empty.
/// Chain cycles, premature chain ends and the 128-cluster budget remain errors.
pub async fn read<B: BlockDevice>(
    device: &mut B,
    volume: Fat32,
    file: &File,
    offset: u32,
) -> Result<Vec<u8, 128>, B::Error> {
    if file.directory || file.size > MAX_FILE_SIZE || offset > file.size {
        return Err(Error::InvalidFile.into());
    }
    let length = core::cmp::min(128, file.size - offset) as usize;
    let mut data = Vec::new();
    if length == 0 {
        return Ok(data);
    }
    let cluster_bytes = volume.sectors_per_cluster * 512;
    let mut cluster = file.cluster;
    let mut chain = Chain::default();
    chain.visit(cluster).map_err(B::Error::from)?;
    for _ in 0..offset / cluster_bytes {
        cluster = next_cluster(device, volume, cluster)
            .await?
            .ok_or_else(|| B::Error::from(Error::InvalidChain))?;
        chain.visit(cluster).map_err(B::Error::from)?;
    }
    let mut position = offset % cluster_bytes;
    let mut bytes = [0; 512];
    while data.len() < length {
        let lba = volume.cluster_sector(cluster).map_err(B::Error::from)? + position / 512;
        device.read_sector(lba, &mut bytes).await?;
        let begin = (position % 512) as usize;
        let count = core::cmp::min(512 - begin, length - data.len());
        data.extend_from_slice(&bytes[begin..begin + count])
            .map_err(|_| B::Error::from(Error::Limit))?;
        position += count as u32;
        if position == cluster_bytes && data.len() < length {
            cluster = next_cluster(device, volume, cluster)
                .await?
                .ok_or_else(|| B::Error::from(Error::InvalidChain))?;
            chain.visit(cluster).map_err(B::Error::from)?;
            position = 0;
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bpb() -> [u8; 512] {
        let mut b = [0; 512];
        b[0] = 0xeb;
        b[11..13].copy_from_slice(&512u16.to_le_bytes());
        b[13] = 1;
        b[14..16].copy_from_slice(&32u16.to_le_bytes());
        b[16] = 2;
        b[32..36].copy_from_slice(&70000u32.to_le_bytes());
        b[36..40].copy_from_slice(&550u32.to_le_bytes());
        b[44..48].copy_from_slice(&2u32.to_le_bytes());
        b[510..].copy_from_slice(&[0x55, 0xaa]);
        b
    }
    fn volume() -> Fat32 {
        Fat32::parse(
            &bpb(),
            Partition {
                start: 100,
                sectors: 70000,
            },
        )
        .unwrap()
    }
    #[test]
    fn bot_exact_wire_and_read_only() {
        let b = cbw(0x12345678, &[0x28, 0, 0, 0, 0, 7, 0, 0, 1, 0], 512).unwrap();
        assert_eq!(
            &b[..15],
            &[85, 83, 66, 67, 120, 86, 52, 18, 0, 2, 0, 0, 128, 0, 10]
        );
        assert!(cbw(1, &[0x2a; 10], 512).is_err());
        assert!(cbw(1, &[0x12; 6], 512).is_err());
        let mut s = [0; 13];
        s[..4].copy_from_slice(b"USBS");
        s[4..8].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(csw(&s, 7, 512, 512), Ok(()));
        assert!(csw(&s[..12], 7, 512, 512).is_err());
        assert!(csw(&s, 8, 512, 512).is_err());
        assert!(csw(&s, 7, 512, 511).is_err());
        s[12] = 1;
        assert_eq!(csw(&s, 7, 512, 512), Err(Error::CommandFailed));
        s[12] = 2;
        assert_eq!(csw(&s, 7, 512, 512), Err(Error::InvalidBot));
        s[12] = 0;
        s[0] = 0;
        assert!(csw(&s, 7, 512, 512).is_err());
    }
    #[test]
    fn partition_bounds_and_ambiguity() {
        let mut b = [0; 512];
        b[510..].copy_from_slice(&[85, 170]);
        b[450] = 0x0c;
        b[454..458].copy_from_slice(&100u32.to_le_bytes());
        b[458..462].copy_from_slice(&70000u32.to_le_bytes());
        assert_eq!(partition(&b, 70100).unwrap().start, 100);
        assert!(partition(&b, 70099).is_err());
        b[466] = 0x0b;
        assert!(partition(&b, 70100).is_err());
    }
    #[test]
    fn bpb_geometry_bounds_active_fat() {
        let v = volume();
        assert_eq!(v.cluster_sector(2), Ok(1232));
        assert!(v.cluster_sector(1).is_err());
        assert!(v.cluster_sector(v.clusters + 2).is_err());
        for p in [11, 13, 14, 16, 32, 36, 44, 510] {
            let mut b = bpb();
            b[p] = 0;
            if p == 11 {
                b[12] = 0;
            }
            if p == 32 {
                b[32..36].fill(0);
            }
            assert!(Fat32::parse(&b, v.partition).is_err(), "field {p}");
        }
        let mut b = bpb();
        b[40] = 0x81;
        assert_eq!(
            Fat32::parse(&b, v.partition).unwrap().fat_start,
            v.fat_start + 550
        );
        b[40] = 0x82;
        assert!(Fat32::parse(&b, v.partition).is_err());
    }
    #[test]
    fn malformed_geometry_rejects_disguised_fat_and_overflow() {
        let part = volume().partition;
        for clusters in [4084u32, 4085, 65524] {
            let mut b = bpb();
            b[32..36].copy_from_slice(&(32 + 1100 + clusters).to_le_bytes());
            assert_eq!(Fat32::parse(&b, part), Err(Error::InvalidBpb));
        }
        let mut b = bpb();
        b[36..40].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(Fat32::parse(&b, part), Err(Error::InvalidBpb));
        b[36..40].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(Fat32::parse(&b, part), Err(Error::InvalidBpb));
        assert_eq!(
            Fat32::parse(
                &bpb(),
                Partition {
                    start: u32::MAX - 100,
                    sectors: 70000
                }
            ),
            Err(Error::InvalidBpb)
        );
        let mut mbr = [0; 512];
        mbr[510..].copy_from_slice(&[0x55, 0xaa]);
        mbr[450] = 0x0c;
        mbr[454..458].copy_from_slice(&(u32::MAX - 100).to_le_bytes());
        mbr[458..462].copy_from_slice(&200u32.to_le_bytes());
        assert_eq!(partition(&mbr, u32::MAX), Err(Error::InvalidPartition));
    }

    #[test]
    fn fragmented_chain_cycles_reserved_and_end() {
        let v = volume();
        let mut b = [0; 512];
        let mut chain = Chain::default();
        for c in [2, 17, 4, 90] {
            chain.visit(c).unwrap();
        }
        assert_eq!(chain.visit(17), Err(Error::InvalidChain));
        for c in [0, 1, 0x0fff_fff0, 0x0fff_fff7, v.clusters + 2] {
            b[..4].copy_from_slice(&c.to_le_bytes());
            assert!(v.next(&b, 0).is_err());
        }
        b[..4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert_eq!(v.next(&b, 0), Ok(None));
        b[..4].copy_from_slice(&17u32.to_le_bytes());
        assert_eq!(v.next(&b, 0), Ok(Some(17)));
        let mut chain = Chain::default();
        for c in 2..130 {
            chain.visit(c).unwrap();
        }
        assert_eq!(chain.visit(130), Err(Error::Limit));
    }
    #[test]
    fn short_directory_names_and_lfn_skipping() {
        let mut b = [0; 32];
        b[..11].copy_from_slice(b"HELLO   TXT");
        b[26] = 2;
        b[28] = 127;
        let f = directory_entry(&b).unwrap().unwrap();
        assert_eq!(f.name, "HELLO.TXT");
        assert_eq!(f.size, 127);
        assert!(!f.directory);
        b[11] = 16;
        assert!(directory_entry(&b).unwrap().unwrap().directory);
        b[11] = 15;
        assert_eq!(directory_entry(&b), Ok(None));
        b[11] = 0;
        b[0] = 0xe5;
        assert_eq!(directory_entry(&b), Ok(None));
    }
    struct Disk {
        sectors: Vec<(u32, [u8; 512]), 16>,
        reads: usize,
        deleted_fallback: bool,
    }
    impl Disk {
        fn new() -> Self {
            Self {
                sectors: Vec::new(),
                reads: 0,
                deleted_fallback: false,
            }
        }
        fn put(&mut self, lba: u32, bytes: [u8; 512]) {
            if let Some((_, old)) = self.sectors.iter_mut().find(|(n, _)| *n == lba) {
                *old = bytes;
            } else {
                self.sectors.push((lba, bytes)).unwrap();
            }
        }
    }
    impl BlockDevice for Disk {
        type Error = Error;
        async fn read_sector(&mut self, lba: u32, bytes: &mut [u8; 512]) -> Result<(), Error> {
            self.reads += 1;
            if let Some((_, sector)) = self.sectors.iter().find(|(n, _)| *n == lba) {
                *bytes = *sector;
                Ok(())
            } else if self.deleted_fallback {
                *bytes = [0xe5; 512];
                Ok(())
            } else {
                Err(Error::InvalidChain)
            }
        }
    }
    fn run<F: core::future::Future>(future: F) -> F::Output {
        let mut future = core::pin::pin!(future);
        let mut context = core::task::Context::from_waker(core::task::Waker::noop());
        match future.as_mut().poll(&mut context) {
            core::task::Poll::Ready(result) => result,
            core::task::Poll::Pending => panic!("fake sector reads must complete immediately"),
        }
    }
    fn file_entry(cluster: u16, size: u32) -> [u8; 32] {
        let mut entry = [0; 32];
        entry[..11].copy_from_slice(b"FRAGMENTBIN");
        entry[26..28].copy_from_slice(&cluster.to_le_bytes());
        entry[28..32].copy_from_slice(&size.to_le_bytes());
        entry
    }
    #[test]
    fn mount_mbr_and_superfloppy_real_read_path() {
        let mut disk = Disk::new();
        disk.put(0, bpb());
        assert_eq!(run(mount(&mut disk, 70000)).unwrap().partition.start, 0);
        let mut mbr = [0; 512];
        mbr[510..].copy_from_slice(&[85, 170]);
        mbr[450] = 12;
        mbr[454..458].copy_from_slice(&100u32.to_le_bytes());
        mbr[458..462].copy_from_slice(&70000u32.to_le_bytes());
        disk.put(0, mbr);
        disk.put(100, bpb());
        assert_eq!(run(mount(&mut disk, 70100)).unwrap(), volume());
        assert!(run(mount(&mut disk, 70000)).is_err());
    }
    #[test]
    fn fragmented_file_unaligned_crossings_eof_and_truncated_chain() {
        let v = volume();
        let mut disk = Disk::new();
        let mut fat = [0; 512];
        fat[12..16].copy_from_slice(&9u32.to_le_bytes());
        fat[36..40].copy_from_slice(&4u32.to_le_bytes());
        fat[16..20].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
        disk.put(v.fat_start, fat);
        for (cluster, value) in [(3, 11), (9, 22), (4, 33)] {
            disk.put(v.cluster_sector(cluster).unwrap(), [value; 512]);
        }
        let mut file = directory_entry(&file_entry(3, 1100)).unwrap().unwrap();
        let data = run(read(&mut disk, v, &file, 500)).unwrap();
        assert_eq!(&data[..12], &[11; 12]);
        assert_eq!(&data[12..], &[22; 116]);
        let data = run(read(&mut disk, v, &file, 1020)).unwrap();
        assert_eq!(&data[..4], &[22; 4]);
        assert_eq!(&data[4..], &[33; 76]);
        assert!(run(read(&mut disk, v, &file, 1100)).unwrap().is_empty());
        assert!(run(read(&mut disk, v, &file, 1101)).is_err());
        let reads = disk.reads;
        assert_eq!(
            run(read(&mut disk, v, &file, u32::MAX)),
            Err(Error::InvalidFile)
        );
        assert_eq!(disk.reads, reads);
        fat[12..16].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
        disk.put(v.fat_start, fat);
        assert_eq!(
            run(read(&mut disk, v, &file, 500)),
            Err(Error::InvalidChain)
        );
        fat[12..16].copy_from_slice(&9u32.to_le_bytes());
        fat[36..40].copy_from_slice(&3u32.to_le_bytes());
        disk.put(v.fat_start, fat);
        assert_eq!(
            run(read(&mut disk, v, &file, 1024)),
            Err(Error::InvalidChain)
        );
        file.directory = true;
        assert_eq!(run(read(&mut disk, v, &file, 0)), Err(Error::InvalidFile));
        file.directory = false;
        file.size = MAX_FILE_SIZE + 1;
        assert_eq!(run(read(&mut disk, v, &file, 0)), Err(Error::InvalidFile));
        file.size = 0;
        file.cluster = 0;
        assert!(run(read(&mut disk, v, &file, 0)).unwrap().is_empty());
    }
    #[test]
    fn root_listing_fragmentation_cycles_and_scan_budget() {
        let v = volume();
        let mut disk = Disk::new();
        let mut fat = [0; 512];
        fat[8..12].copy_from_slice(&5u32.to_le_bytes());
        fat[20..24].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
        disk.put(v.fat_start, fat);
        disk.put(v.cluster_sector(2).unwrap(), [0xe5; 512]);
        let mut root = [0; 512];
        root[..32].copy_from_slice(&file_entry(3, 42));
        disk.put(v.cluster_sector(5).unwrap(), root);
        let files = run(list(&mut disk, v)).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "FRAGMENT.BIN");
        assert_eq!(files[0].size, 42);
        disk.put(v.cluster_sector(5).unwrap(), [0xe5; 512]);
        fat[20..24].copy_from_slice(&2u32.to_le_bytes());
        disk.put(v.fat_start, fat);
        assert_eq!(run(list(&mut disk, v)), Err(Error::InvalidChain));
        let mut v = v;
        v.sectors_per_cluster = 128;
        disk.deleted_fallback = true;
        disk.reads = 0;
        assert_eq!(run(list(&mut disk, v)), Err(Error::Limit));
        assert_eq!(disk.reads, 32);
    }

    #[test]
    fn list_entry_limit_rejects_partial_mapping() {
        let v = volume();
        let mut disk = Disk::new();
        let mut fat = [0; 512];
        fat[8..12].copy_from_slice(&3u32.to_le_bytes());
        fat[12..16].copy_from_slice(&4u32.to_le_bytes());
        disk.put(v.fat_start, fat);
        let mut sector = [0; 512];
        for entry in sector.as_chunks_mut::<32>().0 {
            *entry = file_entry(6, 10);
        }
        disk.put(v.cluster_sector(2).unwrap(), sector);
        disk.put(v.cluster_sector(3).unwrap(), sector);
        let mut last = [0; 512];
        disk.put(v.cluster_sector(4).unwrap(), last);
        assert_eq!(run(list(&mut disk, v)).unwrap().len(), 32);
        last[..32].copy_from_slice(&file_entry(6, 10));
        disk.put(v.cluster_sector(4).unwrap(), last);
        assert_eq!(run(list(&mut disk, v)), Err(Error::Limit));
    }
    #[test]
    fn data_in_stall_preserves_count_for_csw() {
        use super::super::types::UsbError;

        assert_eq!(data_in_completion(Ok(512)), Ok((512, false)));
        assert_eq!(data_in_completion(Ok(64)), Ok((64, false)));
        for received in [0usize, 64, 512] {
            assert_eq!(
                data_in_completion(Err((UsbError::Stall, received))),
                Ok((received, true))
            );
            let mut status = [0; 13];
            status[..4].copy_from_slice(b"USBS");
            status[4..8].copy_from_slice(&7u32.to_le_bytes());
            status[8..12].copy_from_slice(&(512 - received as u32).to_le_bytes());
            status[12] = 1;
            assert_eq!(
                csw(&status, 7, 512, received as u32),
                Err(Error::CommandFailed)
            );
            status[12] = 2;
            assert_eq!(
                csw(&status, 7, 512, received as u32),
                Err(Error::InvalidBot)
            );
            status[12] = 1;
            status[4] = 8;
            assert_eq!(
                csw(&status, 7, 512, received as u32),
                Err(Error::InvalidBot)
            );
        }
        for error in [
            UsbError::Timeout,
            UsbError::Disconnected,
            UsbError::Controller,
        ] {
            assert_eq!(data_in_completion(Err((error, 64))), Err(error));
        }
    }

    #[test]
    fn sense_formats_and_residue_validation() {
        let mut fixed = [0; 18];
        fixed[0] = 0xf0;
        fixed[2] = 2;
        fixed[7] = 10;
        fixed[12] = 0x3a;
        assert_eq!(sense(&fixed), Ok((2, 0x3a, 0)));
        assert!(sense(&fixed[..13]).is_err());
        fixed[7] = 0;
        assert!(sense(&fixed).is_err());
        assert_eq!(sense(&[0x72, 6, 0x28, 0, 0, 0, 0, 0]), Ok((6, 0x28, 0)));
        let mut s = [0; 13];
        s[..4].copy_from_slice(b"USBS");
        s[4..8].copy_from_slice(&9u32.to_le_bytes());
        s[8..12].copy_from_slice(&20u32.to_le_bytes());
        assert_eq!(csw(&s, 9, 36, 16), Ok(()));
        assert!(csw(&s, 9, 36, 37).is_err());
        // A device may receive/send bytes that the SCSI command did not process.
        assert_eq!(csw(&s, 9, 36, 36), Ok(()));
        s[12] = 1;
        assert_eq!(csw(&s, 9, 36, 36), Err(Error::CommandFailed));
        s[12] = 0;
        s[8..12].copy_from_slice(&37u32.to_le_bytes());
        assert!(csw(&s, 9, 36, 0).is_err());
        assert!(cbw(1, &[0x28, 0, 0, 0, 0, 0, 0, 0, 2, 0], 512).is_err());
    }

    #[test]
    fn capacity_rejects_read_capacity16_and_non512() {
        assert_eq!(capacity(&[0, 0, 0, 9, 0, 0, 2, 0]), Ok((10, 512)));
        assert!(capacity(&[255, 255, 255, 255, 0, 0, 2, 0]).is_err());
        assert!(capacity(&[0, 0, 0, 9, 0, 0, 4, 0]).is_err());
    }
}
