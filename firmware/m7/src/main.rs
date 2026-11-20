#![no_std]
#![no_main]
#![forbid(unsafe_code)]

mod http;
mod network;
mod status_led;

#[cfg(feature = "defmt")]
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32::gpio::{Level, Output, Speed};

use m7::boot::{self, M4Boot};
use m7::{network_bus::Bus, usb::host::Host};
use panic_halt as _;
use shared::logging;
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
        )
        .unwrap(),
    );
    // This task owns the sole M7 IPC endpoint. The LED task never touches IPC
    // or either USB controller; USB-C/FS remains exclusively assigned to M4.
    let mut host = Host::new(p.USB_OTG_HS, p.PB14, p.PB15, p.PA15, ipc).with_network(bus);
    host.run(|dropped| logging::warn!("USB capture dropped {} frames (cumulative)", dropped))
        .await;
}
