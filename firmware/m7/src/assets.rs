//! Exclusive safe GPIO SPI owner; writes are driven only by the OTA actor.
use embassy_stm32::gpio::{Input, Output};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use m7::{
    dashboard::{HEADER_SIZE, Header, crc32},
    ota_control::Control,
    ota_manifest::{self as layout, Manifest},
    ota_nor::{Error, Nor, Spi},
    ota_progress::{READ_QUANTUM, blocks},
};
use picoserve::{
    io::{Read, Write},
    response::{Content, IntoResponse, Response, ResponseWriter, StatusCode},
};
use sha2::{Digest, Sha256};

pub struct Flash {
    pub clock: Output<'static>,
    pub select: Output<'static>,
    pub mosi: Output<'static>,
    pub miso: Input<'static>,
    pub hold: Output<'static>,
    pub protect: Output<'static>,
}

impl Flash {
    fn byte(&mut self, value: u8) -> u8 {
        let mut result = 0;
        for bit in (0..8).rev() {
            self.mosi.set_level(((value >> bit) & 1 != 0).into());
            cortex_m::asm::delay(32);
            self.clock.set_high();
            cortex_m::asm::delay(32);
            result = (result << 1) | u8::from(self.miso.is_high());
            self.clock.set_low();
        }
        result
    }
}

impl Spi for Flash {
    fn select(&mut self, selected: bool) {
        self.select.set_level((!selected).into());
        cortex_m::asm::delay(32);
    }
    fn byte(&mut self, value: u8) -> u8 {
        Flash::byte(self, value)
    }
}

pub struct Assets {
    pub flash: Mutex<NoopRawMutex, Option<Nor<Flash>>>,
    pub header: Option<Header>,
    pub slot: u32,
    pub metadata_slot: u32,
    pub current: Option<Manifest>,
    pub control: Control,
}

impl Assets {
    pub fn new(mut flash: Flash) -> Self {
        flash.hold.set_high();
        flash.protect.set_high();
        Self {
            flash: Mutex::new(Nor::open(flash).ok()),
            header: None,
            slot: layout::DASHBOARD_SLOTS[0],
            metadata_slot: layout::METADATA_SLOTS[0],
            current: None,
            control: Control::new(),
        }
    }

    pub async fn read(&self, address: u32, bytes: &mut [u8]) -> Result<(), Error> {
        for (index, chunk) in bytes.chunks_mut(READ_QUANTUM).enumerate() {
            {
                let mut flash = self.flash.lock().await;
                flash
                    .as_mut()
                    .ok_or(Error::Identity)?
                    .read(address + (index * READ_QUANTUM) as u32, chunk)?;
            }
            embassy_futures::yield_now().await;
        }
        Ok(())
    }

    pub async fn hash_with_progress(
        &self,
        address: u32,
        length: u32,
        progress: &mut dyn FnMut(),
    ) -> Result<[u8; 32], Error> {
        let mut digest = Sha256::new();
        let mut buffer = [0; 256];
        for block in blocks(0..length as usize) {
            self.read(address + block.start as u32, &mut buffer[..block.len()])
                .await?;
            digest.update(&buffer[..block.len()]);
            progress();
        }
        Ok(digest.finalize().into())
    }

    pub async fn dashboard(&self, slot: u32, manifest: &Manifest) -> Result<Header, Error> {
        // Startup authentication cannot feed an unconfirmed trial's watchdog.
        self.dashboard_with_progress(slot, manifest, &mut || {})
            .await
    }

    pub async fn dashboard_with_progress(
        &self,
        slot: u32,
        manifest: &Manifest,
        progress: &mut dyn FnMut(),
    ) -> Result<Header, Error> {
        let mut bytes = [0; HEADER_SIZE];
        self.read(slot, &mut bytes).await?;
        let header = Header::parse(&bytes).ok_or(Error::Verify)?;
        if (HEADER_SIZE + header.length).next_multiple_of(4096)
            != manifest.dashboard_length() as usize
        {
            return Err(Error::Verify);
        }
        let mut digest = Sha256::new();
        digest.update(bytes);
        let mut buffer = [0; 256];
        let mut checksum = !0;
        for block in blocks(HEADER_SIZE..manifest.dashboard_length() as usize) {
            let offset = block.start;
            let length = block.len();
            self.read(slot + offset as u32, &mut buffer[..length])
                .await?;
            if offset == HEADER_SIZE && buffer[..3] != [0x1f, 0x8b, 8] {
                return Err(Error::Verify);
            }
            let payload = (HEADER_SIZE + header.length)
                .saturating_sub(offset)
                .min(length);
            checksum = crc32(checksum, &buffer[..payload]);
            if buffer[payload..length].iter().any(|b| *b != 0xff) {
                return Err(Error::Verify);
            }
            digest.update(&buffer[..length]);
            progress();
        }
        if !checksum != header.checksum || digest.finalize().as_slice() != manifest.dashboard_hash()
        {
            return Err(Error::Verify);
        }
        Ok(header)
    }

    pub async fn initialize(
        &mut self,
        internal: embassy_stm32::Peri<'_, embassy_stm32::peripherals::FLASH>,
    ) -> Result<(), Error> {
        let mut internal = embassy_stm32::flash::Flash::new_blocking(internal);
        let mut digest = Sha256::new();
        let mut buffer = [0; 256];
        let mut m7 = [0; 8];
        let mut m4 = [0; 8];
        for offset in (0..layout::ACTIVE_LENGTH).step_by(256) {
            internal
                .blocking_read(layout::ACTIVE_ORIGIN - 0x08000000 + offset, &mut buffer)
                .map_err(|_| Error::Verify)?;
            if offset == 0 {
                m7.copy_from_slice(&buffer[..8]);
            }
            if offset == layout::M4_ORIGIN - layout::ACTIVE_ORIGIN {
                m4.copy_from_slice(&buffer[..8]);
            }
            digest.update(buffer);
            embassy_futures::yield_now().await;
        }
        let active_hash: [u8; 32] = digest.finalize().into();
        for metadata_slot in layout::METADATA_SLOTS {
            self.read(metadata_slot, &mut buffer).await?;
            let Ok(manifest) = Manifest::verify(&buffer, &layout::TRUSTED_KEY) else {
                continue;
            };
            if manifest.active_hash() != active_hash || !manifest.matches_running_build() {
                continue;
            }
            manifest
                .check_vectors(&m7, &m4)
                .map_err(|_| Error::Verify)?;
            for slot in layout::DASHBOARD_SLOTS {
                if let Ok(header) = self.dashboard(slot, &manifest).await {
                    self.slot = slot;
                    self.metadata_slot = metadata_slot;
                    self.header = Some(header);
                    self.current = Some(manifest);
                    return Ok(());
                }
            }
        }
        Err(Error::Verify)
    }
}

pub struct DashboardService(pub &'static Assets);

impl<State, PathParameters> picoserve::routing::RequestHandlerService<State, PathParameters>
    for DashboardService
{
    async fn call_request_handler_service<R: Read, W: ResponseWriter<Error = R::Error>>(
        &self,
        _state: &State,
        _path_parameters: PathParameters,
        request: picoserve::request::Request<'_, R>,
        response_writer: W,
    ) -> Result<picoserve::ResponseSent, W::Error> {
        let Some(header) = self.0.header else {
            return Response::new(StatusCode::SERVICE_UNAVAILABLE,
                "Dashboard assets missing or invalid. Use USB-C diagnostics; provision verified QSPI assets after backup.")
                .with_header("Cache-Control", "no-store")
                .write_to(request.body_connection.finalize().await?, response_writer).await;
        };
        let etag = header.etag();
        let unchanged = request
            .parts
            .headers()
            .get("If-None-Match")
            .is_some_and(|value| header.matches_etag(value.as_raw()));
        let connection = request.body_connection.finalize().await?;
        if unchanged {
            Response::empty(StatusCode::NOT_MODIFIED)
                .with_header("ETag", etag)
                .with_header("Cache-Control", "no-cache")
                .write_to(connection, response_writer)
                .await
        } else {
            Response::ok(Dashboard(self.0, header))
                .with_header("Content-Encoding", "gzip")
                .with_header("Cache-Control", "no-cache")
                .with_header("ETag", etag)
                .write_to(connection, response_writer)
                .await
        }
    }
}

struct Dashboard(&'static Assets, Header);

impl Content for Dashboard {
    fn content_type(&self) -> &'static str {
        "text/html; charset=utf-8"
    }
    fn content_length(&self) -> usize {
        self.1.length
    }
    async fn write_content<W: Write>(self, mut writer: W) -> Result<(), W::Error> {
        let mut bytes = [0; 256];
        for offset in (0..self.1.length).step_by(bytes.len()) {
            let length = bytes.len().min(self.1.length - offset);
            if self
                .0
                .read(
                    self.0.slot + HEADER_SIZE as u32 + offset as u32,
                    &mut bytes[..length],
                )
                .await
                .is_err()
            {
                return Ok(());
            }
            writer.write_all(&bytes[..length]).await?;
        }
        Ok(())
    }
}
