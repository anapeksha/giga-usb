#![no_std]
#![no_main]
#![forbid(unsafe_code)]

mod hid_transport;
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
    static PROXY: StaticCell<m4::proxy::Proxy> = StaticCell::new();
    let proxy = PROXY.init(m4::proxy::Proxy::new());
    let bus = BUS.init(m4::ui_bus::Bus::new(|| Instant::now().as_millis()));
    let processor = PROCESSOR.init(Processor::new());
    transport::start(spawner, p, bus, proxy);
    spawner.spawn(ipc_task(ipc, bus, processor, proxy).unwrap());
}

#[embassy_executor::task]
async fn ipc_task(
    mut ipc: shared::ipc::Endpoint,
    bus: &'static m4::ui_bus::Bus,
    processor: &'static mut Processor,
    proxy: &'static m4::proxy::Proxy,
) {
    let mut pending = shared::ipc_delivery::Pending::new();
    let mut incoming_command = None;
    let mut command_turn = true;
    let mut proxy_turn = true;
    let mut command_epoch = proxy.io_epoch();
    loop {
        let epoch = proxy.io_epoch();
        if epoch != command_epoch {
            bus.cancel_proxy_enable();
            pending.discard_if(|message| {
                message.event == Event::Command
                    && message
                        .decode::<HostCommand>()
                        .is_ok_and(|command| matches!(command, HostCommand::ProxyEnable { .. }))
            });
            if incoming_command
                .is_some_and(|command| matches!(command, HostCommand::ProxyEnable { .. }))
            {
                incoming_command = None;
            }
            command_epoch = epoch;
        }
        proxy.tick(Instant::now().as_millis());
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
                    Event::Proxy => match message.decode::<shared::usb::proxy::HostToProxy>() {
                        Ok(shared::usb::proxy::HostToProxy::Status { status }) => {
                            bus.emit(Instant::now().as_millis(), UiEvent::Proxy { status });
                        }
                        Ok(shared::usb::proxy::HostToProxy::Support {
                            unsupported_interfaces,
                            ..
                        }) => {
                            if unsupported_interfaces != 0 {
                                bus.error("Proxy ignores non-boot HID interfaces; only boot keyboard/mouse input can be forwarded");
                            }
                        }
                        Ok(frame) => proxy.receive(frame, Instant::now().as_millis()),
                        Err(_) => proxy.fail(shared::usb::proxy::ProxyFault::Transport),
                    },
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
                    proxy.fail(shared::usb::proxy::ProxyFault::Transport);
                    bus.error("Malformed IPC frame");
                    break;
                }
            }
        }
        if pending.discard_if(|message| {
            message.event == Event::Command
                && message
                    .decode::<HostCommand>()
                    .is_ok_and(|command| !proxy.command_allowed(command))
        }) {
            bus.error("Stale proxy enable; request status and explicitly enable again");
        }
        if pending.is_none()
            && proxy_turn
            && let Some(frame) = proxy.next_frame()
        {
            match Message::encode(Event::Proxy, &frame) {
                Ok(message) => pending.set(Some(message)),
                Err(_) => proxy.fail(shared::usb::proxy::ProxyFault::Transport),
            }
        }
        if pending.is_none() && !command_turn {
            pending.set(bus.network_frame());
            command_turn = true;
        }
        if pending.is_none()
            && let Some(command) = bus.command().filter(|command| {
                let allowed = proxy.command_allowed(*command);
                if !allowed {
                    bus.error("Stale proxy enable; request status and explicitly enable again");
                }
                allowed
            })
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
        if pending.is_none()
            && let Some(frame) = proxy.next_frame()
        {
            match Message::encode(Event::Proxy, &frame) {
                Ok(message) => pending.set(Some(message)),
                Err(_) => proxy.fail(shared::usb::proxy::ProxyFault::Transport),
            }
        }
        let event = pending.event();
        if event.is_some() {
            proxy_turn = event != Some(Event::Proxy);
        }
        if pending.retry(|message| ipc.try_send(message)).is_err() {
            if event == Some(Event::Ui) {
                bus.network_failed();
            }
            proxy.fail(shared::usb::proxy::ProxyFault::Transport);
            bus.error("IPC send failed");
        } else if event == Some(Event::Ui) && pending.is_none() {
            bus.network_frame_sent();
        }
        // No cross-core waker: bound idle/full polling without imposing a
        // millisecond of latency on each single-slot frame/chunk.
        Timer::after_micros(25).await;
    }
}
