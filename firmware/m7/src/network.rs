//! M7 owns Wi-Fi SDIO and every DMA buffer in AXI SRAM, never M4 SRAM.

use core::{
    cell::{Cell, RefCell},
    fmt::Write,
    net::Ipv4Addr,
    task::{Context, Waker},
};
use cyw43::{JoinAuth, JoinOptions, State};
use embassy_futures::join::join;
use embassy_futures::select::{Either, select};
use embassy_net::{
    Config, ConfigV4, Ipv4Cidr, Stack, StackResources, StaticConfigV4,
    driver::{Capabilities, Driver, HardwareAddress, LinkState},
};
use embassy_stm32::{
    Peri, bind_interrupts,
    gpio::{Level, Output, Speed},
    peripherals,
    sdmmc::{self, Sdmmc, sdio::SerialDataInterface},
    time::Hertz,
};
use embassy_time::{Delay, Duration, Timer, with_timeout};
use giga_r1::wifi::Wifi;
use leasehund::{DhcpConfigBuilder, DhcpServer};
use shared::{
    logging,
    usb::types::{HostCommand, NetworkInfo},
    wifi::WifiControl,
};

use crate::http;
use m7::{
    credentials::{CredentialStore, prepare_m7},
    network_bus::Bus,
};
use static_cell::StaticCell;

bind_interrupts!(struct Irqs { SDMMC1 => sdmmc::InterruptHandler<peripherals::SDMMC1>; });
static RADIO: StaticCell<State> = StaticCell::new();
static RESOURCES: StaticCell<StackResources<6>> = StaticCell::new();

pub struct Pins {
    pub flash: Peri<'static, peripherals::FLASH>,
    pub sdmmc: Peri<'static, peripherals::SDMMC1>,
    pub clock: Peri<'static, peripherals::PC12>,
    pub command: Peri<'static, peripherals::PD2>,
    pub d0: Peri<'static, peripherals::PC8>,
    pub d1: Peri<'static, peripherals::PC9>,
    pub d2: Peri<'static, peripherals::PC10>,
    pub d3: Peri<'static, peripherals::PC11>,
    pub power: Peri<'static, peripherals::PB10>,
}

/// Run setup AP/HTTP services and persistent station provisioning on M7.
/// Association/DHCP failure or later link loss restores setup mode. The open AP
/// and unauthenticated HTTP expose provisioning credentials unencrypted.
#[embassy_executor::task]
pub async fn run(pins: Pins, bus: &'static Bus) {
    let mut store = CredentialStore::new(prepare_m7(pins.flash)).ok();
    let loaded = if let Some(store) = store.as_mut() {
        match store.load().await {
            Ok(request) => request,
            Err(_) => {
                logging::warn!("Wi-Fi credentials unreadable; starting setup");
                None
            }
        }
    } else {
        logging::error!("Wi-Fi credential storage unavailable");
        None
    };
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
            return;
        }
    };
    let runner = parts.take_runner().unwrap();
    let wifi_control = WifiControl::new();
    wifi_control.set_saved(loaded.is_some());
    if let Some(request) = loaded {
        let _ = wifi_control.submit(request);
    }
    let link = ModeLink {
        ap: Cell::new(false),
        waker: RefCell::new(None),
    };
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
        let config = Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(ip, 24),
            gateway: None,
            dns_servers: heapless::Vec::new(),
        });
        // Fixed seed for TCP sequence/port selection; not security-grade randomness.
        let (stack, mut net) = embassy_net::new(
            ModeDriver(parts.device, &link),
            config.clone(),
            RESOURCES.init(StackResources::new()),
            0x4749_4741_5553_4201,
        );
        // HTTP futures borrow control alongside the network control loop.
        let http = join(
            http::serve(stack, bus, &wifi_control),
            http::serve(stack, bus, &wifi_control),
        );
        let _ = bus.command(HostCommand::Status);

        join(
            net.run(),
            join(http, async {
                let mut setup_error = None;
                loop {
                    let request = if let Some(request) = wifi_control.take_request() {
                        request
                    } else {
                        parts.control.start_ap_open("GigaUSB", 6).await;
                        stack.set_config_v4(config.ipv4.clone());
                        link.set(true);
                        wifi_control.setup(setup_error);
                        report_network(bus, &mac, "setup", Some(ip), setup_error);
                        logging::info!("Wi-Fi setup dashboard: http://192.168.4.1/");
                        // Dropping this select's DHCP future closes its socket before station mode.
                        let result = select(dhcp(stack), async {
                            loop {
                                if let Some(request) = wifi_control.take_request() {
                                    break request;
                                }
                                Timer::after_millis(50).await;
                            }
                        })
                        .await;
                        let Either::Second(request) = result else {
                            continue;
                        };
                        Timer::after_secs(1).await;
                        request
                    };
                    let mut request = Some(request);
                    let was_ap = link.ap.get();
                    link.set(false);
                    stack.set_config_v4(ConfigV4::None);
                    if was_ap {
                        parts.control.close_ap().await;
                    }
                    stack.set_config_v4(Config::dhcpv4(Default::default()).ipv4);
                    let credentials = request.as_ref().unwrap();
                    let mut key = [0u8; 32];
                    let options = if credentials.password.is_empty() {
                        JoinOptions::new_open()
                    } else if credentials.password.len() == 64 {
                        for (out, hex) in key
                            .iter_mut()
                            .zip(credentials.password.as_bytes().as_chunks::<2>().0.iter())
                        {
                            let digit = |b: u8| {
                                if b.is_ascii_digit() {
                                    b - b'0'
                                } else {
                                    b.to_ascii_lowercase() - b'a' + 10
                                }
                            };
                            *out = digit(hex[0]) * 16 + digit(hex[1]);
                        }
                        let mut options = JoinOptions::new(&key);
                        options.auth = JoinAuth::Wpa2;
                        options.passphrase_is_prehashed = true;
                        options
                    } else {
                        let mut options = JoinOptions::new(credentials.password.as_bytes());
                        options.auth = JoinAuth::Wpa2;
                        options
                    };
                    report_network(bus, &mac, "associating", None, None);
                    logging::info!("Wi-Fi station association started");
                    let association = with_timeout(
                        Duration::from_secs(30),
                        parts.control.join(&credentials.ssid, options),
                    )
                    .await;

                    key.fill(0);
                    core::hint::black_box(&key);
                    let error = 'station: {
                        match association {
                            Ok(Ok(())) => {}
                            Ok(Err(cyw43::JoinError::NetworkNotFound)) => {
                                break 'station "association network not found";
                            }
                            Ok(Err(cyw43::JoinError::AuthenticationFailure)) => {
                                break 'station "association authentication failed";
                            }
                            Ok(Err(cyw43::JoinError::JoinFailure(status))) => {
                                logging::warn!("Wi-Fi association failed: status={}", status);
                                break 'station "association failed";
                            }
                            Err(_) => break 'station "association timed out",
                        }
                        report_network(bus, &mac, "link_wait", None, None);
                        logging::info!("Wi-Fi associated; waiting for driver link");
                        if with_timeout(Duration::from_secs(10), stack.wait_link_up())
                            .await
                            .is_err()
                        {
                            break 'station "driver link timed out";
                        }
                        report_network(bus, &mac, "dhcp", None, None);
                        logging::info!("Wi-Fi driver link up; waiting for DHCP");
                        if with_timeout(Duration::from_secs(30), stack.wait_config_up())
                            .await
                            .is_err()
                        {
                            break 'station "DHCP timed out";
                        }
                        let credentials = request.take().unwrap();
                        let mut save_error = None;
                        let save_warning = if wifi_control.state().saved {
                            "Connected, but new credentials were not saved; previous saved network retained"
                        } else {
                            "Connected, but credentials were not saved"
                        };
                        if let Some(store) = store.as_mut() {
                            let unchanged = match store.load().await {
                                Ok(Some(saved)) => {
                                    saved.ssid == credentials.ssid
                                        && saved.password == credentials.password
                                }
                                Ok(None) => false,
                                Err(_) => {
                                    logging::warn!("Wi-Fi credential comparison read failed");
                                    false
                                },
                            };
                            let saved = if unchanged {
                                true
                            } else {
                                bus.set_saving(true);
                                let result = store.save(&credentials).await;
                                bus.set_saving(false);
                                result.is_ok()
                            };
                            if saved {
                                wifi_control.set_saved(true);
                            } else {
                                logging::error!("Wi-Fi connected but credentials not saved");
                                save_error = Some(save_warning);
                            }
                        } else {
                            logging::error!("Wi-Fi connected but credential storage unavailable");
                            save_error = Some(save_warning);
                        }
                        drop(credentials);
                        let mut last_ip = None;
                        loop {
                            if !stack.is_link_up() {
                                break 'station "connection lost";
                            }
                            let Some(config) = stack.config_v4() else {
                                break 'station "DHCP lease lost";
                            };
                            let address = config.address.address();
                            if last_ip != Some(address) {
                                let mut ip_text = heapless::String::new();
                                let _ = write!(ip_text, "{}", address);
                                logging::info!(
                                    "Wi-Fi station dashboard: http://{}/",
                                    ip_text.as_str()
                                );
                                wifi_control.connected(ip_text);
                                wifi_control.set_error(save_error);
                                report_network(bus, &mac, "connected", Some(address), None);
                                last_ip = Some(address);
                            }
                            Timer::after_millis(250).await;
                        }
                    };
                    drop(request);
                    report_network(bus, &mac, "error", None, Some(error));
                    logging::warn!("Wi-Fi station stopped: {}", error);
                    stack.set_config_v4(ConfigV4::None);
                    parts.control.leave().await;
                    setup_error = Some(error);
                }
            }),
        )
        .await;
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

// AP availability overrides station association only while the AP is running.
struct ModeLink {
    ap: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}
impl ModeLink {
    fn set(&self, ap: bool) {
        self.ap.set(ap);
        if let Some(waker) = self.waker.borrow_mut().take() {
            waker.wake();
        }
    }
}
struct ModeDriver<'a, D>(D, &'a ModeLink);
impl<D: Driver> Driver for ModeDriver<'_, D> {
    type RxToken<'a>
        = D::RxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = D::TxToken<'a>
    where
        Self: 'a;
    fn receive(&mut self, cx: &mut Context<'_>) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.0.receive(cx)
    }
    fn transmit(&mut self, cx: &mut Context<'_>) -> Option<Self::TxToken<'_>> {
        self.0.transmit(cx)
    }
    fn link_state(&mut self, cx: &mut Context<'_>) -> LinkState {
        *self.1.waker.borrow_mut() = Some(cx.waker().clone());
        let station = self.0.link_state(cx);
        if self.1.ap.get() {
            LinkState::Up
        } else {
            station
        }
    }
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }
    fn hardware_address(&self) -> HardwareAddress {
        self.0.hardware_address()
    }
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
