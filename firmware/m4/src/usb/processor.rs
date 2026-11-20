#![forbid(unsafe_code)]
//! Bounded topology and HID subset decoder, not an arbitrary USB class stack.
use super::hid::Layout;
use super::types::*;
use core::fmt::Write;
use heapless::Vec;

pub struct Processor {
    descriptor: Vec<u8, MAX_DESCRIPTOR_SIZE>,
    assembly: Option<(u8, u8, u16)>,
    device: Option<DeviceInfo>,
    interfaces: Vec<InterfaceInfo, 16>,
    endpoints: Vec<(EndpointInfo, u8), 32>,
    keys: [Vec<u8, 6>; 16],
    status: Option<UsbEvent>,
    lifecycle: Option<UsbEvent>,
    latest_error: Option<UsbEvent>,
    latest_diagnostic: Option<UsbEvent>,
    latest_storage: Option<UsbEvent>,
    rejected_layouts: Vec<u8, 16>,
    layouts: Vec<(u8, Layout), 4>,
    report_keys: Vec<(u8, u8, Vec<u8, 6>), 32>,
}
impl Default for Processor {
    fn default() -> Self {
        Self::new()
    }
}
impl Processor {
    pub const fn new() -> Self {
        Self {
            descriptor: Vec::new(),
            assembly: None,
            device: None,
            interfaces: Vec::new(),
            endpoints: Vec::new(),
            keys: [const { Vec::new() }; 16],
            status: None,
            lifecycle: None,
            latest_error: None,
            latest_diagnostic: None,
            latest_storage: None,
            rejected_layouts: Vec::new(),
            layouts: Vec::new(),
            report_keys: Vec::new(),
        }
    }
    fn reset(&mut self) {
        self.descriptor.clear();
        self.assembly = None;
        self.device = None;
        self.interfaces.clear();
        self.endpoints.clear();
        for keys in &mut self.keys {
            keys.clear();
        }
        self.status = None;
        self.lifecycle = None;
        self.latest_error = None;
        self.latest_diagnostic = None;
        self.latest_storage = None;
        self.rejected_layouts.clear();
        self.layouts.clear();
        self.report_keys.clear();
    }
    /// Replay retained status and partial topology, including failure diagnostics.
    /// Does not replay input, transfers, directory listings, or file contents.
    pub fn snapshot(&self, mut emit: impl FnMut(UiEvent)) {
        emit(UiEvent::Hello {
            protocol_version: shared::protocol::PROTOCOL_VERSION,
        });
        if let Some(event) = &self.status {
            emit(UiEvent::Host {
                event: event.clone(),
            });
        }
        for event in [&self.lifecycle, &self.latest_error, &self.latest_diagnostic]
            .into_iter()
            .flatten()
        {
            emit(UiEvent::Host {
                event: event.clone(),
            });
        }
        if let Some(descriptor) = self.device {
            emit(UiEvent::Device { descriptor });
        }
        for &descriptor in &self.interfaces {
            emit(UiEvent::Interface { descriptor });
        }
        for &(descriptor, _) in &self.endpoints {
            emit(UiEvent::Endpoint { descriptor });
        }
        if let Some(event) = &self.latest_storage {
            emit(UiEvent::Host {
                event: event.clone(),
            });
        }
    }
    /// Forward a host event and emit derived topology/input after bounded validation.
    /// Descriptor chunks must be contiguous and ordered; attachment changes or a new
    /// lifecycle session clear retained context, while errors preserve partial topology.
    pub fn process(&mut self, event: UsbEvent, mut emit: impl FnMut(UiEvent)) {
        match &event {
            UsbEvent::Attached { .. } | UsbEvent::Detached => {
                self.reset();
                self.status = Some(event.clone());
            }
            UsbEvent::Status { .. } | UsbEvent::Configured { .. } | UsbEvent::Ready => {
                self.status = Some(event.clone())
            }
            UsbEvent::Lifecycle { session_id, .. } => {
                if self.lifecycle.as_ref().is_some_and(|e| matches!(e, UsbEvent::Lifecycle { session_id: previous, .. } if previous != session_id)) {
                    self.reset();
                }
                self.lifecycle = Some(event.clone());
            }
            UsbEvent::Error { .. } => self.latest_error = Some(event.clone()),
            UsbEvent::Diagnostic { .. } => self.latest_diagnostic = Some(event.clone()),
            // Listings have no start/end boundary here; reads must never be replayed.
            UsbEvent::Storage { .. } => self.latest_storage = Some(event.clone()),
            _ => {}
        }
        emit(UiEvent::Host {
            event: event.clone(),
        });
        match event {
            UsbEvent::Descriptor {
                descriptor_type,
                index,
                offset,
                total_length,
                data,
            } => {
                if offset == 0 {
                    self.descriptor.clear();
                    self.assembly = Some((descriptor_type, index, total_length));
                }
                if total_length == 0
                    || usize::from(total_length) > MAX_DESCRIPTOR_SIZE
                    || self.assembly != Some((descriptor_type, index, total_length))
                    || usize::from(offset) != self.descriptor.len()
                    || data.is_empty()
                    || self.descriptor.len() + data.len() > usize::from(total_length)
                {
                    self.assembly = None;
                    self.descriptor.clear();
                    error(&mut emit, "Invalid descriptor chunk length or order");
                    return;
                }
                if self.descriptor.extend_from_slice(&data).is_err() {
                    self.assembly = None;
                    return;
                }
                if self.descriptor.len() == usize::from(total_length) {
                    self.assembly = None;
                    if !self.parse(descriptor_type, index, &mut emit) {
                        error(&mut emit, "Malformed or unsupported descriptor limits");
                    }
                }
            }
            UsbEvent::HidReport { endpoint, data } => self.hid(endpoint, &data, &mut emit),
            _ => {}
        }
    }
    fn parse(&mut self, kind: u8, index: u8, emit: &mut impl FnMut(UiEvent)) -> bool {
        let d = self.descriptor.as_slice();
        if kind == 1 {
            if d.len() != 18
                || d[0] != 18
                || d[1] != 1
                || !matches!(d[7], 8 | 16 | 32 | 64)
                || d[17] == 0
            {
                return false;
            }
            let descriptor = DeviceInfo {
                usb_version: le(d, 2),
                class: d[4],
                subclass: d[5],
                protocol: d[6],
                max_packet_size: d[7],
                vendor_id: le(d, 8),
                product_id: le(d, 10),
                device_version: le(d, 12),
                configurations: d[17],
            };
            self.device = Some(descriptor);
            emit(UiEvent::Device { descriptor });
            return true;
        }
        if kind == 0x22 {
            if !self
                .interfaces
                .iter()
                .any(|i| i.number == index && i.alternate == 0 && i.class == 3)
            {
                return false;
            }
            let layout = match Layout::parse(d) {
                Ok(layout) => layout,
                Err(reason) => {
                    self.layouts.retain(|(interface, _)| *interface != index);
                    self.report_keys
                        .retain(|(interface, _, _)| *interface != index);
                    if !self.rejected_layouts.contains(&index) {
                        let _ = self.rejected_layouts.push(index);
                        let mut message = heapless::String::new();
                        let _ = write!(message, "HID interface {index}: {reason}");
                        emit(UiEvent::Error { message });
                    }
                    return true;
                }
            };
            if let Some(entry) = self
                .layouts
                .iter_mut()
                .find(|(interface, _)| *interface == index)
            {
                entry.1 = layout;
            } else if self.layouts.push((index, layout)).is_err() {
                if !self.rejected_layouts.contains(&index) {
                    let _ = self.rejected_layouts.push(index);
                    let mut message = heapless::String::new();
                    let _ = write!(message, "HID interface {index}: Too many HID interfaces");
                    emit(UiEvent::Error { message });
                }
                return true;
            }
            self.rejected_layouts
                .retain(|interface| *interface != index);
            self.report_keys
                .retain(|(interface, _, _)| *interface != index);
            return true;
        }
        if kind != 2 {
            return true;
        }
        // Validate the entire tree before committing or emitting partial topology.
        if d.len() < 9 || d[0] < 9 || d[1] != 2 || usize::from(le(d, 2)) != d.len() {
            return false;
        }
        let mut interfaces = Vec::<InterfaceInfo, 16>::new();
        let mut endpoints = Vec::<(EndpointInfo, u8), 32>::new();
        let mut protocol = 0;
        let mut interface = 0;
        let mut alternate = 0;
        let mut expected = 0;
        let mut seen = 0;
        let mut have_interface = false;
        let mut offset = 0;
        while offset < d.len() {
            if d.len() - offset < 2 {
                return false;
            }
            let n = usize::from(d[offset]);
            if n < 2 || n > d.len() - offset {
                return false;
            }
            let t = &d[offset..offset + n];
            match t[1] {
                4 => {
                    if n < 9 || (have_interface && seen != expected) {
                        return false;
                    }
                    let info = InterfaceInfo {
                        number: t[2],
                        alternate: t[3],
                        endpoints: t[4],
                        class: t[5],
                        subclass: t[6],
                        protocol: t[7],
                    };
                    protocol = if info.class == 3 && info.subclass == 1 && info.alternate == 0 {
                        info.protocol
                    } else {
                        0
                    };
                    interface = info.number;
                    alternate = info.alternate;
                    expected = info.endpoints;
                    seen = 0;
                    have_interface = true;
                    if interfaces.push(info).is_err() {
                        return false;
                    }
                }
                5 => {
                    if n < 7
                        || !have_interface
                        || seen >= expected
                        || t[2] & 15 == 0
                        || t[2] & 0x70 != 0
                    {
                        return false;
                    }
                    seen += 1;
                    let info = EndpointInfo {
                        interface,
                        alternate,
                        address: t[2],
                        attributes: t[3],
                        max_packet_size: le(t, 4),
                        interval: t[6],
                    };
                    if endpoints
                        .iter()
                        .any(|(e, _)| e.address == info.address && e.alternate == info.alternate)
                    {
                        return false;
                    }
                    let boot = if info.address & 0x80 != 0 && info.attributes & 3 == 3 {
                        protocol
                    } else {
                        0
                    };
                    if endpoints.push((info, boot)).is_err() {
                        return false;
                    }
                }
                _ => {}
            }
            offset += n;
        }
        if !have_interface || seen != expected {
            return false;
        }
        self.layouts.clear();
        self.rejected_layouts.clear();
        self.report_keys.clear();
        self.interfaces = interfaces;
        self.endpoints = endpoints;
        for keys in &mut self.keys {
            keys.clear();
        }
        for &descriptor in &self.interfaces {
            emit(UiEvent::Interface { descriptor });
        }
        for &(descriptor, _) in &self.endpoints {
            emit(UiEvent::Endpoint { descriptor });
        }
        true
    }
    fn hid(&mut self, endpoint: u8, data: &[u8], emit: &mut impl FnMut(UiEvent)) {
        let address = endpoint | 0x80;
        let Some((info, _)) = self
            .endpoints
            .iter()
            .find(|(e, _)| e.address == address && e.alternate == 0)
            .copied()
        else {
            return;
        };
        if self.rejected_layouts.contains(&info.interface) {
            return;
        }
        if let Some((_, layout)) = self.layouts.iter().find(|(i, _)| *i == info.interface) {
            let decoded = match layout.decode(data) {
                Ok(decoded) => decoded,
                Err(message) => {
                    error(emit, message);
                    return;
                }
            };
            if decoded.keyboard {
                let slot = if let Some(slot) = self
                    .report_keys
                    .iter()
                    .position(|(i, id, _)| *i == info.interface && *id == decoded.id)
                {
                    slot
                } else {
                    if self
                        .report_keys
                        .push((info.interface, decoded.id, Vec::new()))
                        .is_err()
                    {
                        error(emit, "HID key state capacity exceeded");
                        return;
                    }
                    self.report_keys.len() - 1
                };
                let previous = &mut self.report_keys[slot].2;
                let mut pressed = Vec::new();
                let mut released = Vec::new();
                for &key in &decoded.keys {
                    if !previous.contains(&key) {
                        let _ = pressed.push(key);
                    }
                }
                for &key in previous.iter() {
                    if !decoded.keys.contains(&key) {
                        let _ = released.push(key);
                    }
                }
                *previous = decoded.keys.clone();
                emit(UiEvent::Keyboard {
                    modifiers: decoded.modifiers,
                    keys: decoded.keys,
                    pressed,
                    released,
                });
            }
            if decoded.mouse {
                emit(UiEvent::Mouse {
                    buttons: decoded.buttons,
                    dx: decoded.axes[0],
                    dy: decoded.axes[1],
                    wheel: decoded.axes[2],
                });
            }
            return;
        }
        let protocol = self
            .endpoints
            .iter()
            .find(|(e, _)| *e == info)
            .map(|(_, p)| *p)
            .unwrap_or(0);
        match protocol {
            1 => {
                if data.len() != 8 || data[1] != 0 || data[2..].iter().any(|k| (1..=3).contains(k))
                {
                    error(emit, "Invalid boot keyboard report or rollover");
                    return;
                }
                let previous = &mut self.keys[usize::from(endpoint & 15)];
                let mut keys = Vec::new();
                let mut pressed = Vec::new();
                let mut released = Vec::new();
                for &k in &data[2..] {
                    if k != 0 && !keys.contains(&k) {
                        let _ = keys.push(k);
                    }
                }
                for &k in &keys {
                    if !previous.contains(&k) {
                        let _ = pressed.push(k);
                    }
                }
                for &k in previous.iter() {
                    if !keys.contains(&k) {
                        let _ = released.push(k);
                    }
                }
                *previous = keys.clone();
                emit(UiEvent::Keyboard {
                    modifiers: data[0],
                    keys,
                    pressed,
                    released,
                });
            }
            2 => {
                if !(3..=4).contains(&data.len()) {
                    error(emit, "Invalid boot mouse report length");
                    return;
                }
                emit(UiEvent::Mouse {
                    buttons: data[0],
                    dx: data[1] as i8,
                    dy: data[2] as i8,
                    wheel: data.get(3).copied().unwrap_or(0) as i8,
                });
            }
            _ => {}
        }
    }
}
fn le(d: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([d[i], d[i + 1]])
}
fn error(emit: &mut impl FnMut(UiEvent), text: &str) {
    let mut message = heapless::String::new();
    let _ = message.push_str(text);
    emit(UiEvent::Error { message });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descriptor(
        p: &mut Processor,
        kind: u8,
        offset: u16,
        total: u16,
        data: &[u8],
    ) -> Vec<UiEvent, 16> {
        let mut events = Vec::new();
        p.process(
            UsbEvent::Descriptor {
                descriptor_type: kind,
                index: 0,
                offset,
                total_length: total,
                data: Vec::from_slice(data).unwrap(),
            },
            |e| {
                events.push(e).unwrap();
            },
        );
        events
    }
    fn boot(p: &mut Processor, protocol: u8) {
        let d = [
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 3, 1, protocol, 0, 7, 5, 0x81, 3, 8, 0,
            10,
        ];
        assert!(
            !descriptor(p, 2, 0, 25, &d)
                .iter()
                .any(|e| matches!(e, UiEvent::Error { .. }))
        );
    }
    #[test]
    fn raw_hid_packet_is_forwarded_once_without_semantic_decoding() {
        let mut p = Processor::new();
        boot(&mut p, 1);
        let packet = UsbEvent::Transfer {
            endpoint: 0x81,
            direction: Direction::In,
            bytes: 8,
            pid: 2,
            data: Vec::from_slice(&[0, 0, 4, 0, 0, 0, 0, 0]).unwrap(),
        };
        let mut events = Vec::<UiEvent, 4>::new();
        p.process(packet.clone(), |event| events.push(event).unwrap());
        assert_eq!(events.as_slice(), &[UiEvent::Host { event: packet }]);
        p.process(
            UsbEvent::HidReport {
                endpoint: 1,
                data: Vec::from_slice(&[0, 0, 4, 0, 0, 0, 0, 0]).unwrap(),
            },
            |event| events.push(event).unwrap(),
        );
        assert_eq!(events.len(), 3);
        assert!(
            matches!(&events[2], UiEvent::Keyboard { pressed, .. } if pressed.as_slice() == [4])
        );
    }

    #[test]
    fn composite_association_and_error_snapshot() {
        let mut p = Processor::new();
        let d = [
            9, 2, 41, 0, 2, 1, 0, 128, 50, 9, 4, 3, 0, 1, 3, 1, 1, 0, 7, 5, 129, 3, 8, 0, 10, 9, 4,
            7, 0, 1, 3, 1, 1, 0, 7, 5, 130, 3, 8, 0, 10,
        ];
        descriptor(&mut p, 2, 0, 41, &d);
        assert_eq!(p.endpoints[0].0.interface, 3);
        assert_eq!(p.endpoints[1].0.interface, 7);
        for endpoint in [1, 2] {
            let mut pressed = false;
            p.process(
                UsbEvent::HidReport {
                    endpoint,
                    data: Vec::from_slice(&[0, 0, 4, 0, 0, 0, 0, 0]).unwrap(),
                },
                |e| {
                    if let UiEvent::Keyboard { pressed: keys, .. } = e {
                        pressed = keys.as_slice() == [4];
                    }
                },
            );
            assert!(pressed);
        }
        p.process(UsbEvent::PortReset, |_| {});
        p.process(
            UsbEvent::Error {
                code: UsbError::Timeout,
            },
            |_| {},
        );
        descriptor(&mut p, 2, 0, 3, &[0, 2, 0]);
        let mut endpoints = 0;
        let mut error_seen = false;
        p.snapshot(|e| match e {
            UiEvent::Endpoint { .. } => endpoints += 1,
            UiEvent::Host {
                event: UsbEvent::Error { .. },
            } => error_seen = true,
            _ => {}
        });
        assert_eq!(endpoints, 2);
        assert!(error_seen);
    }

    #[test]
    fn snapshot_keeps_diagnostic_after_plain_error_until_new_session() {
        let mut p = Processor::new();
        p.process(
            UsbEvent::Lifecycle {
                state: HostState::Enumerating,
                session_id: 1,
            },
            |_| {},
        );
        let diagnostic = UsbEvent::Diagnostic {
            code: UsbError::Timeout,
            setup: [0; 8],
            session_id: 1,
            transfer_id: 9,
            stage: TransferStage::Data,
            endpoint: 0x83,
            elapsed_us: 1000,
            retries: 2,
            hcint: 1,
            hcchar: 2,
            hctsiz: 3,
            hprt: 4,
        };
        p.process(diagnostic.clone(), |_| {});
        p.process(
            UsbEvent::Error {
                code: UsbError::Stall,
            },
            |_| {},
        );
        let mut found_diagnostic = false;
        let mut found_error = false;
        p.snapshot(|event| {
            found_diagnostic |= event
                == UiEvent::Host {
                    event: diagnostic.clone(),
                };
            found_error |= matches!(
                event,
                UiEvent::Host {
                    event: UsbEvent::Error {
                        code: UsbError::Stall
                    }
                }
            );
        });
        assert!(found_diagnostic && found_error);
        p.process(
            UsbEvent::Lifecycle {
                state: HostState::Enumerating,
                session_id: 2,
            },
            |_| {},
        );
        p.snapshot(|event| {
            assert!(!matches!(
                event,
                UiEvent::Host {
                    event: UsbEvent::Diagnostic { .. } | UsbEvent::Error { .. }
                }
            ))
        });
    }
    #[test]
    fn rejected_layout_reports_reason_once_and_never_boot_decodes() {
        for subclass in [0, 1] {
            let mut p = Processor::new();
            let d = [
                9, 2, 25, 0, 1, 1, 0, 128, 50, 9, 4, 0, 0, 1, 3, subclass, 1, 0, 7, 5, 129, 3, 8,
                0, 10,
            ];
            descriptor(&mut p, 2, 0, 25, &d);
            let rejected = descriptor(&mut p, 0x22, 0, 2, &[0x75, 33]);
            assert_eq!(
                rejected
                    .iter()
                    .filter(|e| matches!(e, UiEvent::Error { .. }))
                    .count(),
                1
            );
            assert!(rejected.iter().any(|e| matches!(e, UiEvent::Error { message } if message.as_str() == "HID interface 0: HID field wider than 32 bits")));
            for _ in 0..2 {
                let report = UsbEvent::HidReport {
                    endpoint: 1,
                    data: Vec::from_slice(&[0, 0, 4, 0, 0, 0, 0, 0]).unwrap(),
                };
                let mut events = Vec::<UiEvent, 4>::new();
                p.process(report.clone(), |e| events.push(e).unwrap());
                assert_eq!(events.as_slice(), &[UiEvent::Host { event: report }]);
            }
        }
    }
    fn storage_capability() -> UsbEvent {
        UsbEvent::Storage {
            blocks: 4096,
            block_size: 512,
            vendor: heapless::String::try_from("TEST").unwrap(),
            product: heapless::String::try_from("Read-only disk").unwrap(),
            fat32: true,
        }
    }
    #[test]
    fn storage_snapshot_survives_errors_status_and_port_reset() {
        let mut p = Processor::new();
        p.process(
            UsbEvent::Lifecycle {
                state: HostState::Ready,
                session_id: 1,
            },
            |_| {},
        );
        let capability = storage_capability();
        p.process(capability.clone(), |_| {});
        for event in [
            UsbEvent::Error {
                code: UsbError::Stall,
            },
            UsbEvent::Status {
                enabled: true,
                attached: true,
                address: 1,
                transfers: 99,
                errors: 1,
                dropped: 64,
            },
            UsbEvent::PortReset,
            UsbEvent::Lifecycle {
                state: HostState::Error,
                session_id: 1,
            },
        ] {
            p.process(event, |_| {});
            let mut found = false;
            p.snapshot(|event| {
                found |= event
                    == UiEvent::Host {
                        event: capability.clone(),
                    }
            });
            assert!(found);
        }
    }
    #[test]
    fn storage_snapshot_clears_on_detach_or_new_session() {
        for detach in [false, true] {
            let mut p = Processor::new();
            p.process(
                UsbEvent::Lifecycle {
                    state: HostState::Ready,
                    session_id: 1,
                },
                |_| {},
            );
            p.process(storage_capability(), |_| {});
            let event = if detach {
                UsbEvent::Detached
            } else {
                UsbEvent::Lifecycle {
                    state: HostState::Waiting,
                    session_id: 2,
                }
            };
            p.process(event, |_| {});
            p.snapshot(|event| {
                assert!(!matches!(
                    event,
                    UiEvent::Host {
                        event: UsbEvent::Storage { .. }
                    }
                ))
            });
        }
    }
    #[test]
    fn storage_files_and_correlated_reads_are_live_only() {
        let mut p = Processor::new();
        p.process(storage_capability(), |_| {});
        for event in [
            UsbEvent::File {
                index: 0,
                name: heapless::String::try_from("TEST.TXT").unwrap(),
                size: 3,
                directory: false,
            },
            UsbEvent::FileData {
                index: 0,
                offset: 0,
                total_size: 3,
                data: Vec::from_slice(&[1, 2, 3]).unwrap(),
                eof: true,
            },
        ] {
            let mut emitted = Vec::<UiEvent, 2>::new();
            p.process(event.clone(), |event| emitted.push(event).unwrap());
            assert_eq!(emitted.as_slice(), &[UiEvent::Host { event }]);
        }
        p.snapshot(|event| {
            assert!(!matches!(
                event,
                UiEvent::Host {
                    event: UsbEvent::File { .. } | UsbEvent::FileData { .. }
                }
            ))
        });
    }
    #[test]
    fn processor_memory_budget() {
        extern crate std;
        std::println!("Processor: {} bytes", core::mem::size_of::<Processor>());
        assert!(core::mem::size_of::<Processor>() <= 12288);
    }
    #[test]
    fn malformed_and_chunks() {
        let mut p = Processor::new();
        for (offset, total, data) in [
            (1, 18, &[18, 1][..]),
            (0, 1025, &[18, 1][..]),
            (0, 3, &[0, 2, 0][..]),
        ] {
            assert!(
                descriptor(&mut p, 2, offset, total, data)
                    .iter()
                    .any(|e| matches!(e, UiEvent::Error { .. }))
            );
        }
        descriptor(&mut p, 1, 0, 18, &[18, 1, 0, 2]);
        assert!(
            descriptor(&mut p, 1, 5, 18, &[0; 13])
                .iter()
                .any(|e| matches!(e, UiEvent::Error { .. }))
        );
        let d = [
            18, 1, 0, 2, 0, 0, 0, 64, 0x34, 0x12, 0x78, 0x56, 0, 1, 0, 0, 0, 1,
        ];
        descriptor(&mut p, 1, 0, 18, &d[..8]);
        assert!(descriptor(&mut p, 1, 8, 18, &d[8..]).iter().any(
            |e| matches!(e, UiEvent::Device { descriptor } if descriptor.vendor_id == 0x1234)
        ));
    }
    #[test]
    fn keyboard_transitions_and_rollover() {
        let mut p = Processor::new();
        boot(&mut p, 1);
        let mut events = Vec::<UiEvent, 8>::new();
        for data in [
            [2, 0, 4, 0, 0, 0, 0, 0],
            [0, 0, 5, 0, 0, 0, 0, 0],
            [0, 0, 1, 0, 0, 0, 0, 0],
        ] {
            p.process(
                UsbEvent::HidReport {
                    endpoint: 1,
                    data: Vec::from_slice(&data).unwrap(),
                },
                |e| {
                    events.push(e).unwrap();
                },
            );
        }
        assert!(
            matches!(&events[1], UiEvent::Keyboard { modifiers: 2, pressed, .. } if pressed.as_slice() == [4])
        );
        assert!(
            matches!(&events[3], UiEvent::Keyboard { pressed, released, .. } if pressed.as_slice() == [5] && released.as_slice() == [4])
        );
        assert!(matches!(&events[5], UiEvent::Error { .. }));
    }
    #[test]
    fn mouse_signed_and_detach() {
        let mut p = Processor::new();
        boot(&mut p, 2);
        let mut mouse = false;
        p.process(
            UsbEvent::HidReport {
                endpoint: 0x81,
                data: Vec::from_slice(&[1, 255, 128, 127]).unwrap(),
            },
            |e| {
                mouse |= matches!(
                    e,
                    UiEvent::Mouse {
                        dx: -1,
                        dy: -128,
                        wheel: 127,
                        ..
                    }
                );
            },
        );
        assert!(mouse);
        p.process(UsbEvent::Detached, |_| {});
        p.process(
            UsbEvent::HidReport {
                endpoint: 1,
                data: Vec::from_slice(&[1, 2, 3]).unwrap(),
            },
            |e| assert!(!matches!(e, UiEvent::Mouse { .. })),
        );
    }
}
