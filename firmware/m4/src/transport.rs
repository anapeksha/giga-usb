//! USB-C device diagnostics. The development VID/PID is not a production VID
//! allocation. No VBUS sense pin is claimed: the self-powered board uses the
//! HAL's vbus_detect=false mode. M7 owns the USB clock mux, M4 owns OTG_FS.
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_stm32::{
    Peripherals, bind_interrupts,
    peripherals::USB_OTG_FS,
    usb::{Config as OtgConfig, Driver, InterruptHandler},
};
use embassy_time::Timer;
use embassy_usb::{
    Builder, Config, UsbDevice,
    class::cdc_acm::{CdcAcmClass, ControlChanged, Receiver, Sender, State},
    driver::EndpointError,
};
use heapless::Vec;
use m4::ui_bus::Bus;
use shared::usb::types::*;
use static_cell::StaticCell;

type UsbDriver = Driver<'static, USB_OTG_FS>;
bind_interrupts!(struct Irqs { OTG_FS => InterruptHandler<USB_OTG_FS>; });

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
                                        let accepted = bus.accept_command(request.command).is_ok();
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
                    bus.serial_failed(record);
                }
            }
        }
    }
}
