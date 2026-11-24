//! USB-C boot HID functions. The USB lifecycle task owns these endpoints and
//! cancels all device/endpoint futures before a latched fatal disconnect. Application HID code
//! issues no direct FIFO-register writes or flush commands.
use crate::transport::UsbDriver;
use core::{future::Future, pin::pin};
use embassy_stm32::pac;
use embassy_time::{Duration, Instant, Timer};
use embassy_usb::{
    Builder, Handler,
    control::{InResponse, OutResponse, Recipient, Request, RequestType},
    driver::{Driver, Endpoint, EndpointError, EndpointIn},
};
use m4::proxy::{KEYBOARD_DESCRIPTOR, MOUSE_DESCRIPTOR, OutputResult, Proxy, Work};
use shared::usb::proxy::{BootKind, OUTPUT_REPORT_DEADLINE_MS};
use static_cell::StaticCell;

type In = <UsbDriver as Driver<'static>>::EndpointIn;
pub struct Output {
    keyboard: In,
    mouse: In,
}
struct Control {
    proxy: &'static Proxy,
    kind: BootKind,
    interface: u16,
    descriptor: &'static [u8],
    hid: [u8; 9],
}
struct Link {
    proxy: &'static Proxy,
    configured: bool,
    suspended: bool,
}
impl Handler for Link {
    fn enabled(&mut self, enabled: bool) {
        if !enabled {
            self.configured = false;
            self.proxy.bus_reset();
        }
    }
    fn reset(&mut self) {
        self.configured = false;
        self.suspended = false;
        self.proxy.bus_reset();
    }
    fn configured(&mut self, configured: bool) {
        self.proxy.link(false);
        self.configured = configured;
        self.proxy.link(configured && !self.suspended);
    }
    fn set_alternate_setting(&mut self, interface: embassy_usb::types::InterfaceNumber, _: u8) {
        // SET_INTERFACE may cancel/re-enable a HID endpoint inside Embassy.
        // Invalidate completion proof even if the selected alternate remains 0.
        if matches!(u8::from(interface), 2 | 3) {
            self.proxy.link(false);
            self.proxy.link(self.configured && !self.suspended);
        }
    }
    fn suspended(&mut self, suspended: bool) {
        self.suspended = suspended;
        self.proxy.link(self.configured && !suspended);
    }
}
impl Handler for Control {
    fn control_out(&mut self, req: Request, data: &[u8]) -> Option<OutResponse> {
        if (req.request_type, req.recipient, req.index)
            != (RequestType::Class, Recipient::Interface, self.interface)
        {
            return None;
        }
        let accepted = match req.request {
            9 if self.kind == BootKind::Keyboard
                && req.value == 0x0200
                && req.length == 1
                && data.len() == 1 =>
            {
                self.proxy.set_leds(data[0])
            }
            10 if req.value & 255 == 0 && req.length == 0 && data.is_empty() => {
                self.proxy
                    .set_idle(self.kind, u32::from(req.value >> 8) * 4);
                true
            }
            11 if req.value <= 1 && req.length == 0 && data.is_empty() => {
                self.proxy.set_protocol(self.kind, req.value as u8);
                true
            }
            _ => false,
        };
        Some(if accepted {
            OutResponse::Accepted
        } else {
            OutResponse::Rejected
        })
    }
    fn control_in<'a>(&'a mut self, req: Request, buf: &'a mut [u8]) -> Option<InResponse<'a>> {
        if req.recipient != Recipient::Interface || req.index != self.interface {
            return None;
        }
        if req.request_type == RequestType::Standard && req.request == Request::GET_DESCRIPTOR {
            return Some(match req.value {
                0x2200 => InResponse::Accepted(self.descriptor),
                0x2100 => InResponse::Accepted(&self.hid),
                _ => InResponse::Rejected,
            });
        }
        if req.request_type != RequestType::Class {
            return None;
        }
        let n = match req.request {
            1 if req.value == 0x0100 => self.proxy.get_report(self.kind, false, buf),
            1 if req.value == 0x0200 => self.proxy.get_report(self.kind, true, buf),
            2 if req.value == 0 && req.length == 1 && !buf.is_empty() => {
                buf[0] = (self.proxy.idle(self.kind) / 4) as u8;
                Some(1)
            }
            3 if req.value == 0 && req.length == 1 && !buf.is_empty() => {
                buf[0] = self.proxy.protocol(self.kind);
                Some(1)
            }
            _ => None,
        };
        Some(match n {
            Some(n) => InResponse::Accepted(&buf[..n]),
            None => InResponse::Rejected,
        })
    }
}
fn function(
    builder: &mut Builder<'static, UsbDriver>,
    proxy: &'static Proxy,
    kind: BootKind,
    cell: &'static StaticCell<Control>,
) -> In {
    let (protocol, descriptor, mps) = match kind {
        BootKind::Keyboard => (1, KEYBOARD_DESCRIPTOR, 8),
        BootKind::Mouse => (2, MOUSE_DESCRIPTOR, 3),
    };
    let mut function = builder.function(3, 1, protocol);
    let mut interface = function.interface();
    let number = interface.interface_number();
    let mut alt = interface.alt_setting(3, 1, protocol, None);
    let len = descriptor.len() as u16;
    let hid = [9, 0x21, 0x11, 0x01, 0, 1, 0x22, len as u8, (len >> 8) as u8];
    alt.descriptor(0x21, &hid[2..]);
    let endpoint = alt.endpoint_interrupt_in(None, mps, 1);
    drop(function);
    builder.handler(cell.init(Control {
        proxy,
        kind,
        interface: u8::from(number) as u16,
        descriptor,
        hid,
    }));
    endpoint
}
pub fn add(builder: &mut Builder<'static, UsbDriver>, proxy: &'static Proxy) -> Output {
    static KEYBOARD: StaticCell<Control> = StaticCell::new();
    static MOUSE: StaticCell<Control> = StaticCell::new();
    static LINK: StaticCell<Link> = StaticCell::new();
    let keyboard = function(builder, proxy, BootKind::Keyboard, &KEYBOARD);
    let mouse = function(builder, proxy, BootKind::Mouse, &MOUSE);

    builder.handler(LINK.init(Link {
        proxy,
        configured: false,
        suspended: false,
    }));
    Output { keyboard, mouse }
}

/// Embassy 0.6's OTG EndpointIn::write returns after FIFO submission, not XFRC.
/// Wait for endpoint retirement before acknowledging a report; never retry a
/// relative report on an uncertain completion. Only the two owned HID IN
/// endpoints are observed here through read-only typed PAC. Uncertain completion
/// returns to the single device owner for a disconnect latched until board reset.
/// Logical generation changes cannot retract bytes already submitted: drain that
/// single transfer to retirement, without extending its original deadline.
async fn send(endpoint: &mut In, bytes: &[u8], proxy: &Proxy, revision: u32) -> OutputResult {
    let Some(epoch) = proxy.io_epoch() else {
        return OutputResult::Superseded;
    };
    let deadline = Instant::now() + Duration::from_millis(OUTPUT_REPORT_DEADLINE_MS);
    let index = endpoint.info().addr.index();
    let write = async {
        let mut write = pin!(endpoint.write(bytes));
        core::future::poll_fn(|cx| {
            // This single-packet driver future has not submitted any bytes
            // until it returns Ready(Ok). Cancellation here is harmless.
            if !proxy.valid(revision) {
                return core::task::Poll::Ready(Ok(false));
            }
            if Instant::now() >= deadline {
                return core::task::Poll::Ready(Err(EndpointError::Disabled));
            }
            write.as_mut().poll(cx).map(|result| result.map(|()| true))
        })
        .await
    };
    match embassy_futures::select::select(write, async {
        loop {
            if !proxy.valid(revision) || Instant::now() >= deadline {
                break;
            }
            Timer::after_micros(100).await;
        }
    })
    .await
    {
        embassy_futures::select::Either::First(Ok(true)) => {}
        embassy_futures::select::Either::First(Ok(false)) => return OutputResult::Superseded,
        embassy_futures::select::Either::First(Err(_)) => return OutputResult::Failed,
        embassy_futures::select::Either::Second(()) => {
            return if proxy.valid(revision) {
                OutputResult::Failed
            } else {
                OutputResult::Superseded
            };
        }
    }
    loop {
        // Bytes are now in the FIFO. Proxy enable/disable changes revision but
        // not the USB I/O epoch; quiesce rather than cancelling/replaying them.
        if !proxy.io_valid(epoch) || Instant::now() >= deadline {
            return OutputResult::Failed;
        }
        let control = pac::USB_OTG_FS.diepctl(index).read();
        if !control.usbaep() || control.stall() {
            return OutputResult::Failed;
        }
        if !control.epena() {
            return OutputResult::Retired;
        }
        Timer::after_micros(100).await;
    }
}

impl Output {
    /// Returns only on uncertain output. The caller must stop UsbDevice::run
    /// and disconnect; a dropped write may still have bytes in the hardware FIFO.
    pub async fn run(&mut self, proxy: &Proxy) {
        loop {
            let Some((revision, work)) = proxy.work(Instant::now().as_millis()) else {
                Timer::after_millis(1).await;
                continue;
            };
            let result = match work {
                Work::Neutral => {
                    // Try both releases independently within the same deadline.
                    let (k, m) = embassy_futures::join::join(
                        send(&mut self.keyboard, &[0; 8], proxy, revision),
                        send(&mut self.mouse, &[0; 3], proxy, revision),
                    )
                    .await;
                    k.combine(m)
                }
                Work::Report(report) => match report.kind {
                    BootKind::Keyboard => {
                        send(&mut self.keyboard, &report.bytes, proxy, revision).await
                    }
                    BootKind::Mouse => {
                        send(&mut self.mouse, &report.bytes[..3], proxy, revision).await
                    }
                },
            };
            match result {
                OutputResult::Failed => return,
                OutputResult::Superseded => {}
                OutputResult::Retired => {
                    proxy.completed(revision, work, Instant::now().as_millis())
                }
            }
        }
    }
}
