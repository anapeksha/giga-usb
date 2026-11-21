#![no_std]
#![no_main]
#![forbid(unsafe_code)]

mod transport;

#[cfg(feature = "defmt")]
use defmt_rtt as _;

use embassy_executor::Spawner;
use embassy_time::{Instant, Timer};
use m4::usb::processor::Processor;
use panic_halt as _;
use shared::{
    protocol::{Event, Message},
    usb::types::*,
};
use static_cell::StaticCell;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let (p, ipc) = m4::boot::init_m4();
    static BUS: StaticCell<m4::ui_bus::Bus> = StaticCell::new();
    static PROCESSOR: StaticCell<Processor> = StaticCell::new();
    let bus = BUS.init(m4::ui_bus::Bus::new(|| Instant::now().as_millis()));
    let processor = PROCESSOR.init(Processor::new());
    transport::start(spawner, p, bus);
    spawner.spawn(ipc_task(ipc, bus, processor).unwrap());
}

#[embassy_executor::task]
async fn ipc_task(
    mut ipc: shared::ipc::Endpoint,
    bus: &'static m4::ui_bus::Bus,
    processor: &'static mut Processor,
) {
    let mut pending = shared::ipc_delivery::Pending::new();
    let mut incoming_command = None;
    let mut command_turn = true;
    loop {
        if let Some(command) = incoming_command.take() {
            incoming_command = bus.accept_command(command).err();
        }
        if bus.take_replay() {
            bus.replay_serial(processor);
        }
        // Drain RX independently of TX. If command admission is full, retain
        // that command and apply backpressure rather than consuming it twice.
        for _ in 0..8 {
            if incoming_command.is_some() {
                break;
            }
            match ipc.try_receive() {
                Ok(Some(message)) => match message.event {
                    Event::Usb => match message.decode::<CapturedEvent>() {
                        Ok(capture) => processor.process(capture.event, |event| {
                            bus.emit(capture.timestamp_us / 1000, event)
                        }),
                        Err(_) => bus.error("Malformed IPC capture"),
                    },
                    Event::Command => match message.decode::<HostCommand>() {
                        Ok(command) => {
                            incoming_command = bus.accept_ipc_command(command, processor).err();
                        }
                        Err(_) => bus.error("Malformed network command"),
                    },
                    Event::Ping if pending.is_none() => {
                        pending.set(Some(Message {
                            event: Event::Pong,
                            value: message.value,
                        }));
                    }
                    _ => {}
                },
                Ok(None) => break,
                Err(_) => {
                    bus.error("Malformed IPC frame");
                    break;
                }
            }
        }
        if pending.is_none() && !command_turn {
            pending.set(bus.network_frame());
            command_turn = true;
        }
        if pending.is_none()
            && let Some(command) = bus.command()
        {
            command_turn = false;
            if command == HostCommand::Status {
                processor.snapshot(|event| bus.emit(Instant::now().as_millis(), event));
            }
            match Message::encode(Event::Command, &command) {
                Ok(message) => pending.set(Some(message)),
                Err(_) => bus.error("Command encoding failed"),
            }
        }
        if pending.is_none() {
            pending.set(bus.network_frame());
            command_turn = true;
        }
        let event = pending.event();
        if pending.retry(|message| ipc.try_send(message)).is_err() {
            if event == Some(Event::Ui) {
                bus.network_failed();
            }
            bus.error("IPC send failed");
        } else if event == Some(Event::Ui) && pending.is_none() {
            bus.network_frame_sent();
        }
        // No cross-core waker: bound idle/full polling without imposing a
        // millisecond of latency on each single-slot frame/chunk.
        Timer::after_micros(25).await;
    }
}
