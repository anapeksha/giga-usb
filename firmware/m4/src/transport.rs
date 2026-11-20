//! USB-C device diagnostics. The development VID/PID is not a production VID
//! allocation. No VBUS sense pin is claimed: the self-powered board uses the
//! HAL's vbus_detect=false mode. M7 owns the USB clock mux, M4 owns OTG_FS.
use core::{cell::RefCell, mem::take};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_stm32::{
    Peripherals, bind_interrupts,
    peripherals::USB_OTG_FS,
    usb::{Config as OtgConfig, Driver, InterruptHandler},
};
use embassy_time::{Instant, Timer};
use embassy_usb::{
    Builder, Config, UsbDevice,
    class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender, State},
    driver::EndpointError,
};
use heapless::{Deque, String, Vec};
use shared::{
    protocol::{Event, Message, RECORD_BYTES},
    usb::types::*,
};
use static_cell::StaticCell;

type UsbDriver = Driver<'static, USB_OTG_FS>;
bind_interrupts!(struct Irqs { OTG_FS => InterruptHandler<USB_OTG_FS>; });

struct Queues {
    output: Deque<UiRecord, 64>,
    network: Deque<UiRecord, 64>,
    network_json: Vec<u8, { RECORD_BYTES }>,
    network_offset: usize,
    network_dropped: u32,
    input: Deque<HostCommand, 4>,
    connected: bool,
    replay: bool,
    generation: u32,
    dropped: u32,
}
fn raw_trace(record: &UiRecord) -> bool {
    matches!(
        &record.event,
        UiEvent::Host {
            event: UsbEvent::Transfer { .. } | UsbEvent::TransferContext { .. }
        }
    )
}

/// Returns the number of discarded records, whether evicted or not admitted.
fn queue_record(queue: &mut Deque<UiRecord, 64>, record: UiRecord) -> u32 {
    if !queue.is_full() {
        let _ = queue.push_back(record);
        return 0;
    }
    if !raw_trace(&record)
        && let Some(oldest_trace) = queue.iter().position(raw_trace)
    {
        // Rotate exactly once, omitting only the oldest raw trace. All retained
        // records keep their relative order, and the new semantic record is last.
        for index in 0..queue.len() {
            if let Some(queued) = queue.pop_front()
                && index != oldest_trace
            {
                let _ = queue.push_back(queued);
            }
        }
        let _ = queue.push_back(record);
    }
    // A full all-semantic queue keeps its existing records and drops the new one.
    1
}

// All tasks run cooperatively on the M4 executor. No borrow crosses an await.
pub struct Bus(RefCell<Queues>);
impl Bus {
    pub const fn new() -> Self {
        Self(RefCell::new(Queues {
            output: Deque::new(),
            network: Deque::new(),
            network_json: Vec::new(),
            network_offset: 0,
            network_dropped: 0,
            input: Deque::new(),
            connected: false,
            replay: false,
            generation: 0,
            dropped: 0,
        }))
    }
    pub fn emit(&self, timestamp_ms: u64, event: UiEvent) {
        let mut q = self.0.borrow_mut();

        let record = UiRecord {
            timestamp_ms,
            event: event.clone(),
        };
        let dropped = queue_record(&mut q.network, record);
        q.network_dropped = q.network_dropped.saturating_add(dropped);
        if !q.connected {
            return;
        }
        let dropped = queue_record(
            &mut q.output,
            UiRecord {
                timestamp_ms,
                event,
            },
        );
        q.dropped = q.dropped.saturating_add(dropped);
    }
    pub fn error(&self, text: &str) {
        let mut message = String::new();
        let _ = message.push_str(text);
        self.emit(Instant::now().as_millis(), UiEvent::Error { message });
    }
    pub fn command(&self) -> Option<HostCommand> {
        self.0.borrow_mut().input.pop_front()
    }
    pub fn take_replay(&self) -> bool {
        take(&mut self.0.borrow_mut().replay)
    }
    pub fn accept_command(&self, command: HostCommand) {
        if self.0.borrow_mut().input.push_back(command).is_err() {
            self.error("Busy: command queue full");
        }
    }
    pub fn network_frame(&self) -> Option<Message> {
        let mut q = self.0.borrow_mut();
        if q.network_json.is_empty() {
            let record = if q.network_dropped != 0 {
                let count = take(&mut q.network_dropped);
                UiRecord {
                    timestamp_ms: Instant::now().as_millis(),
                    event: UiEvent::Dropped { count },
                }
            } else {
                q.network.pop_front()?
            };
            q.network_json.resize(RECORD_BYTES, 0).ok()?;
            match serde_json_core::to_slice(&record, &mut q.network_json) {
                Ok(n) => q.network_json.truncate(n),
                Err(_) => {
                    q.network_json.clear();
                    q.network_dropped = q.network_dropped.saturating_add(1);
                    return None;
                }
            }
            q.network_offset = 0;
        }
        let start = q.network_offset;
        let end = (start + 255).min(q.network_json.len());
        let mut bytes = Vec::<u8, 256>::new();
        let flags = u8::from(start == 0) | (u8::from(end == q.network_json.len()) << 1);
        let _ = bytes.push(flags);
        let _ = bytes.extend_from_slice(&q.network_json[start..end]);
        q.network_offset = end;
        if end == q.network_json.len() {
            q.network_json.clear();
        }
        Message::with_payload(Event::Ui, &bytes).ok()
    }
    fn connection(&self, connected: bool) {
        let mut q = self.0.borrow_mut();
        if q.connected != connected {
            q.connected = connected;
            q.generation = q.generation.wrapping_add(1);
            q.output.clear();
            q.replay = connected;
        }
    }
    fn generation(&self) -> u32 {
        self.0.borrow().generation
    }
    fn next(&self) -> Option<UiRecord> {
        let mut q = self.0.borrow_mut();
        if q.dropped != 0 {
            let count = take(&mut q.dropped);
            return Some(UiRecord {
                timestamp_ms: Instant::now().as_millis(),
                event: UiEvent::Dropped { count },
            });
        }
        q.output.pop_front()
    }
}

pub fn start(spawner: Spawner, p: Peripherals, bus: &'static Bus) {
    static OUT: StaticCell<[u8; 256]> = StaticCell::new();
    static CONFIG: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL: StaticCell<[u8; 64]> = StaticCell::new();
    static STATE: StaticCell<State<'static>> = StaticCell::new();
    let mut otg = OtgConfig::default();
    otg.vbus_detection = false;
    let driver = Driver::new_fs(p.USB_OTG_FS, Irqs, p.PA12, p.PA11, OUT.init([0; 256]), otg);
    // Development identity only; obtain an allocated VID/PID before production.
    let mut config = Config::new(0x1209, 0x0001);
    config.manufacturer = Some("GigaUSB");
    config.product = Some("GigaUSB Diagnostics");
    config.serial_number = Some("GIGAUSB-DEV");
    let mut builder = Builder::new(
        driver,
        config,
        CONFIG.init([0; 256]),
        BOS.init([0; 256]),
        &mut [],
        CONTROL.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, STATE.init(State::new()), 64);
    let (sender, receiver, control) = class.split_with_control();
    spawner.spawn(control_task(control, bus).unwrap());
    spawner.spawn(usb_task(builder.build()).unwrap());
    spawner.spawn(rx_task(receiver, bus).unwrap());
    spawner.spawn(tx_task(sender, bus).unwrap());
}
#[embassy_executor::task]
async fn usb_task(mut device: UsbDevice<'static, UsbDriver>) {
    device.run().await
}

#[embassy_executor::task]
async fn control_task(control: ControlChanged<'static>, bus: &'static Bus) {
    loop {
        bus.connection(control.dtr());
        control.control_changed().await;
    }
}

#[embassy_executor::task]
async fn rx_task(mut rx: Receiver<'static, UsbDriver>, bus: &'static Bus) {
    let mut packet = [0; 64];
    let mut line = Vec::<u8, 128>::new();
    loop {
        rx.wait_connection().await;
        line.clear();
        let mut oversize = false;
        let mut generation = bus.generation();
        loop {
            if !rx.dtr() {
                bus.connection(false);
                line.clear();
                oversize = false;
            }
            match select(rx.read_packet(&mut packet), Timer::after_millis(20)).await {
                Either::First(Ok(n)) => {
                    if generation != bus.generation() {
                        generation = bus.generation();
                        line.clear();
                        oversize = false;
                    }
                    if !rx.dtr() {
                        continue;
                    }
                    for &byte in &packet[..n] {
                        if byte == b'\n' {
                            if oversize {
                                bus.error("Command exceeds 128 bytes");
                            } else if !line.is_empty() {
                                match serde_json_core::from_slice::<CommandRequest>(&line) {
                                    Ok((request, used))
                                        if line[used..].iter().all(u8::is_ascii_whitespace) =>
                                    {
                                        let accepted = bus
                                            .0
                                            .borrow_mut()
                                            .input
                                            .push_back(request.command)
                                            .is_ok();
                                        if !accepted {
                                            bus.error("Busy: command queue full");
                                        }
                                    }
                                    _ => bus.error("Malformed or unknown command"),
                                }
                            }
                            line.clear();
                            oversize = false;
                        } else if !oversize && line.push(byte).is_err() {
                            oversize = true;
                            line.clear();
                        }
                    }
                }
                Either::First(Err(_)) => {
                    bus.connection(false);
                    line.clear();
                    break;
                }
                Either::Second(_) => {}
            }
        }
    }
}

#[embassy_executor::task]
async fn tx_task(mut tx: Sender<'static, UsbDriver>, bus: &'static Bus) {
    static JSON: StaticCell<[u8; 2048]> = StaticCell::new();
    let json = JSON.init([0; 2048]);
    loop {
        tx.wait_connection().await;
        loop {
            bus.connection(tx.dtr());
            if !tx.dtr() {
                Timer::after_millis(10).await;
                continue;
            }
            let Some(record) = bus.next() else {
                Timer::after_millis(1).await;
                continue;
            };
            // The OTG driver copies packets into its FIFO before returning; a
            // cancelled write may leave that packet pending, but cannot retain
            // this buffer. Prefix newline resynchronizes a partial JSON line.
            json[0] = b'\n';
            let Ok(n) = serde_json_core::to_slice(&record, &mut json[1..2047]) else {
                bus.error("UI record exceeds JSON buffer");
                continue;
            };
            json[n + 1] = b'\n';
            let generation = bus.generation();
            let write = async {
                for chunk in json[..n + 2].chunks(64) {
                    if !tx.dtr() || generation != bus.generation() {
                        return Err(EndpointError::Disabled);
                    }
                    tx.write_packet(chunk).await?;
                }
                // Bulk streams need a short packet to terminate a full-size tail.
                if (n + 2) % 64 == 0 {
                    tx.write_packet(&[]).await?;
                }
                Ok::<(), EndpointError>(())
            };
            match select(write, Timer::after_millis(250)).await {
                Either::First(Ok(())) => {}
                Either::First(Err(_)) => {
                    bus.connection(false);
                    break;
                }
                Either::Second(_) => {
                    let mut q = bus.0.borrow_mut();
                    let lost = match record.event {
                        UiEvent::Dropped { count } => count,
                        _ => 1,
                    };
                    q.dropped = q.dropped.saturating_add(lost);
                }
            }
        }
    }
}
