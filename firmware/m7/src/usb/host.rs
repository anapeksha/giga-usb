//! Project-owned, non-DMA USB-A host for the H747 GIGA.
//!
//! Timer-driven polling, NOT IRQ-driven: all register waits have deadlines and
//! yield to the executor. Only one root device and one channel are supported;
//! no hubs or isochronous transfers. HID interpretation and MSC command handling
//! are separate from this packet transport.
//! Register offsets/fields follow locked stm32-metapac 21 otg_v1; power/PHY
//! sequencing was cross-checked against embassy-stm32 0.6 and synopsys-otg 0.3.3.
//! No code from either USB stack is invoked. Boot must provide USBSEL=HSI48.

use embassy_stm32::gpio::{Level, Output, Speed};
use embassy_stm32::{Peri, pac, pac::otg::regs, peripherals};
use embassy_time::{Duration, Instant, Timer};
use heapless::{Deque, Vec};

use super::host_logic::{
    Packet, SelectedConfig, channel_outcome, halt_outcome, queue_capture,
    root_channel_characteristics, select_configuration,
};
use super::types::{
    CapturedEvent, DESCRIPTOR_CHUNK_SIZE, Direction, HostCommand, HostState, MAX_DESCRIPTOR_SIZE,
    TransferKind, TransferStage, UsbError, UsbEvent, UsbSpeed,
};
use crate::ipc_delivery::Delivery;
use crate::network_bus::Bus;
use shared::protocol::{Event, Message};
use shared::usb::proxy::{BootKind, ProxyFault, ProxySupport};

const ENABLE: u32 = 1 << 31;
const HALT: u32 = 1 << 30;
const RX_LEVEL: u32 = 1 << 4;
const PORT_CHANGES: u32 = (1 << 1) | (1 << 3) | (1 << 5);

/// Owns the HS controller and USB-A pins for its entire lifetime. USB-C/FS is
/// never accessed. Do not construct another driver for these resources on M4.
/// IPC delivery is independent; this task only accesses core-local channels.
pub struct Host<'d> {
    _usb: Peri<'d, peripherals::USB_OTG_HS>,
    _dm: Peri<'d, peripherals::PB14>,
    _dp: Peri<'d, peripherals::PB15>,
    vbus: Output<'d>,
    delivery: &'d Delivery,
    network: Option<&'d Bus>,
    command_turn: crate::ipc_delivery::CommandTurn,
    storage_commands: Deque<HostCommand, 4>,
    enabled: bool,
    restart: bool,
    attached: bool,
    address: u8,
    mps: u16,
    low: bool,
    transfers: u32,
    errors: u32,
    dropped: u32,
    traced_dropped: u32,
    dropped_trace: fn(u32),
    status_at: Instant,
    session_id: u32,
    transfer_id: u32,
    transfer_sequence: u32,
    stage: TransferStage,
    state: HostState,
    setup: [u8; 8],
    active_endpoint: u8,
    attempt_started: Option<Instant>,
    attempt_retries: u32,
    failure_snapshot: Option<UsbEvent>,
    failure_emitted: bool,
    proxy_keyboard: Option<u8>,
    proxy_transfer_generation: Option<u32>,
}

impl<'d> Host<'d> {
    pub fn new(
        usb: Peri<'d, peripherals::USB_OTG_HS>,
        dm: Peri<'d, peripherals::PB14>,
        dp: Peri<'d, peripherals::PB15>,
        vbus: Peri<'d, peripherals::PA15>,
        delivery: &'d Delivery,
    ) -> Self {
        Self {
            _usb: usb,
            _dm: dm,
            _dp: dp,
            vbus: Output::new(vbus, Level::Low, Speed::Low),
            delivery,
            network: None,
            command_turn: crate::ipc_delivery::CommandTurn::default(),
            storage_commands: Deque::new(),
            // USB-A VBUS stays off until an explicit browser Start command.
            enabled: false,
            restart: false,
            attached: false,
            address: 0,
            mps: 8,
            low: false,
            transfers: 0,
            errors: 0,
            dropped: 0,
            traced_dropped: 0,
            dropped_trace: |_| {},
            status_at: Instant::now() + Duration::from_secs(1),
            session_id: 0,
            transfer_id: 0,
            transfer_sequence: 0,
            stage: TransferStage::Setup,
            state: HostState::Stopped,
            setup: [0; 8],
            active_endpoint: 0,
            attempt_started: None,
            attempt_retries: 0,
            failure_snapshot: None,
            failure_emitted: false,
            proxy_keyboard: None,
            proxy_transfer_generation: None,
        }
    }

    pub fn with_network(mut self, network: &'d Bus) -> Self {
        self.network = Some(network);
        self
    }

    fn port(&self, clear: u32, set: u32) {
        // PENA is write-one-to-disable, the three change flags are W1C.
        // Never echo any of these bits in a read/modify/write.
        pac::USB_OTG_HS.hprt().write_value(regs::Hprt(
            (pac::USB_OTG_HS.hprt().read().0 & !(PORT_CHANGES | 4 | clear)) | set,
        ));
    }

    fn connected(&self) -> bool {
        pac::USB_OTG_HS.hprt().read().0 & 1 != 0
            && pac::USB_OTG_HS.gintsts().read().0 & (1 << 29) == 0
    }

    fn collect_delivery_failures(&mut self) {
        self.dropped = self.dropped.saturating_add(self.delivery.failed.replace(0));
    }

    fn emit(&mut self, event: UsbEvent) {
        if let Some(bus) = self.network {
            bus.observe_host(&event, Instant::now().as_millis());
        }
        self.collect_delivery_failures();
        let record = CapturedEvent {
            timestamp_us: Instant::now().as_micros(),
            event,
        };
        match Message::encode(Event::Usb, &record) {
            Ok(message) => {
                if queue_capture(&self.delivery.captures, message, &mut self.dropped).is_err() {
                    self.dropped = self.dropped.saturating_add(1);
                }
            }
            _ => self.dropped = self.dropped.saturating_add(1),
        }
        self.collect_delivery_failures();
    }

    async fn emit_reliable(&mut self, event: UsbEvent, deadline: Instant) -> Result<(), UsbError> {
        let record = CapturedEvent {
            timestamp_us: Instant::now().as_micros(),
            event,
        };
        let message = Message::encode(Event::Usb, &record).map_err(|_| {
            self.dropped = self.dropped.saturating_add(1);
            UsbError::Overflow
        })?;
        loop {
            self.collect_delivery_failures();
            if queue_capture(&self.delivery.captures, message.clone(), &mut self.dropped).is_ok() {
                self.collect_delivery_failures();
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.dropped = self.dropped.saturating_add(1);
                return Err(UsbError::Overflow);
            }
            if let Err(error) = self.pause(1_000, true).await {
                self.dropped = self.dropped.saturating_add(1);
                return Err(error);
            }
        }
    }

    pub(super) async fn storage_event(&mut self, event: UsbEvent) -> Result<(), UsbError> {
        self.emit_reliable(event, Instant::now() + Duration::from_secs(2))
            .await
    }

    fn status(&mut self) {
        self.delivery.proxy.snapshot();
        if let Some(info) = self.network.and_then(Bus::network) {
            self.emit(UsbEvent::Network { info });
        }
        self.emit(UsbEvent::Lifecycle {
            state: self.state,
            session_id: self.session_id,
        });
        self.emit(UsbEvent::Status {
            enabled: self.enabled,
            attached: self.attached,
            address: self.address,
            transfers: self.transfers,
            errors: self.errors,
            dropped: self.dropped,
        });
    }

    fn lifecycle(&mut self, state: HostState) {
        self.state = state;
        self.emit(UsbEvent::Lifecycle {
            state,
            session_id: self.session_id,
        });
    }

    fn clear_transfer_context(&mut self) {
        // Transfer zero with empty SETUP is controller/idle context, not an EP0 request.
        self.transfer_id = 0;
        self.stage = TransferStage::Setup;
        self.setup = [0; 8];
        self.active_endpoint = 0;
        self.attempt_started = None;
        self.attempt_retries = 0;
        self.failure_snapshot = None;
        self.failure_emitted = false;
    }

    fn save_failure(&mut self, code: UsbError) {
        if self.failure_snapshot.is_some() {
            return;
        }
        self.failure_snapshot = Some(UsbEvent::Diagnostic {
            code,
            setup: self.setup,
            session_id: if self.attached { self.session_id } else { 0 },
            transfer_id: self.transfer_id,
            stage: self.stage,
            endpoint: self.active_endpoint,
            elapsed_us: self
                .attempt_started
                .map_or(0, |started| (Instant::now() - started).as_micros()),
            retries: self.attempt_retries,
            hcint: pac::USB_OTG_HS.hcint(0).read().0,
            hcchar: pac::USB_OTG_HS.hcchar(0).read().0,
            hctsiz: pac::USB_OTG_HS.hctsiz(0).read().0,
            hprt: pac::USB_OTG_HS.hprt().read().0,
        });
    }

    fn emit_failure(&mut self) {
        if !self.failure_emitted
            && let Some(snapshot) = self.failure_snapshot.clone()
        {
            self.emit(snapshot);
            self.failure_emitted = true;
        }
    }

    fn error(&mut self, code: UsbError) {
        self.delivery.proxy.fault(ProxyFault::Transport);
        self.save_failure(code);
        self.lifecycle(HostState::Error);
        self.emit_failure();
        self.errors = self.errors.saturating_add(1);
        self.emit(UsbEvent::Error { code });
    }

    fn handle_command(&mut self, command: HostCommand, request_processor_snapshot: bool) {
        // Enforce maintenance at execution as well as HTTP admission: CDC and
        // previously buffered commands cannot bypass the update owner's latch.
        if self.network.is_some_and(Bus::maintenance)
            && matches!(
                command,
                HostCommand::Start | HostCommand::ProxyEnable { .. } | HostCommand::Reset
            )
        {
            self.status();
            return;
        }
        match command {
            HostCommand::Start => {
                if !self.enabled {
                    self.enabled = true;
                    self.restart = true;
                }
            }
            HostCommand::ProxyEnable { generation } => {
                if generation != self.delivery.proxy.status().generation {
                    self.status();
                } else if self.state == HostState::Ready {
                    self.delivery
                        .proxy
                        .enable(generation, Instant::now().as_millis());
                } else {
                    self.delivery.proxy.fault(ProxyFault::Unsupported);
                }
            }
            HostCommand::ProxyDisable => self.delivery.proxy.disable(),
            HostCommand::Stop => {
                self.delivery.proxy.disable();
                self.enabled = false;
                self.lifecycle(HostState::Stopped);
                self.restart = true;
                // VBUS is removed immediately, even during a transfer wait.
                self.vbus.set_low();
            }
            HostCommand::Reset => {
                self.delivery.proxy.disable();
                if self.enabled {
                    self.restart = true;
                }
            }
            HostCommand::Status => {
                self.delivery.request_snapshot(request_processor_snapshot);
                self.status();
            }
            command @ (HostCommand::StorageList | HostCommand::StorageRead { .. }) => {
                if !self.enabled || !matches!(self.state, HostState::Ready | HostState::Enumerating)
                {
                    self.emit(UsbEvent::StorageError {
                        message: super::storage::ascii(
                            b"Start and enumerate a mass-storage device before requesting files",
                        ),
                    });
                } else if self.storage_commands.push_back(command).is_err() {
                    self.emit(UsbEvent::StorageError {
                        message: super::storage::ascii(
                            b"Storage command queue full; retry after the current response",
                        ),
                    });
                }
            }
        }
    }

    pub(super) fn service(&mut self) {
        self.delivery.proxy.tick(Instant::now().as_millis());
        if self.delivery.proxy.take_changed().is_some() {
            self.delivery.proxy.snapshot();
        }
        self.collect_delivery_failures();

        for _ in 0..4 {
            let Some(work) = self.command_turn.next(self.network, self.delivery) else {
                break;
            };
            match work {
                crate::ipc_delivery::Work::LocalCommand(command) => {
                    self.handle_command(command, true)
                }
                crate::ipc_delivery::Work::Incoming(message) if message.event == Event::Command => {
                    match message.decode::<HostCommand>() {
                        Ok(command) => self.handle_command(command, false),
                        Err(_) => self.delivery.note_transport_error(),
                    }
                }
                crate::ipc_delivery::Work::Incoming(message) if message.event == Event::Ping => {
                    let mut reply = message;
                    reply.event = Event::Pong;
                    if queue_capture(&self.delivery.captures, reply, &mut self.dropped).is_err() {
                        self.dropped = self.dropped.saturating_add(1);
                    }
                }
                _ => {}
            }
        }
        if Instant::now() >= self.status_at {
            self.status_at = Instant::now() + Duration::from_secs(1);
            self.status();
            if self.dropped != self.traced_dropped {
                self.traced_dropped = self.dropped;
                (self.dropped_trace)(self.dropped);
            }
        }
        self.collect_delivery_failures();
    }

    pub(super) async fn pause(
        &mut self,
        micros: u64,
        require_connection: bool,
    ) -> Result<(), UsbError> {
        let end = Instant::now() + Duration::from_micros(micros);
        loop {
            self.service();
            if !self.enabled || self.restart {
                return Err(UsbError::Controller);
            }
            if require_connection && !self.connected() {
                return Err(UsbError::Disconnected);
            }
            if require_connection && pac::USB_OTG_HS.hprt().read().0 & (1 << 4) != 0 {
                return Err(UsbError::Controller);
            }
            let now = Instant::now();
            if now >= end {
                return Ok(());
            }
            Timer::at(core::cmp::min(end, now + Duration::from_micros(100))).await;
        }
    }

    async fn wait_bits(
        &mut self,
        mut read: impl FnMut() -> u32,
        mask: u32,
        value: u32,
        ms: u64,
    ) -> Result<(), UsbError> {
        let deadline = Instant::now() + Duration::from_millis(ms);
        while read() & mask != value {
            if Instant::now() >= deadline {
                return Err(UsbError::Timeout);
            }
            self.pause(100, false).await?;
        }
        Ok(())
    }

    async fn initialize(&mut self) -> Result<(), UsbError> {
        self.clear_transfer_context();
        self.attempt_started = Some(Instant::now());
        // Boot owns USBSEL and HSI48; never modify the shared kernel selector
        // after M4 release. Only enable/reset HS, never the M4 FS controller.
        pac::RCC.ahb4enr().modify(|w| w.set_gpioben(true));
        let _ = pac::RCC.ahb4enr().read();
        pac::GPIOB
            .moder()
            .modify(|w| w.0 = (w.0 & !(15 << 28)) | (10 << 28));
        pac::GPIOB.otyper().modify(|w| w.0 &= !(3 << 14));
        pac::GPIOB.pupdr().modify(|w| w.0 &= !(15 << 28));
        pac::GPIOB.ospeedr().modify(|w| w.0 |= 15 << 28);
        pac::GPIOB
            .afr(1)
            .modify(|w| w.0 = (w.0 & !(255 << 24)) | (0xcc << 24));
        pac::PWR.cr3().modify(|w| {
            w.set_usb33den(true);
            // Board supplies VDD33USB externally, as in the locked HAL.
            w.set_usbregen(false);
        });
        let power_deadline = Instant::now() + Duration::from_millis(100);
        while !pac::PWR.cr3().read().usb33rdy() {
            if Instant::now() >= power_deadline {
                return Err(UsbError::Controller);
            }
            self.pause(100, false).await?;
        }
        pac::RCC.ahb1enr().modify(|w| {
            w.set_usb_otg_hsen(true);
            w.set_usb_otg_hs_ulpien(false);
        });
        let _ = pac::RCC.ahb1enr().read();
        pac::RCC.ahb1rstr().modify(|w| w.set_usb_otg_hsrst(true));
        cortex_m::asm::dsb();
        self.pause(10, false).await?;
        pac::RCC.ahb1rstr().modify(|w| w.set_usb_otg_hsrst(false));
        self.pause(10, false).await?;
        pac::USB_OTG_HS.gahbcfg().write_value(regs::Gahbcfg(0)); // No DMA, no global interrupt delivery.
        pac::USB_OTG_HS.gintmsk().write_value(regs::Gintmsk(0));
        pac::USB_OTG_HS.pcgcctl().write_value(regs::Pcgcctl(0)); // Ungate PHY and AHB clocks.
        pac::USB_OTG_HS
            .gusbcfg()
            .write_value(regs::Gusbcfg((1 << 6) | (6 << 10))); // Internal FS PHY; 64MHz AHB TRDT=6.
        self.pause(10, false).await?;
        self.wait_bits(|| pac::USB_OTG_HS.grstctl().read().0, 1 << 31, 1 << 31, 100)
            .await?;
        pac::USB_OTG_HS.grstctl().write_value(regs::Grstctl(1)); // Core soft reset.
        self.wait_bits(|| pac::USB_OTG_HS.grstctl().read().0, 1, 0, 100)
            .await?;
        self.wait_bits(|| pac::USB_OTG_HS.grstctl().read().0, 1 << 31, 1 << 31, 100)
            .await?;
        self.pause(10, false).await?; // At least three PHY clocks after reset.
        pac::USB_OTG_HS
            .gccfg_v2()
            .write_value(regs::GccfgV2(1 << 16)); // GCCFG v2 PWRDWN=1 powers FS PHY, VBDEN=0.
        pac::USB_OTG_HS
            .gusbcfg()
            .write_value(regs::Gusbcfg((1 << 29) | (1 << 6) | (6 << 10)));
        self.pause(50_000, false).await?; // Force-host settling interval.
        self.wait_bits(|| pac::USB_OTG_HS.gintsts().read().0, 1, 1, 100)
            .await?;
        pac::USB_OTG_HS.hcfg().write_value(regs::Hcfg(1)); // FS/LS clock select: 48MHz.
        pac::USB_OTG_HS.hfir().write_value(regs::Hfir(48_000));
        // 1024-word HS RAM: RX=256, nonperiodic TX=128, periodic TX=128.
        // Only <=64-byte packets are ever active; unused FIFO RAM is reserved.
        pac::USB_OTG_HS.grxfsiz().write_value(regs::Grxfsiz(256));
        pac::USB_OTG_HS
            .hnptxfsiz()
            .write_value(regs::Fsiz((128 << 16) | 256));
        pac::USB_OTG_HS
            .hptxfsiz()
            .write_value(regs::Fsiz((128 << 16) | 384));
        pac::USB_OTG_HS
            .grstctl()
            .write_value(regs::Grstctl((1 << 4) | (1 << 5) | (16 << 6)));
        self.wait_bits(
            || pac::USB_OTG_HS.grstctl().read().0,
            (1 << 4) | (1 << 5),
            0,
            100,
        )
        .await?;
        self.pause(10, false).await?;
        for channel in 0..12 {
            pac::USB_OTG_HS.hcchar(channel).write_value(regs::Hcchar(0));
            pac::USB_OTG_HS
                .hcint(channel)
                .write_value(regs::Hcint(0x7ff));
            pac::USB_OTG_HS
                .hcintmsk(channel)
                .write_value(regs::Hcintmsk(0));
        }
        pac::USB_OTG_HS
            .gintsts()
            .write_value(regs::Gintsts(0xffff_ffff));
        self.port(0, 1 << 12);
        self.vbus.set_high(); // PA15 active-high USB-A power switch.
        self.pause(200_000, false).await?;
        self.emit(UsbEvent::Ready);
        Ok(())
    }

    async fn reset_port(&mut self) -> Result<(), UsbError> {
        self.clear_transfer_context();
        self.attempt_started = Some(Instant::now());
        self.address = 0;
        self.mps = 8;
        pac::USB_OTG_HS.hcfg().write_value(regs::Hcfg(1));
        pac::USB_OTG_HS.hfir().write_value(regs::Hfir(48_000));
        // Clear stale RX statuses/data from a previous disconnected device.
        pac::USB_OTG_HS
            .grstctl()
            .write_value(regs::Grstctl((1 << 4) | (1 << 5) | (16 << 6)));
        self.wait_bits(
            || pac::USB_OTG_HS.grstctl().read().0,
            (1 << 4) | (1 << 5),
            0,
            100,
        )
        .await?;
        self.pause(10, true).await?;
        self.port(0, 1 << 8);
        let result = self.pause(50_000, true).await;
        if let Err(code) = result {
            self.save_failure(code);
        }
        self.port(1 << 8, 0); // Always release reset, including cancellation.
        result?;
        self.pause(20_000, true).await?;
        let deadline = Instant::now() + Duration::from_millis(100);
        while pac::USB_OTG_HS.hprt().read().0 & 4 == 0 {
            if Instant::now() >= deadline {
                return Err(UsbError::Timeout);
            }
            self.pause(100, true).await?;
        }
        self.low = match (pac::USB_OTG_HS.hprt().read().0 >> 17) & 3 {
            1 => false,
            2 => true,
            _ => return Err(UsbError::Unsupported),
        };
        pac::USB_OTG_HS
            .hcfg()
            .write_value(regs::Hcfg(if self.low { 2 } else { 1 }));
        pac::USB_OTG_HS
            .hfir()
            .write_value(regs::Hfir(if self.low { 6_000 } else { 48_000 }));
        self.emit(UsbEvent::PortReset);
        Ok(())
    }

    fn channel_snapshot(&self, stage: &str) {
        shared::logging::warn!(
            "USB {} HCCHAR={:08x} HCINT={:08x} HCTSIZ={:08x} HNPTXSTS={:08x} HPTXSTS={:08x} GINT={:08x} HPRT={:08x} GAHBCFG={:08x} GINTMSK={:08x} HCINTMSK={:08x}",
            stage,
            pac::USB_OTG_HS.hcchar(0).read().0,
            pac::USB_OTG_HS.hcint(0).read().0,
            pac::USB_OTG_HS.hctsiz(0).read().0,
            pac::USB_OTG_HS.hnptxsts().read().0,
            pac::USB_OTG_HS.hptxsts().read().0,
            pac::USB_OTG_HS.gintsts().read().0,
            pac::USB_OTG_HS.hprt().read().0,
            pac::USB_OTG_HS.gahbcfg().read().0,
            pac::USB_OTG_HS.gintmsk().read().0,
            pac::USB_OTG_HS.hcintmsk(0).read().0,
        );
    }

    // Cleanup only: packet() has already consumed its result or abandoned it.
    // Pop statuses and discard payload so a full RX FIFO cannot stall a halt.
    fn drain_abandoned_rx(&self) {
        for _ in 0..16 {
            if pac::USB_OTG_HS.gintsts().read().0 & RX_LEVEL == 0 {
                break;
            }
            let status = pac::USB_OTG_HS.grxstsp().read().0;
            if (status >> 17) & 15 == 2 {
                let count = (status >> 4) & 0x7ff;
                for _ in 0..count.div_ceil(4) {
                    let _ = pac::USB_OTG_HS.fifo(0).read().0;
                }
            }
        }
    }

    async fn halt(&mut self) -> Result<(), UsbError> {
        let end = Instant::now() + Duration::from_millis(10);
        let mut requested = false;
        loop {
            self.service();
            self.drain_abandoned_rx();
            let characteristics = pac::USB_OTG_HS.hcchar(0).read().0;
            let port = pac::USB_OTG_HS.hprt().read().0;
            if let Some(outcome) = halt_outcome(
                characteristics,
                port,
                pac::USB_OTG_HS.gintsts().read().0,
                Instant::now() >= end,
            ) {
                if outcome == Err(UsbError::Controller) {
                    self.channel_snapshot(if port & (1 << 4) != 0 {
                        "halt overcurrent"
                    } else if requested {
                        "halt timeout"
                    } else {
                        "halt queue timeout"
                    });
                }
                return outcome;
            }
            // DWC2 slave-mode disable consumes a request queue entry. As in
            // TinyUSB channel_disable(), wait for the appropriate queue first,
            // but keep draining RX and yielding rather than blocking the core.
            let periodic = matches!((characteristics >> 18) & 3, 1 | 3);
            let queue = if periodic {
                pac::USB_OTG_HS.hptxsts().read().0
            } else {
                pac::USB_OTG_HS.hnptxsts().read().0
            };
            if !requested && (queue >> 16) & 0xff != 0 {
                pac::USB_OTG_HS
                    .hcchar(0)
                    .write_value(regs::Hcchar(characteristics | ENABLE | HALT));
                requested = true;
            }
            // Cleanup must also work after Stop/Reset: do not use pause.
            Timer::after_micros(100).await;
        }
    }

    async fn prepare_out(&mut self, length: usize, deadline: Instant) -> Result<(), UsbError> {
        // Only channel zero is used, and the caller has confirmed CHENA=0.
        // Flush TX FIFO zero before reloading: a cancelled/NAKed OUT may leave
        // unsent words behind. Never flush an active channel's TX FIFO.
        pac::USB_OTG_HS.grstctl().write_value(regs::Grstctl(1 << 5));
        while pac::USB_OTG_HS.grstctl().read().0 & (1 << 5) != 0 {
            if Instant::now() >= deadline {
                return Err(UsbError::Timeout);
            }
            self.pause(100, true).await?;
        }
        self.pause(10, true).await?; // At least three PHY clocks after TX flush.
        let words = length.div_ceil(4) as u32;
        loop {
            if Instant::now() >= deadline {
                return Err(UsbError::Timeout);
            }
            let space = pac::USB_OTG_HS.hnptxsts().read().0; // HNPTXSTS: FIFO words and request-queue slots.
            if space & 0xffff >= words && (space >> 16) & 0xff != 0 {
                return Ok(());
            }
            self.pause(100, true).await?;
        }
    }

    /// Exactly one packet per activation. PID only changes after successful ACK.
    /// NAK retries preserve PID and share the caller's absolute deadline.
    async fn packet(
        &mut self,
        packet: Packet,
        out: &[u8],
        input: &mut [u8],
        deadline: Instant,
    ) -> Result<usize, UsbError> {
        if !packet.valid_buffers(out.len(), input.len()) {
            return Err(UsbError::Unsupported);
        }
        // Only throttle at a packet-safe boundary. Never extend the transfer's
        // absolute deadline or wait indefinitely for a stopped M4. Reserve room
        // for context/payload without delaying an active channel's RX draining.
        let drain_until =
            crate::ipc_delivery::drain_deadline(Instant::now().as_micros(), deadline.as_micros());
        while crate::ipc_delivery::should_drain(
            self.delivery.captures.len(),
            Instant::now().as_micros(),
            drain_until,
        ) {
            self.pause(100, true).await?;
        }
        self.active_endpoint = packet.endpoint;
        self.attempt_started = Some(Instant::now());
        self.attempt_retries = 0;
        self.failure_snapshot = None;
        self.failure_emitted = false;
        let result = self.packet_attempts(packet, out, input, deadline).await;
        if let Err(code) = result
            && code != UsbError::Nak
            && self.enabled
            && !self.restart
            && !self.proxy_transfer_cancelled()
        {
            self.save_failure(code);
            // Cleanup can fail after an otherwise recoverable STALL/NAK. Keep
            // the pre-cleanup registers, but report the error actually returned.
            let elapsed = self
                .attempt_started
                .map_or(0, |started| (Instant::now() - started).as_micros());
            if let Some(UsbEvent::Diagnostic {
                code: saved_code,
                elapsed_us,
                retries,
                ..
            }) = &mut self.failure_snapshot
            {
                *saved_code = code;
                *elapsed_us = elapsed;
                *retries = self.attempt_retries;
            }
            if code != UsbError::Stall {
                self.emit_failure();
            }
        }
        result
    }

    async fn packet_attempts(
        &mut self,
        packet: Packet,
        out: &[u8],
        input: &mut [u8],
        deadline: Instant,
    ) -> Result<usize, UsbError> {
        let Packet { endpoint, kind, .. } = packet;
        let incoming = endpoint & 0x80 != 0;

        if kind != 0 {
            self.setup = [0; 8];
            self.transfer_sequence = self.transfer_sequence.wrapping_add(1).max(1);
            self.transfer_id = self.transfer_sequence;
            self.stage = TransferStage::Data;
        }
        loop {
            self.service();
            if !self.enabled || self.restart || self.proxy_transfer_cancelled() {
                return Err(UsbError::Controller);
            }
            if !self.connected() {
                return Err(UsbError::Disconnected);
            }
            if pac::USB_OTG_HS.hprt().read().0 & (1 << 4) != 0 {
                return Err(UsbError::Controller);
            }
            if Instant::now() >= deadline {
                return Err(UsbError::Timeout);
            }
            self.halt().await?;
            if !self.enabled || self.restart || self.proxy_transfer_cancelled() {
                return Err(UsbError::Controller);
            }
            if !self.connected() {
                return Err(UsbError::Disconnected);
            }
            if !incoming {
                self.prepare_out(out.len(), deadline).await?;
            }
            if self.proxy_transfer_cancelled() {
                return Err(UsbError::Controller);
            }
            self.failure_snapshot = None;
            pac::USB_OTG_HS.hcint(0).write_value(regs::Hcint(0x7ff));
            pac::USB_OTG_HS
                .hctsiz(0)
                .write_value(regs::Hctsiz(packet.transfer_size(out.len())));
            // ST's USB_HC_Init sets LSDEV only for LS behind a faster port,
            // not for the directly attached root device supported here.
            let characteristics = root_channel_characteristics(
                packet,
                self.address,
                pac::USB_OTG_HS.hfnum().read().0,
            );
            pac::USB_OTG_HS
                .hcchar(0)
                .write_value(regs::Hcchar(characteristics | ENABLE));
            if !incoming {
                for chunk in out.chunks(4) {
                    let mut word = [0; 4];
                    word[..chunk.len()].copy_from_slice(chunk);
                    pac::USB_OTG_HS
                        .fifo(0)
                        .write_value(regs::Fifo(u32::from_le_bytes(word)));
                }
            }
            let mut received = 0;
            let mut overflow = false;
            let mut saw_data = false;
            let outcome = loop {
                self.service();
                if !self.enabled || self.restart || self.proxy_transfer_cancelled() {
                    break Err(UsbError::Controller);
                }
                if !self.connected() {
                    break Err(UsbError::Disconnected);
                }
                if pac::USB_OTG_HS.hprt().read().0 & (1 << 4) != 0 {
                    self.vbus.set_low();
                    break Err(UsbError::Controller);
                }
                // RX status must be popped, including zero-byte completion and
                // channel-halted entries. Drain data even if the user buffer is short.
                for _ in 0..16 {
                    if pac::USB_OTG_HS.gintsts().read().0 & RX_LEVEL == 0 {
                        break;
                    }
                    let status = pac::USB_OTG_HS.grxstsp().read().0;
                    let count = ((status >> 4) & 0x7ff) as usize;
                    let packet_status = (status >> 17) & 15;
                    if packet_status == 2 {
                        if status & 15 != 0 || !incoming || saw_data {
                            overflow = true;
                        }
                        saw_data = true;
                        for start in (0..count).step_by(4) {
                            let word = pac::USB_OTG_HS.fifo(0).read().0.to_le_bytes();
                            for byte in &word[..core::cmp::min(4, count - start)] {
                                if received < input.len() {
                                    input[received] = *byte;
                                } else {
                                    overflow = true;
                                }
                                received += 1;
                            }
                        }
                    }
                }
                let flags = pac::USB_OTG_HS.hcint(0).read().0;
                match channel_outcome(flags, pac::USB_OTG_HS.gintsts().read().0 & RX_LEVEL != 0) {
                    Some(Err(error)) => break Err(error),
                    Some(Ok(())) => {
                        break if overflow {
                            Err(UsbError::Overflow)
                        } else if incoming && !saw_data {
                            Err(UsbError::Transaction)
                        } else {
                            Ok(if incoming { received } else { out.len() })
                        };
                    }
                    None => {}
                }
                if Instant::now() >= deadline {
                    break Err(UsbError::Timeout);
                }
                Timer::after_micros(100).await;
            };
            if outcome == Err(UsbError::Nak) && kind != 3 {
                self.attempt_retries = self.attempt_retries.saturating_add(1);
            }
            // Preserve registers even if halt itself fails or the final NAK
            // exhausts the deadline. Only terminal outcomes publish diagnostics.
            self.save_failure(outcome.err().unwrap_or(UsbError::Controller));
            if let Err(error) = self.halt().await {
                shared::logging::warn!(
                    "USB packet halt failed endpoint={:02x} original outcome={:?}",
                    endpoint,
                    outcome,
                );
                return Err(error);
            }
            pac::USB_OTG_HS.hcint(0).write_value(regs::Hcint(0x7ff));
            match outcome {
                Err(UsbError::Nak) if kind != 3 => {
                    if Instant::now() >= deadline {
                        return Err(UsbError::Timeout);
                    }
                    self.pause(1_000, true).await?;
                }
                Ok(count) => {
                    self.failure_snapshot = None;
                    self.transfers = self.transfers.saturating_add(1);
                    self.emit(UsbEvent::TransferContext {
                        transfer_id: self.transfer_id,
                        session_id: self.session_id,
                        endpoint,
                        kind: match kind {
                            2 => TransferKind::Bulk,
                            3 => TransferKind::Interrupt,
                            _ => TransferKind::Control,
                        },
                        stage: self.stage,
                        elapsed_us: self
                            .attempt_started
                            .map_or(0, |started| (Instant::now() - started).as_micros()),
                        retries: self.attempt_retries,
                    });
                    self.emit(UsbEvent::Transfer {
                        endpoint,
                        direction: if incoming {
                            Direction::In
                        } else {
                            Direction::Out
                        },
                        bytes: count as u16,
                        pid: packet.pid as u8,
                        // Supported EP0 and boot-HID packets are at most 64 bytes.
                        data: Vec::from_slice(if incoming { &input[..count] } else { out })
                            .expect("host packet exceeds capture capacity"),
                    });
                    return Ok(count);
                }
                other => return other,
            }
        }
    }

    pub(super) async fn control(
        &mut self,
        setup: [u8; 8],
        data: &mut [u8],
    ) -> Result<usize, UsbError> {
        self.failure_snapshot = None;
        self.failure_emitted = false;
        self.active_endpoint = 0;
        self.setup = setup;
        self.attempt_started = Some(Instant::now());
        self.attempt_retries = 0;
        self.transfer_sequence = self.transfer_sequence.wrapping_add(1).max(1);
        self.transfer_id = self.transfer_sequence;
        self.stage = TransferStage::Setup;
        let length = usize::from(u16::from_le_bytes([setup[6], setup[7]]));
        if length > data.len() || (length > 0 && setup[0] & 0x80 == 0) {
            return Err(UsbError::Unsupported); // Only IN data / no-data requests needed here.
        }
        let deadline = Instant::now() + Duration::from_millis(1000);
        self.packet(Packet::control(0, 3, self.mps), &setup, &mut [], deadline)
            .await?;
        let mut total = 0;
        let mut pid = 2; // DATA1 after SETUP; DPID encoding DATA0=0, DATA1=2.
        self.stage = TransferStage::Data;
        while total < length {
            let end = core::cmp::min(length, total + usize::from(self.mps));
            let count = self
                .packet(
                    Packet::control(0x80, pid, self.mps),
                    &[],
                    &mut data[total..end],
                    deadline,
                )
                .await?;
            total += count;
            pid ^= 2;
            if count < usize::from(self.mps) {
                break;
            }
        }
        self.stage = TransferStage::Status;
        if setup[0] & 0x80 != 0 {
            self.packet(Packet::control(0, 2, self.mps), &[], &mut [], deadline)
                .await?;
        } else {
            let count = self
                .packet(Packet::control(0x80, 2, self.mps), &[], &mut [], deadline)
                .await?;
            if count != 0 {
                return Err(UsbError::Transaction);
            }
        }
        Ok(total)
    }

    fn proxy_transfer_cancelled(&self) -> bool {
        self.proxy_transfer_generation.is_some_and(|generation| {
            let status = self.delivery.proxy.status();
            !status.enabled || !status.ready || status.generation != generation
        })
    }

    /// Host-to-device class writes, including keyboard LED SET_REPORT. SETUP is
    /// always SETUP PID, the first data packet DATA1, and status is DATA1 IN.
    async fn control_out(&mut self, setup: [u8; 8], data: &[u8]) -> Result<usize, UsbError> {
        let packets = super::host_logic::ControlOut::new(setup, data.len(), self.mps)?;
        self.clear_transfer_context();
        self.setup = setup;
        self.transfer_sequence = self.transfer_sequence.wrapping_add(1).max(1);
        self.transfer_id = self.transfer_sequence;
        self.stage = TransferStage::Setup;
        let deadline = Instant::now() + Duration::from_millis(1000);
        self.packet(Packet::control(0, 3, self.mps), &setup, &mut [], deadline)
            .await?;
        self.stage = TransferStage::Data;
        for (range, pid) in packets {
            let count = self
                .packet(
                    Packet::control(0, pid, self.mps),
                    &data[range.clone()],
                    &mut [],
                    deadline,
                )
                .await?;
            if count != range.len() {
                return Err(UsbError::Transaction);
            }
        }
        self.stage = TransferStage::Status;
        let count = self
            .packet(Packet::control(0x80, 2, self.mps), &[], &mut [], deadline)
            .await?;
        if count != 0 {
            return Err(UsbError::Transaction);
        }
        Ok(data.len())
    }

    async fn descriptor(
        &mut self,
        kind: u8,
        length: usize,
        data: &mut [u8],
    ) -> Result<usize, UsbError> {
        let len = (length as u16).to_le_bytes();
        self.control([0x80, 6, 0, kind, 0, 0, len[0], len[1]], data)
            .await
    }

    async fn capture_descriptor(
        &mut self,
        kind: u8,
        interface: u8,
        bytes: &[u8],
    ) -> Result<(), UsbError> {
        let deadline = Instant::now() + Duration::from_secs(2);
        for (index, chunk) in bytes.chunks(DESCRIPTOR_CHUNK_SIZE).enumerate() {
            self.emit_reliable(
                UsbEvent::Descriptor {
                    descriptor_type: kind,
                    index: interface,
                    offset: (index * DESCRIPTOR_CHUNK_SIZE) as u16,
                    total_length: bytes.len() as u16,
                    data: Vec::from_slice(chunk).expect("bounded descriptor chunk"),
                },
                deadline,
            )
            .await?;
        }
        Ok(())
    }

    pub(super) async fn enumerate(&mut self) -> Result<SelectedConfig, UsbError> {
        let mut device = [0; 18];
        if self.descriptor(1, 8, &mut device).await? != 8
            || device[0] != 18
            || device[1] != 1
            || !matches!(device[7], 8 | 16 | 32 | 64)
            || (self.low && device[7] != 8)
        {
            return Err(UsbError::InvalidDescriptor);
        }
        self.mps = u16::from(device[7]);
        self.control([0, 5, 1, 0, 0, 0, 0, 0], &mut []).await?;
        self.pause(2_000, true).await?;
        self.address = 1;
        if self.descriptor(1, 18, &mut device).await? != 18
            || device[0] != 18
            || device[1] != 1
            || u16::from(device[7]) != self.mps
            || device[17] == 0
        {
            return Err(UsbError::InvalidDescriptor);
        }
        self.capture_descriptor(1, 0, &device).await?;
        if device[4] == 9 {
            return Err(UsbError::Unsupported);
        } // Root hubs unsupported.
        let mut config = [0; MAX_DESCRIPTOR_SIZE];
        if self.descriptor(2, 9, &mut config).await? != 9 || config[0] != 9 || config[1] != 2 {
            return Err(UsbError::InvalidDescriptor);
        }
        let length = usize::from(u16::from_le_bytes([config[2], config[3]]));
        if !(9..=MAX_DESCRIPTOR_SIZE).contains(&length) {
            return Err(UsbError::InvalidDescriptor);
        }
        if self.descriptor(2, length, &mut config).await? != length {
            return Err(UsbError::InvalidDescriptor);
        }
        self.capture_descriptor(2, 0, &config[..length]).await?;
        let selected = select_configuration(&config[..length], self.low)?;
        if selected.hid.is_empty() && selected.storage.is_none() {
            return Err(UsbError::Unsupported);
        }
        self.control([0, 9, selected.configuration, 0, 0, 0, 0, 0], &mut [])
            .await?;
        for hid in &selected.hid {
            if hid.boot {
                self.control([0x21, 11, 0, 0, hid.interface, 0, 0, 0], &mut [])
                    .await?;
            } else {
                let len = hid.report_length.to_le_bytes();
                let count = self
                    .control(
                        [0x81, 6, 0, 0x22, hid.interface, 0, len[0], len[1]],
                        &mut config,
                    )
                    .await?;
                if count != usize::from(hid.report_length) {
                    return Err(UsbError::InvalidDescriptor);
                }
                self.capture_descriptor(0x22, hid.interface, &config[..count])
                    .await?;
            }
            match self
                .control([0x21, 10, 0, 0, hid.interface, 0, 0, 0], &mut [])
                .await
            {
                Ok(_) | Err(UsbError::Stall) => {
                    self.clear_transfer_context();
                }
                Err(error) => return Err(error),
            }
        }
        self.emit_reliable(
            UsbEvent::Configured {
                address: 1,
                configuration: selected.configuration,
            },
            Instant::now() + Duration::from_secs(2),
        )
        .await?;
        let keyboard = selected.proxy_support(1);
        let mouse = selected.proxy_support(2);
        self.proxy_keyboard = if keyboard == ProxySupport::Boot {
            selected
                .hid
                .iter()
                .find(|ep| ep.boot && ep.protocol == 1)
                .map(|ep| ep.interface)
        } else {
            None
        };
        self.delivery.proxy.support(keyboard, mouse);
        self.delivery
            .proxy
            .support_notice(selected.hid.iter().filter(|ep| !ep.boot).count() as u8);
        Ok(selected)
    }

    async fn capture(&mut self, selected: SelectedConfig) -> Result<(), UsbError> {
        let mut storage = if let Some(endpoint) = selected.storage {
            match super::host_storage::Storage::initialize(self, endpoint).await {
                Ok(storage) => Some(storage),
                Err(error) => {
                    super::host_storage::storage_error(
                        self,
                        "MSC initialization failed: unsupported device, capacity, or SCSI response",
                    )
                    .await?;
                    if matches!(error, UsbError::Disconnected | UsbError::Controller) {
                        return Err(error);
                    }
                    None
                }
            }
        } else {
            None
        };
        let mut next = [Instant::now(); 4];
        let mut toggles = [0; 4];
        let mut recoveries = [0u8; 4];
        let mut buffer = [0; 64];
        loop {
            if let Some((generation, bits)) = self.delivery.proxy.take_leds()
                && let Some(interface) = self.proxy_keyboard
                && self.delivery.proxy.status().ready
            {
                self.proxy_transfer_generation = Some(generation);
                let result = self
                    .control_out(super::host_logic::led_setup(interface), &[bits])
                    .await;
                self.proxy_transfer_generation = None;
                let current = self.delivery.proxy.status().generation == generation;
                // Cancellation/session changes cannot attribute an old LED failure
                // to a newly enabled generation.
                if result.is_err() && current {
                    self.emit_failure();
                    self.delivery.proxy.led_failed();
                }
                if result == Err(UsbError::Disconnected)
                    || (current && result == Err(UsbError::Controller))
                {
                    result?;
                }
            }
            if let Some(command) = self.storage_commands.pop_front() {
                if let Some(storage) = &mut storage {
                    storage.handle(self, command).await?;
                } else {
                    super::host_storage::storage_error(
                        self,
                        "No initialized mass-storage device is attached",
                    )
                    .await?;
                }
            }
            for (i, endpoint) in selected.hid.iter().enumerate() {
                if Instant::now() < next[i] {
                    continue;
                }
                next[i] = Instant::now() + Duration::from_millis(u64::from(endpoint.interval));
                let deadline = Instant::now() + Duration::from_millis(20);
                let proxy = self.delivery.proxy.status();
                match self
                    .packet(
                        Packet {
                            endpoint: endpoint.endpoint,
                            kind: 3,
                            pid: toggles[i],
                            mps: endpoint.packet,
                        },
                        &[],
                        &mut buffer[..usize::from(endpoint.packet)],
                        deadline,
                    )
                    .await
                {
                    Ok(count) => {
                        toggles[i] ^= 2;
                        if endpoint.boot
                            && proxy.ready
                            && self.delivery.proxy.status().generation == proxy.generation
                        {
                            let kind = if endpoint.protocol == 1 {
                                BootKind::Keyboard
                            } else {
                                BootKind::Mouse
                            };
                            self.delivery.proxy.report(
                                kind,
                                &buffer[..count],
                                Instant::now().as_millis(),
                            );
                        }
                        self.emit(UsbEvent::HidReport {
                            endpoint: endpoint.endpoint,
                            data: Vec::from_slice(&buffer[..count])
                                .map_err(|_| UsbError::Overflow)?,
                        });
                    }
                    Err(UsbError::Nak) => {}
                    Err(UsbError::Stall) if recoveries[i] < 2 => {
                        recoveries[i] += 1;
                        self.clear_endpoint_halt(endpoint.endpoint).await?;
                        toggles[i] = 0;
                    }
                    Err(error) => return Err(error),
                }
            }
            self.clear_transfer_context();
            self.pause(100, true).await?;
        }
    }

    pub(super) async fn clear_endpoint_halt(&mut self, endpoint: u8) -> Result<(), UsbError> {
        self.control([2, 1, 0, 0, endpoint, 0, 0, 0], &mut [])
            .await
            .map(|_| ())
    }

    pub(super) async fn bulk_in(
        &mut self,
        endpoint: u8,
        mps: u16,
        toggle: &mut bool,
        data: &mut [u8],
    ) -> Result<usize, UsbError> {
        self.bulk_in_counted(endpoint, mps, toggle, data)
            .await
            .map_err(|(error, _)| error)
    }

    /// On failure, returns the error and bytes already received in this call.
    /// Successful packets still advance the caller's toggle before a later STALL.
    pub(super) async fn bulk_in_counted(
        &mut self,
        endpoint: u8,
        mps: u16,
        toggle: &mut bool,
        data: &mut [u8],
    ) -> Result<usize, (UsbError, usize)> {
        self.clear_transfer_context();
        self.active_endpoint = endpoint;
        if self.low
            || endpoint & 0x80 == 0
            || endpoint & 0x70 != 0
            || endpoint & 15 == 0
            || mps == 0
            || mps > 64
        {
            return Err((UsbError::Unsupported, 0));
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut total = 0;
        let mut packet_data = [0; 64];
        while total < data.len() {
            let count = self
                .packet(
                    Packet {
                        endpoint,
                        kind: 2,
                        pid: if *toggle { 2 } else { 0 },
                        mps,
                    },
                    &[],
                    &mut packet_data[..usize::from(mps)],
                    deadline,
                )
                .await
                .map_err(|error| (error, total))?;
            *toggle = !*toggle;
            if count > data.len() - total {
                return Err((UsbError::Overflow, total + count));
            }
            data[total..total + count].copy_from_slice(&packet_data[..count]);
            total += count;
            if count < usize::from(mps) {
                break;
            }
        }
        Ok(total)
    }

    pub(super) async fn bulk_out(
        &mut self,
        endpoint: u8,
        mps: u16,
        toggle: &mut bool,
        data: &[u8],
    ) -> Result<usize, UsbError> {
        self.clear_transfer_context();
        self.active_endpoint = endpoint;
        if self.low || endpoint & 0xf0 != 0 || endpoint & 15 == 0 || mps == 0 || mps > 64 {
            return Err(UsbError::Unsupported);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut total = 0;
        if data.is_empty() {
            self.packet(
                Packet {
                    endpoint,
                    kind: 2,
                    pid: if *toggle { 2 } else { 0 },
                    mps,
                },
                &[],
                &mut [],
                deadline,
            )
            .await?;
            *toggle = !*toggle;
            return Ok(0);
        }
        for chunk in data.chunks(usize::from(mps)) {
            total += self
                .packet(
                    Packet {
                        endpoint,
                        kind: 2,
                        pid: if *toggle { 2 } else { 0 },
                        mps,
                    },
                    chunk,
                    &mut [],
                    deadline,
                )
                .await?;
            *toggle = !*toggle;
        }
        Ok(total)
    }

    fn detached(&mut self) {
        if self.attached {
            self.delivery.proxy.session(self.session_id);
            self.proxy_keyboard = None;
        }
        self.clear_transfer_context();
        self.storage_commands.clear();
        if self.attached {
            self.emit(UsbEvent::Detached);
        }
        self.attached = false;
        self.address = 0;
        self.mps = 8;
        self.lifecycle(if self.enabled {
            HostState::Waiting
        } else {
            HostState::Stopped
        });
    }

    /// Runs forever; callback reports cumulative capture loss without requiring
    /// a logging dependency in shared. Status also exposes this count each second.
    pub async fn run(&mut self, dropped_trace: fn(u32)) -> ! {
        self.dropped_trace = dropped_trace;
        let mut initialized = false;
        let mut failed_device = false;
        loop {
            self.clear_transfer_context();
            self.service();
            if self.restart || (!self.enabled && initialized) {
                // Never reuse a possibly unhalted channel: the next initialize
                // asserts the HS RCC reset. VBUS off also clears device address.
                if initialized {
                    let _ = self.halt().await;
                    self.port(1 << 12, 0);
                }
                self.vbus.set_low();
                // An error is not a detach: preserve the partial topology until
                // an explicit lifecycle command or a real disconnect.
                if self.restart {
                    self.detached();
                }
                initialized = false;
                failed_device = false;
                self.restart = false;
                self.status();
                // Discharge/recovery time also applies to an explicit Reset.
                Timer::after_millis(100).await;
            }
            if !self.enabled {
                Timer::after_millis(1).await;
                continue;
            }
            if !initialized {
                match self.initialize().await {
                    Ok(()) => {
                        initialized = true;
                        self.lifecycle(HostState::Waiting);
                    }
                    Err(error) => {
                        self.vbus.set_low();
                        if !self.restart && self.enabled {
                            self.error(error);
                            self.enabled = false;
                        }
                        continue;
                    }
                }
            }
            let port = pac::USB_OTG_HS.hprt().read().0;
            if port & (1 << 4) != 0 {
                self.error(UsbError::Controller);
                self.enabled = false;
                self.vbus.set_low();
                continue;
            }
            // Acknowledge only observed W1C changes, never PENA.
            pac::USB_OTG_HS.hprt().write_value(regs::Hprt(
                (port & !(PORT_CHANGES | 4)) | (port & PORT_CHANGES),
            ));
            if !self.connected() {
                match self.halt().await {
                    Ok(()) => {}
                    Err(UsbError::Disconnected) => {
                        // The port vanished with a live channel. initialize()
                        // resets HS before any channel registers can be reused.
                        self.port(1 << 12, 0);
                        self.vbus.set_low();
                        initialized = false;
                        Timer::after_millis(100).await;
                    }
                    Err(error) => {
                        self.error(error);
                        self.enabled = false;
                        self.vbus.set_low();
                        continue;
                    }
                }
                self.detached();
                failed_device = false;
                pac::USB_OTG_HS
                    .gintsts()
                    .write_value(regs::Gintsts(1 << 29)); // Disconnect W1C after observing it.
            } else if !self.attached && !failed_device {
                // Require 100ms continuously connected before reset. A bounce
                // aborts this wait and starts a fresh interval on the next pass.
                match self.pause(100_000, true).await {
                    Ok(()) => {
                        self.low = (pac::USB_OTG_HS.hprt().read().0 >> 17) & 3 == 2;
                        self.attached = true;
                        self.session_id = self.session_id.wrapping_add(1).max(1);
                        self.delivery.proxy.session(self.session_id);
                        self.clear_transfer_context();
                        self.lifecycle(HostState::Attached);
                        self.emit(UsbEvent::Attached {
                            speed: if self.low {
                                UsbSpeed::Low
                            } else {
                                UsbSpeed::Full
                            },
                        });
                        let result = async {
                            self.lifecycle(HostState::Enumerating);
                            self.reset_port().await?;
                            let selected = self.enumerate().await?;
                            self.lifecycle(HostState::Ready);
                            self.capture(selected).await
                        }
                        .await;
                        if let Err(error) = result
                            && !self.restart
                            && self.enabled
                        {
                            if error == UsbError::Disconnected {
                                self.detached();
                            } else {
                                self.error(error);
                                // Controller-level timeout may leave a channel
                                // wedged: require Start/Reset, don't reuse it.
                                if error == UsbError::Controller {
                                    self.enabled = false;
                                }
                                failed_device = true;
                            }
                        }
                    }
                    Err(UsbError::Disconnected) => {}
                    Err(error) => {
                        if !self.restart && self.enabled {
                            self.error(error);
                        }
                    }
                }
            }
            Timer::after_millis(1).await;
        }
    }
}
