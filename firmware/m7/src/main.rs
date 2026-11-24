#![no_std]
#![no_main]
#![recursion_limit = "256"]
#![forbid(unsafe_code)]

mod assets;
mod http;
mod network;
mod status_led;

#[cfg(feature = "defmt")]
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_stm32::gpio::{Input, Level, Output, Pull, Speed};
use embassy_time::{Instant, Timer};

use m7::boot::{self, M4Boot};
use m7::{network_bus::Bus, usb::host::Host};
use panic_halt as _;
use shared::{ipc_delivery::Pending, logging, protocol::Event};
use static_cell::StaticCell;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let (p, ipc, m4) = boot::init_m7().unwrap_or_else(|_| {
        logging::error!("Disable BCM4 in option bytes, then perform a full system reset");
        panic!("M4 must remain held until M7 initializes the HAL");
    });
    match m4 {
        M4Boot::Started => logging::info!("M4 released; USB capture IPC initialized"),
        M4Boot::InvalidVectors => {
            logging::warn!("M4 vectors invalid; capture queue will count drops while M4 is held")
        }
    }

    logging::info!(
        "M7 USB-A host: HS internal FS PHY, PB14/PB15, PA15 VBUS; timer polling, no DMA/IRQ"
    );
    static BUS: StaticCell<Bus> = StaticCell::new();
    let bus = BUS.init(Bus::new());
    spawner.spawn(
        status_led::run(
            Output::new(p.PI12, Level::High, Speed::Low),
            Output::new(p.PJ13, Level::High, Speed::Low),
            Output::new(p.PE3, Level::High, Speed::Low),
            bus,
        )
        .unwrap(),
    );
    let mut flash = assets::Flash {
        clock: Output::new(p.PF10, Level::Low, Speed::Low),
        select: Output::new(p.PG6, Level::High, Speed::Low),
        mosi: Output::new(p.PD11, Level::Low, Speed::Low),
        miso: Input::new(p.PD12, Pull::None),
        hold: Output::new(p.PF6, Level::High, Speed::Low),
        protect: Output::new(p.PE2, Level::High, Speed::Low),
    };
    let header = embassy_time::with_timeout(embassy_time::Duration::from_secs(5), flash.validate())
        .await
        .ok()
        .flatten();
    if header.is_none() {
        logging::warn!("QSPI dashboard missing or invalid; HTTP recovery notice only");
    }
    static ASSETS: StaticCell<assets::Assets> = StaticCell::new();
    let assets = ASSETS.init(assets::Assets {
        flash: embassy_sync::mutex::Mutex::new(flash),
        header,
    });
    spawner.spawn(
        network::run(
            network::Pins {
                sdmmc: p.SDMMC1,
                clock: p.PC12,
                command: p.PD2,
                d0: p.PC8,
                d1: p.PC9,
                d2: p.PC10,
                d3: p.PC11,
                power: p.PB10,
            },
            bus,
            spawner,
            assets,
        )
        .unwrap(),
    );
    static DELIVERY: StaticCell<m7::ipc_delivery::Delivery> = StaticCell::new();
    let delivery = DELIVERY.init(m7::ipc_delivery::Delivery::new());
    spawner.spawn(ipc_task(ipc, delivery, bus).unwrap());
    let mut host = Host::new(p.USB_OTG_HS, p.PB14, p.PB15, p.PA15, delivery).with_network(bus);
    host.run(|dropped| logging::warn!("USB capture dropped {} frames (cumulative)", dropped))
        .await;
}

// The sole endpoint owner runs independently of register polling and HTTP. SRAM4
// remains the BSP atomic SPSC transport, not an embassy channel across cores.
#[embassy_executor::task]
async fn ipc_task(
    mut ipc: shared::ipc::Endpoint,
    delivery: &'static m7::ipc_delivery::Delivery,
    bus: &'static Bus,
) {
    let mut pending = Pending::new();
    let mut incoming = None;
    loop {
        for _ in 0..8 {
            if let Some(message) = incoming.take() {
                incoming = delivery.incoming.push_back(message).err();
            }
            if incoming.is_none() {
                match ipc.try_receive() {
                    Ok(Some(message)) if message.event == Event::Ui => bus.receive(&message),
                    Ok(Some(message)) if message.event == Event::Proxy => {
                        match message.decode::<shared::usb::proxy::ProxyToHost>() {
                            Ok(frame) => {
                                if matches!(frame, shared::usb::proxy::ProxyToHost::Output { .. }) {
                                    delivery.cancel_proxy_enable();
                                    bus.cancel_proxy_enable();
                                }
                                delivery.proxy.feedback(frame);
                            }
                            Err(_) => delivery.note_transport_error(),
                        }
                    }
                    Ok(Some(message)) => incoming = delivery.incoming.push_back(message).err(),
                    Ok(None) => {}
                    Err(_) => delivery.note_transport_error(),
                }
            }
            pending.discard_if(|message| {
                message.event == Event::Proxy
                    && message
                        .decode::<shared::usb::proxy::HostToProxy>()
                        .is_ok_and(|frame| frame.generation() != delivery.proxy.status().generation)
            });
            delivery.fill_pending(&mut pending);
            let event = pending.event();
            if pending.retry(|message| ipc.try_send(message)).is_err() {
                if event == Some(Event::Command) {
                    delivery.request_snapshot(true);
                }
                if event == Some(Event::Usb) {
                    delivery.failed.set(delivery.failed.get().saturating_add(1));
                }
                delivery.note_transport_error();
            }
            // Cross-core slot readiness has no executor waker. A short bounded
            // poll replaces the former 1 ms per-frame bottleneck on M4.
            if pending.is_none() && delivery.captures.is_empty() {
                match select(delivery.captures.receive(), Timer::after_micros(25)).await {
                    Either::First(message) => pending.set(Some(message)),
                    Either::Second(_) => {}
                }
            } else {
                Timer::after_micros(25).await;
            }
        }
        delivery.report_transport_errors(bus, Instant::now().as_millis());
        embassy_futures::yield_now().await;
    }
}
