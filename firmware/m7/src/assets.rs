//! Safe GPIO SPI mode 0 reader: no blocking peripheral-ready loops and no writes.
use embassy_stm32::gpio::{Input, Output};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use m7::dashboard::{HEADER_SIZE, Header, OFFSET, crc32};
use picoserve::{
    io::{Read, Write},
    response::{Content, IntoResponse, Response, ResponseWriter, StatusCode},
};

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

    async fn read(&mut self, address: u32, bytes: &mut [u8]) {
        // End every command before yielding, including on future cancellation.
        for (index, chunk) in bytes.chunks_mut(16).enumerate() {
            let address = address + (index * 16) as u32;
            self.select.set_low();
            self.byte(0x03);
            self.byte((address >> 16) as u8);
            self.byte((address >> 8) as u8);
            self.byte(address as u8);
            for byte in chunk {
                *byte = self.byte(0);
            }
            self.select.set_high();
            embassy_futures::yield_now().await;
        }
    }

    pub async fn validate(&mut self) -> Option<Header> {
        self.hold.set_high();
        self.protect.set_high();
        self.select.set_low();
        self.byte(0x9f);
        let jedec = [self.byte(0), self.byte(0), self.byte(0)];
        self.select.set_high();
        if jedec != [0x1f, 0x89, 0x01] {
            return None;
        }
        let mut bytes = [0; HEADER_SIZE];
        self.read(OFFSET, &mut bytes).await;
        let header = Header::parse(&bytes)?;
        let mut buffer = [0; 256];
        let mut checksum = !0;
        for offset in (0..header.length).step_by(buffer.len()) {
            let length = buffer.len().min(header.length - offset);
            self.read(
                OFFSET + HEADER_SIZE as u32 + offset as u32,
                &mut buffer[..length],
            )
            .await;
            if offset == 0 && buffer[..3] != [0x1f, 0x8b, 8] {
                return None;
            }
            checksum = crc32(checksum, &buffer[..length]);
        }
        (!checksum == header.checksum).then_some(header)
    }
}

pub struct Assets {
    pub flash: Mutex<NoopRawMutex, Flash>,
    pub header: Option<Header>,
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
            self.0
                .flash
                .lock()
                .await
                .read(
                    OFFSET + HEADER_SIZE as u32 + offset as u32,
                    &mut bytes[..length],
                )
                .await;
            writer.write_all(&bytes[..length]).await?;
        }
        Ok(())
    }
}
