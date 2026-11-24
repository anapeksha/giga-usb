//! USB-C device diagnostics. The development VID/PID is not a production VID
//! allocation. No VBUS sense pin is claimed: the self-powered board uses the
//! HAL's vbus_detect=false mode. M7 owns the USB clock mux, M4 owns OTG_FS.
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_stm32::{
    Peripherals, bind_interrupts, pac,
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

pub(crate) type UsbDriver = Driver<'static, USB_OTG_FS>;
bind_interrupts!(struct Irqs { OTG_FS => InterruptHandler<USB_OTG_FS>; });

pub fn start(
    spawner: Spawner,
    p: Peripherals,
    bus: &'static Bus,
    proxy: &'static m4::proxy::Proxy,
) {
    static OUT: StaticCell<[u8; 256]> = StaticCell::new();
    static CONFIG: StaticCell<[u8; 512]> = StaticCell::new();
    static BOS: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL: StaticCell<[u8; 64]> = StaticCell::new();
    static STATE: StaticCell<State<'static>> = StaticCell::new();
    let mut otg = OtgConfig::default();
    otg.vbus_detection = false;
    let driver = Driver::new_fs(p.USB_OTG_FS, Irqs, p.PA12, p.PA11, OUT.init([0; 256]), otg);
    // Keep the existing development identity for the composite functions, never
    // copy the attached physical device's identity. Obtain an allocated VID/PID
    // before production. CDC stays first (interfaces 0/1), HID follows (2/3).
    let mut config = Config::new(0x1209, 0x0001);
    config.manufacturer = Some("GigaUSB");
    config.product = Some("GigaUSB Diagnostics");
    config.serial_number = Some("GIGAUSB-DEV");
    let mut builder = Builder::new(
        driver,
        config,
        CONFIG.init([0; 512]),
        BOS.init([0; 256]),
        &mut [],
        CONTROL.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, STATE.init(State::new()), 64);
    let (sender, receiver, control) = class.split_with_control();
    let output = crate::hid_transport::add(&mut builder, proxy);
    spawner.spawn(
        usb_task(
            builder.build(),
            output,
            sender,
            receiver,
            control,
            bus,
            proxy,
        )
        .unwrap(),
    );
}
#[embassy_executor::task]
async fn usb_task(
    mut device: UsbDevice<'static, UsbDriver>,
    mut output: crate::hid_transport::Output,
    mut tx: Sender<'static, UsbDriver>,
    mut rx: Receiver<'static, UsbDriver>,
    control: ControlChanged<'static>,
    bus: &'static Bus,
    proxy: &'static m4::proxy::Proxy,
) {
    static JSON: StaticCell<[u8; 2048]> = StaticCell::new();
    let json = JSON.init([0; 2048]);
    // This is intentionally a single-use service, not a recovery loop. Dropping
    // the select cancels every device/endpoint future before lifecycle writes.
    // CDC futures are independently polled during normal HID operation.
    let _ = select(
        device.run(),
        select(
            output.run(proxy),
            embassy_futures::join::join3(
                control_task(&control, bus),
                rx_task(&mut rx, bus, proxy),
                tx_task(&mut tx, bus, proxy, json),
            ),
        ),
    )
    .await;
    proxy.output_failed();
    bus.connection(false);
    bus.cancel_proxy_enable();
    // Embassy 0.6's OTG disable is a no-op at the hardware boundary. Soft
    // disconnect explicitly, with all device/endpoint futures already stopped.
    pac::USB_OTG_FS.dctl().modify(|w| w.set_sdis(true));
    device.disable().await;
    // The locked STM32H7 OTG_FS instance has nine endpoint slots. Deactivate
    // them without a FIFO flush or hardware wait while physically disconnected.
    for index in 1..9 {
        pac::USB_OTG_FS.diepctl(index).modify(|w| {
            w.set_snak(true);
            if w.epena() {
                w.set_epdis(true);
            }
            w.set_usbaep(false);
        });
        pac::USB_OTG_FS.doepctl(index).modify(|w| {
            w.set_snak(true);
            if w.epena() {
                w.set_epdis(true);
            }
            w.set_usbaep(false);
        });
    }
    bus.error(
        "USB-C output fault: reset/power-cycle the board; USB host Reset cannot recover USB-C",
    );
    // Never poll/read/reopen the old driver: bus reset does not clear its private
    // buffered OUT/SETUP state. Only a full board reset clears the BSS-owned
    // driver state and terminal latch. HTTP/IPC remain live on their own tasks.
    core::future::pending::<()>().await;
}

async fn control_task(control: &ControlChanged<'static>, bus: &Bus) {
    loop {
        bus.connection(control.dtr());
        control.control_changed().await;
    }
}

async fn rx_task(rx: &mut Receiver<'static, UsbDriver>, bus: &Bus, proxy: &m4::proxy::Proxy) {
    let mut packet = [0; 64];
    let mut line = Vec::<u8, 128>::new();
    loop {
        rx.wait_connection().await;
        line.clear();
        let mut oversize = false;
        let mut generation = bus.generation();
        loop {
            let Some(epoch) = proxy.io_epoch() else {
                line.clear();
                oversize = false;
                Timer::after_millis(1).await;
                continue;
            };
            if !rx.dtr() {
                bus.connection(false);
                line.clear();
                oversize = false;
            }
            match select(rx.read_packet(&mut packet), Timer::after_millis(20)).await {
                Either::First(Ok(n)) => {
                    if !proxy.io_valid(epoch) {
                        line.clear();
                        oversize = false;
                        continue;
                    }
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
                                        if !proxy.command_allowed(request.command) {
                                            bus.error("Stale proxy enable; request status and explicitly enable again");
                                        } else if bus.accept_command(request.command).is_err() {
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

async fn tx_task(
    tx: &mut Sender<'static, UsbDriver>,
    bus: &Bus,
    proxy: &m4::proxy::Proxy,
    json: &mut [u8; 2048],
) {
    loop {
        tx.wait_connection().await;
        loop {
            let Some(epoch) = proxy.io_epoch() else {
                Timer::after_millis(1).await;
                continue;
            };
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
                bus.serial_failed(record);
                bus.error("UI record exceeds JSON buffer");
                continue;
            };
            json[n + 1] = b'\n';
            let generation = bus.generation();
            let write = async {
                for chunk in json[..n + 2].chunks(64) {
                    if !tx.dtr() || generation != bus.generation() || !proxy.io_valid(epoch) {
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
                    bus.serial_failed(record);
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
