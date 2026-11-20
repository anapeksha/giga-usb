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
    ipc::SendError,
    protocol::{Event, Message},
    usb::types::*,
};
use static_cell::StaticCell;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let (p, mut ipc) = m4::boot::init_m4();
    static BUS: StaticCell<transport::Bus> = StaticCell::new();
    static PROCESSOR: StaticCell<Processor> = StaticCell::new();
    let bus = BUS.init(transport::Bus::new());
    let processor = PROCESSOR.init(Processor::new());
    transport::start(spawner, p, bus);
    let mut pending = None;
    loop {
        if bus.take_replay() {
            processor.snapshot(|event| bus.emit(Instant::now().as_millis(), event));
        }
        // Always drain the receive slot, even when TX or the command slot is full.
        for _ in 0..16 {
            match ipc.try_receive() {
                Ok(Some(message)) => match message.event {
                    Event::Usb => match message.decode::<CapturedEvent>() {
                        Ok(capture) => processor.process(capture.event, |event| {
                            bus.emit(capture.timestamp_us / 1000, event)
                        }),
                        Err(_) => bus.error("Malformed IPC capture"),
                    },
                    Event::Command => match message.decode::<HostCommand>() {
                        Ok(command) => bus.accept_command(command),
                        Err(_) => bus.error("Malformed network command"),
                    },
                    Event::Ping if pending.is_none() => {
                        pending = Some(Message {
                            event: Event::Pong,
                            value: message.value,
                        });
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
        if pending.is_none()
            && let Some(command) = bus.command()
        {
            if command == HostCommand::Status {
                processor.snapshot(|event| bus.emit(Instant::now().as_millis(), event));
            }
            match Message::encode(Event::Command, &command) {
                Ok(message) => pending = Some(message),
                Err(_) => bus.error("Command encoding failed"),
            }
        }
        if pending.is_none() {
            pending = bus.network_frame();
        }
        if let Some(message) = pending.as_ref() {
            match ipc.try_send(message) {
                Ok(()) => pending = None,
                Err(SendError::Full) => {}
                Err(_) => {
                    pending = None;
                    bus.error("IPC send failed");
                }
            }
        }
        Timer::after_millis(1).await;
    }
}
