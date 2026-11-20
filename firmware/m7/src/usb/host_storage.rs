#![forbid(unsafe_code)]
//! M7-only orchestration over Host's safe packet API; no register access.
//! Every BOT command is bounded by the host's per-stage deadlines. Directory
//! scanning is capped at 32 sectors and chain walking at 128 clusters. Filesystem
//! I/O has a 30-second budget checked between complete BOT commands, never by
//! cancelling an active channel. No writes,
//! formatting, subdirectory traversal, LFN decoding or multi-LUN selection.

use core::fmt::Write;

use embassy_time::{Duration, Instant};
use heapless::{String, Vec};

use super::host::Host;
use super::host_logic::StorageEndpoint;
use super::storage::{self, Fat32, File, MAX_FILES};
use super::types::{HostCommand, UsbError, UsbEvent};

pub(super) struct Storage {
    endpoint: StorageEndpoint,
    input_toggle: bool,
    output_toggle: bool,
    tag: u32,
    blocks: u32,
    volume: Option<Fat32>,
    files: Vec<File, MAX_FILES>,
}

impl Storage {
    /// Probe LUN zero and attempt a read-only FAT32 mount. An unsupported filesystem
    /// is reported as an event but need not prevent retaining the BOT transport.
    pub(super) async fn initialize(
        host: &mut Host<'_>,
        endpoint: StorageEndpoint,
    ) -> Result<Self, UsbError> {
        let mut this = Self {
            endpoint,
            input_toggle: false,
            output_toggle: false,
            tag: 0,
            blocks: 0,
            volume: None,
            files: Vec::new(),
        };
        let mut inquiry = [0; 36];
        this.command(host, &[0x12, 0, 0, 0, 36, 0], &mut inquiry)
            .await?;
        if inquiry[0] != 0 || inquiry[3] & 15 < 2 || inquiry[4] < 31 {
            return Err(UsbError::Unsupported);
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match this.command(host, &[0; 6], &mut []).await {
                Ok(()) => break,
                Err(UsbError::Transaction) if Instant::now() < deadline => {
                    host.pause(250_000, true).await?
                }
                Err(error) => return Err(error),
            }
        }
        let mut capacity = [0; 8];
        this.command(host, &[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], &mut capacity)
            .await?;
        let (blocks, block_size) =
            storage::capacity(&capacity).map_err(|_| UsbError::Unsupported)?;
        this.blocks = blocks;
        let mounted = this.mount(host).await;
        host.storage_event(UsbEvent::Storage {
            blocks,
            block_size,
            vendor: storage::ascii(&inquiry[8..16]),
            product: storage::ascii(&inquiry[16..32]),
            fat32: this.volume.is_some(),
        })
        .await?;
        if let Err(error) = mounted {
            this.volume = None;
            storage_error(host, "Unsupported or malformed FAT32 volume").await?;
            if matches!(error, UsbError::Disconnected | UsbError::Controller) {
                return Err(error);
            }
        }
        Ok(this)
    }

    async fn recover(&mut self, host: &mut Host<'_>) -> Result<(), UsbError> {
        let result = async {
            host.control(
                [0x21, 0xff, 0, 0, self.endpoint.interface, 0, 0, 0],
                &mut [],
            )
            .await?;
            host.clear_endpoint_halt(self.endpoint.in_endpoint).await?;
            host.clear_endpoint_halt(self.endpoint.out_endpoint).await?;
            self.input_toggle = false;
            self.output_toggle = false;
            Ok(())
        }
        .await;
        // Failed recovery leaves BOT synchronization unknown: require a full
        // host reset instead of accepting another CBW with stale toggles.
        result.map_err(|error| {
            if error == UsbError::Disconnected {
                error
            } else {
                UsbError::Controller
            }
        })
    }

    async fn bot(
        &mut self,
        host: &mut Host<'_>,
        cdb: &[u8],
        data: &mut [u8],
    ) -> Result<(), BotError> {
        self.tag = self.tag.wrapping_add(1);
        let cbw = storage::cbw(self.tag, cdb, data.len() as u32).map_err(|_| BotError::Invalid)?;
        let result = async {
            if host
                .bulk_out(
                    self.endpoint.out_endpoint,
                    self.endpoint.out_mps,
                    &mut self.output_toggle,
                    &cbw,
                )
                .await?
                != 31
            {
                return Err(UsbError::Transaction);
            }
            let received = if data.is_empty() {
                0
            } else {
                let (received, clear_halt) = storage::data_in_completion(
                    host.bulk_in_counted(
                        self.endpoint.in_endpoint,
                        self.endpoint.in_mps,
                        &mut self.input_toggle,
                        data,
                    )
                    .await,
                )?;
                if clear_halt {
                    host.clear_endpoint_halt(self.endpoint.in_endpoint).await?;
                    self.input_toggle = false;
                }
                received
            };
            let mut csw = [0; 13];
            let first = host
                .bulk_in(
                    self.endpoint.in_endpoint,
                    self.endpoint.in_mps,
                    &mut self.input_toggle,
                    &mut csw,
                )
                .await;
            let count = match first {
                Err(UsbError::Stall) => {
                    host.clear_endpoint_halt(self.endpoint.in_endpoint).await?;
                    self.input_toggle = false;
                    host.bulk_in(
                        self.endpoint.in_endpoint,
                        self.endpoint.in_mps,
                        &mut self.input_toggle,
                        &mut csw,
                    )
                    .await?
                }
                result => result?,
            };
            Ok((csw, count, received))
        }
        .await;
        match result {
            Ok((csw, count, received)) => {
                match storage::csw(&csw[..count], self.tag, data.len() as u32, received as u32) {
                    Ok(()) if received == data.len() && csw[8..12] == [0; 4] => Ok(()),
                    Err(storage::Error::CommandFailed) => Err(BotError::Failed),
                    Ok(()) => Err(BotError::Short),
                    _ => {
                        self.recover(host).await.map_err(BotError::Usb)?;
                        Err(BotError::Invalid)
                    }
                }
            }
            Err(error) => {
                if !matches!(error, UsbError::Disconnected | UsbError::Controller) {
                    self.recover(host).await.map_err(BotError::Usb)?;
                }
                Err(BotError::Usb(error))
            }
        }
    }

    async fn command(
        &mut self,
        host: &mut Host<'_>,
        cdb: &[u8],
        data: &mut [u8],
    ) -> Result<(), UsbError> {
        match self.bot(host, cdb, data).await {
            Ok(()) => Ok(()),
            Err(BotError::Failed) => {
                let mut sense = [0; 18];
                // REQUEST SENSE is itself a complete BOT command, never recursively retried.
                self.bot(host, &[3, 0, 0, 0, 18, 0], &mut sense)
                    .await
                    .map_err(BotError::usb)?;
                let (key, asc, ascq) = storage::sense(&sense).map_err(|_| UsbError::Transaction)?;
                let mut message: String<96> = String::new();
                let _ = write!(
                    message,
                    "SCSI command {:02x} failed: sense key {:02x}, ASC {:02x}, ASCQ {:02x}",
                    cdb[0], key, asc, ascq
                );
                host.storage_event(UsbEvent::StorageError { message })
                    .await?;
                Err(UsbError::Transaction)
            }
            Err(error) => Err(error.usb()),
        }
    }

    async fn sector(
        &mut self,
        host: &mut Host<'_>,
        lba: u32,
        data: &mut [u8; 512],
    ) -> Result<(), UsbError> {
        if lba >= self.blocks {
            return Err(UsbError::InvalidDescriptor);
        }
        let b = lba.to_be_bytes();
        self.command(host, &[0x28, 0, b[0], b[1], b[2], b[3], 0, 0, 1, 0], data)
            .await
    }

    async fn mount(&mut self, host: &mut Host<'_>) -> Result<(), UsbError> {
        let blocks = self.blocks;
        self.volume = Some(
            storage::mount(
                &mut Reader {
                    storage: self,
                    host,
                    deadline: Instant::now() + Duration::from_secs(30),
                },
                blocks,
            )
            .await?,
        );
        Ok(())
    }

    async fn list(&mut self, host: &mut Host<'_>) -> Result<(), UsbError> {
        self.files.clear();
        let volume = self.volume.ok_or(UsbError::Unsupported)?;
        self.files = storage::list(
            &mut Reader {
                storage: self,
                host,
                deadline: Instant::now() + Duration::from_secs(30),
            },
            volume,
        )
        .await?;
        // Publish only a fully validated, bounded listing. Indices stay stable
        // until the next list command or disconnect; the medium is never written.
        for (index, file) in self.files.iter().enumerate() {
            host.storage_event(UsbEvent::File {
                index: index as u16,
                name: file.name.clone(),
                size: file.size,
                directory: file.directory,
            })
            .await?;
        }
        Ok(())
    }

    async fn read(&mut self, host: &mut Host<'_>, index: u16, offset: u32) -> Result<(), UsbError> {
        let volume = self.volume.ok_or(UsbError::Unsupported)?;
        let file = self
            .files
            .get(usize::from(index))
            .cloned()
            .ok_or(UsbError::Unsupported)?;
        let data = storage::read(
            &mut Reader {
                storage: self,
                host,
                deadline: Instant::now() + Duration::from_secs(30),
            },
            volume,
            &file,
            offset,
        )
        .await?;
        host.storage_event(UsbEvent::FileData {
            index,
            offset,
            total_size: file.size,
            eof: offset + data.len() as u32 == file.size,
            data,
        })
        .await
    }

    /// Handle list/read requests, reporting recoverable failures through storage events.
    /// `Ok(())` does not imply command success; disconnect/controller failures propagate.
    pub(super) async fn handle(
        &mut self,
        host: &mut Host<'_>,
        command: HostCommand,
    ) -> Result<(), UsbError> {
        let result = match command {
            HostCommand::StorageList => self.list(host).await,
            HostCommand::StorageRead { file, offset } => self.read(host, file, offset).await,
            _ => return Ok(()),
        };
        if let Err(error) = result {
            if matches!(command, HostCommand::StorageList) {
                self.files.clear();
            }
            let message = match error {
                UsbError::Unsupported => {
                    "FAT32 unavailable, invalid file/offset, directory, or file exceeds 64 KiB"
                }
                UsbError::Overflow => {
                    "Storage listing exceeds 32 entries, 32 sectors, or chain traversal limit"
                }
                UsbError::InvalidDescriptor => {
                    "Malformed FAT32 entry, truncated chain, cycle, or out-of-volume cluster"
                }
                _ => "Storage command failed; check medium readiness or reset the device",
            };
            storage_error(host, message).await?;
            if matches!(error, UsbError::Disconnected | UsbError::Controller) {
                return Err(error);
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
enum BotError {
    Usb(UsbError),
    Failed,
    Invalid,
    Short,
}
impl BotError {
    fn usb(self) -> UsbError {
        match self {
            Self::Usb(error) => error,
            _ => UsbError::Transaction,
        }
    }
}
struct Reader<'a, 'd> {
    storage: &'a mut Storage,
    host: &'a mut Host<'d>,
    deadline: Instant,
}
impl storage::BlockDevice for Reader<'_, '_> {
    type Error = UsbError;
    async fn read_sector(&mut self, lba: u32, bytes: &mut [u8; 512]) -> Result<(), UsbError> {
        if Instant::now() >= self.deadline {
            return Err(UsbError::Timeout);
        }
        // Complete BOT (including recovery) before enforcing the scan budget;
        // cancelling an in-flight host packet could leave the channel active.
        self.storage.sector(self.host, lba, bytes).await?;
        if Instant::now() >= self.deadline {
            return Err(UsbError::Timeout);
        }
        Ok(())
    }
}
impl From<storage::Error> for UsbError {
    fn from(error: storage::Error) -> Self {
        match error {
            storage::Error::Limit => Self::Overflow,
            storage::Error::InvalidFile => Self::Unsupported,
            _ => Self::InvalidDescriptor,
        }
    }
}
pub(super) async fn storage_error(host: &mut Host<'_>, message: &str) -> Result<(), UsbError> {
    host.storage_event(UsbEvent::StorageError {
        message: storage::ascii(message.as_bytes()),
    })
    .await
}
