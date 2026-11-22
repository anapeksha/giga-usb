//! M7 owns Wi-Fi SDIO and every DMA buffer in AXI SRAM, never M4 SRAM.

use core::{fmt::Write, net::Ipv4Addr};
use cyw43::State;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_net::{Config, Ipv4Cidr, Stack, StackResources, StaticConfigV4};
use embassy_stm32::{
    Peri, bind_interrupts,
    gpio::{Level, Output, Speed},
    peripherals,
    sdmmc::{self, Sdmmc, sdio::SerialDataInterface},
    time::Hertz,
};
use embassy_time::Delay;
use giga_r1::wifi::Wifi;
use leasehund::{DhcpConfigBuilder, DhcpServer};
use shared::{
    logging,
    usb::types::{HostCommand, NetworkInfo},
};

use crate::http;
use m7::{ap_driver::ApDriver, network_bus::Bus};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs { SDMMC1 => sdmmc::InterruptHandler<peripherals::SDMMC1>; });
static RADIO: StaticCell<State> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<8>> = StaticCell::new();

pub struct Pins {
    pub sdmmc: Peri<'static, peripherals::SDMMC1>,
    pub clock: Peri<'static, peripherals::PC12>,
    pub command: Peri<'static, peripherals::PD2>,
    pub d0: Peri<'static, peripherals::PC8>,
    pub d1: Peri<'static, peripherals::PC9>,
    pub d2: Peri<'static, peripherals::PC10>,
    pub d3: Peri<'static, peripherals::PC11>,
    pub power: Peri<'static, peripherals::PB10>,
}

/// Run the open GigaUSB AP, DHCP and three HTTP workers indefinitely.
#[embassy_executor::task]
pub async fn run(
    pins: Pins,
    bus: &'static Bus,
    spawner: Spawner,
    assets: &'static crate::assets::Assets,
) {
    let mut sdmmc = Sdmmc::new_4bit(
        pins.sdmmc,
        Irqs,
        pins.clock,
        pins.command,
        pins.d0,
        pins.d1,
        pins.d2,
        pins.d3,
        Default::default(),
    );
    let wifi = Wifi::new(Output::new(pins.power, Level::Low, Speed::Low)).unwrap();
    let mut delay = Delay;
    let sdmmc = &mut sdmmc;
    let mut parts = match wifi
        .start_async_with(RADIO.init(State::new()), &mut delay, move || {
            SerialDataInterface::new(sdmmc, Hertz::khz(400))
        })
        .await
    {
        Ok(parts) => parts,
        Err(_) => {
            logging::error!("Wi-Fi SDIO startup failed; USB diagnostics remain available");
            report_network(
                bus,
                &[0; 6],
                "error",
                None,
                Some("Wi-Fi SDIO startup failed"),
            );
            return;
        }
    };
    let runner = parts.take_runner().unwrap();
    let services = async {
        parts.initialize().await;
        let mac = parts.control.address().await;
        logging::info!(
            "Wi-Fi MAC: {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5]
        );
        let ip = Ipv4Addr::new(192, 168, 4, 1);
        parts.control.start_ap_open("GigaUSB", 6).await;
        report_network(bus, &mac, "setup", Some(ip), None);
        logging::info!("Wi-Fi dashboard: http://192.168.4.1/");
        let config = Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(ip, 24),
            gateway: None,
            dns_servers: heapless::Vec::new(),
        });
        // Fixed seed for TCP sequence/port selection; not security-grade randomness.
        let (stack, mut net) = embassy_net::new(
            ApDriver(parts.device),
            config,
            RESOURCES.init(StackResources::new()),
            0x4749_4741_5553_4201,
        );
        let _ = bus.command(HostCommand::Status);
        for _ in 0..3 {
            spawner.spawn(http::serve(stack, bus, assets).unwrap());
        }
        join(net.run(), dhcp(stack)).await;
    };
    join(runner.run(), services).await;
}

fn report_network(bus: &Bus, mac: &[u8; 6], mode: &str, ip: Option<Ipv4Addr>, error: Option<&str>) {
    let mut mode_text = heapless::String::new();
    let _ = mode_text.push_str(mode);
    let ip = ip.map(|address| {
        let mut text = heapless::String::new();
        let _ = write!(text, "{}", address);
        text
    });
    let error = error.map(|message| {
        let mut text = heapless::String::new();
        let _ = text.push_str(message);
        text
    });
    bus.set_network(NetworkInfo {
        mode: mode_text,
        mac: *mac,
        ip,
        error,
    });
}

async fn dhcp(stack: Stack<'_>) {
    let ip = Ipv4Addr::new(192, 168, 4, 1);
    let config = DhcpConfigBuilder::<1>::new()
        .server_ip(ip)
        .subnet_mask(Ipv4Addr::new(255, 255, 255, 0))
        .router(ip)
        .ip_pool(
            Ipv4Addr::new(192, 168, 4, 100),
            Ipv4Addr::new(192, 168, 4, 131),
        )
        .lease_time(3600)
        .build();
    let mut server: DhcpServer<32, 1> = DhcpServer::with_config(config);
    server.run_with_callback(stack, |_| {}).await;
}
