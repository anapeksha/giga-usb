//! Request-local filtering of retained host-side USB trace records.
use core::fmt::Write;
use heapless::String;
use serde::Deserialize;
use shared::usb::types::{Direction, TransferKind, UiEvent, UiRecord, UsbEvent};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TraceDirection {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TraceKind {
    Control,
    Interrupt,
    Bulk,
}

/// Omitted filters are unrestricted; `after` defaults to zero and `errors` to false.
/// Search is UTF-8 (at most 96 bytes before trimming), with ASCII-only case folding.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceQuery {
    #[serde(default)]
    pub after: u32,
    pub endpoint: Option<u8>,
    pub direction: Option<TraceDirection>,
    pub kind: Option<TraceKind>,
    #[serde(default)]
    pub errors: bool,
    pub q: Option<String<96>>,
}

pub(crate) fn decode(bytes: &[u8]) -> Option<UiRecord> {
    // UiRecord is serialize-only; mirror only the host envelope on M7.
    #[derive(Deserialize)]
    struct Envelope {
        timestamp_ms: u64,
        event: HostEnvelope,
    }
    #[derive(Deserialize)]
    struct HostEnvelope {
        #[serde(rename = "type")]
        tag: String<32>,
        event: Option<UsbEvent>,
    }
    let (record, used): (Envelope, usize) = serde_json_core::from_slice(bytes).ok()?;
    if used != bytes.len() || record.event.tag != "host" {
        return None;
    }
    Some(UiRecord {
        timestamp_ms: record.timestamp_ms,
        event: UiEvent::Host {
            event: record.event.event?,
        },
    })
}

fn host(record: &UiRecord) -> Option<&UsbEvent> {
    match &record.event {
        UiEvent::Host { event } => Some(event),
        _ => None,
    }
}

fn pair(context: &UsbEvent, payload: &UsbEvent) -> bool {
    let UsbEvent::TransferContext {
        endpoint,
        session_id,
        transfer_id,
        stage,
        ..
    } = context
    else {
        return false;
    };
    match payload {
        UsbEvent::Transfer {
            endpoint: other, ..
        } => endpoint == other,
        UsbEvent::Diagnostic {
            endpoint: other,
            session_id: session,
            transfer_id: transfer,
            stage: other_stage,
            ..
        } => {
            endpoint == other
                && session_id == session
                && transfer_id == transfer
                && stage == other_stage
        }
        _ => false,
    }
}

impl TraceQuery {
    /// Neighbors must be immediately adjacent in the original, unfiltered bus history.
    pub(crate) fn matches(
        &self,
        record: &UiRecord,
        previous: Option<&UiRecord>,
        next: Option<&UiRecord>,
    ) -> bool {
        let Some(event) = host(record) else {
            return false;
        };
        let partner = match event {
            UsbEvent::TransferContext { .. } => {
                next.and_then(host).filter(|next| pair(event, next))
            }
            UsbEvent::Transfer { .. } | UsbEvent::Diagnostic { .. } => {
                previous.and_then(host).filter(|prev| pair(prev, event))
            }
            UsbEvent::Error { .. } => None,
            _ => return false,
        };
        if self.errors
            && !matches!(event, UsbEvent::Diagnostic { .. } | UsbEvent::Error { .. })
            && !matches!(partner, Some(UsbEvent::Diagnostic { .. }))
        {
            return false;
        }
        let endpoint = match event {
            UsbEvent::Transfer { endpoint, .. }
            | UsbEvent::TransferContext { endpoint, .. }
            | UsbEvent::Diagnostic { endpoint, .. } => Some(*endpoint),
            _ => None,
        };
        if self.endpoint.is_some() && self.endpoint != endpoint {
            return false;
        }
        let direction = match event {
            UsbEvent::Transfer { direction, .. } => Some(*direction),
            _ => match partner {
                Some(UsbEvent::Transfer { direction, .. }) => Some(*direction),
                _ => endpoint.map(|ep| {
                    if ep & 128 != 0 {
                        Direction::In
                    } else {
                        Direction::Out
                    }
                }),
            },
        };
        if let Some(wanted) = self.direction
            && direction
                != Some(match wanted {
                    TraceDirection::In => Direction::In,
                    TraceDirection::Out => Direction::Out,
                })
        {
            return false;
        }
        let kind = match event {
            UsbEvent::TransferContext { kind, .. } => Some(*kind),
            _ => match partner {
                Some(UsbEvent::TransferContext { kind, .. }) => Some(*kind),
                _ => None,
            },
        };
        if let Some(wanted) = self.kind
            && kind
                != Some(match wanted {
                    TraceKind::Control => TransferKind::Control,
                    TraceKind::Interrupt => TransferKind::Interrupt,
                    TraceKind::Bulk => TransferKind::Bulk,
                })
        {
            return false;
        }
        self.q.as_ref().is_none_or(|q| {
            let q = q.trim();
            searchable(event, q) || partner.is_some_and(|p| searchable(p, q))
        })
    }
}

fn contains(text: &str, query: &str) -> bool {
    query.is_empty()
        || text
            .as_bytes()
            .windows(query.len())
            .any(|part| part.eq_ignore_ascii_case(query.as_bytes()))
}

fn searchable(event: &UsbEvent, query: &str) -> bool {
    // Debug names expose typed diagnostic codes and fields without copying raw JSON.
    contains(&search_text(event), query)
}

fn search_text(event: &UsbEvent) -> String<1024> {
    let mut text = String::<1024>::new();
    let _ = write!(text, "{event:?}");
    match event {
        UsbEvent::Transfer {
            endpoint,
            direction,
            data,
            pid,
            ..
        } => {
            let _ = write!(
                text,
                " {} endpoint {} ",
                if *direction == Direction::In {
                    "Received"
                } else {
                    "Sent"
                },
                endpoint
            );
            let _ = write!(
                text,
                " {} · endpoint {} ",
                if *direction == Direction::In {
                    "Received"
                } else {
                    "Sent"
                },
                endpoint
            );
            hex(&mut text, data);
            if *pid == 3 && data.len() == 8 {
                setup(&mut text, data);
            }
        }
        UsbEvent::TransferContext { .. } => {
            let _ = text.push_str(" transfer context ");
        }
        UsbEvent::Diagnostic {
            setup: bytes,
            code,
            stage,
            ..
        } => {
            let code = error_label(*code);
            let _ = write!(text, " {code} · {stage:?} ");
            hex(&mut text, bytes);
            if bytes.iter().any(|byte| *byte != 0) {
                setup(&mut text, bytes);
            }
        }
        UsbEvent::Error { code } => {
            let _ = text.push_str(error_label(*code));
        }
        _ => {}
    }
    text
}
fn error_label(code: shared::usb::types::UsbError) -> &'static str {
    use shared::usb::types::UsbError;
    match code {
        UsbError::Timeout => "timeout",
        UsbError::Stall => "stall",
        UsbError::Nak => "nak",
        UsbError::Disconnected => "disconnected",
        UsbError::Babble => "babble",
        UsbError::Transaction => "transaction",
        UsbError::Overflow => "overflow",
        UsbError::InvalidDescriptor => "invalid descriptor",
        UsbError::Unsupported => "unsupported",
        UsbError::Controller => "controller",
    }
}

fn hex(text: &mut String<1024>, bytes: &[u8]) {
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    let _ = text.push(' ');
    for byte in bytes {
        let _ = write!(text, "{byte:02x} ");
    }
}
fn setup(text: &mut String<1024>, bytes: &[u8]) {
    let name = match (bytes[0] & 0x60, bytes[1]) {
        (0, 0) => "GET_STATUS",
        (0, 1) => "CLEAR_FEATURE",
        (0, 3) => "SET_FEATURE",
        (0, 5) => "SET_ADDRESS",
        (0, 6) => "GET_DESCRIPTOR",
        (0, 7) => "SET_DESCRIPTOR",
        (0, 8) => "GET_CONFIGURATION",
        (0, 9) => "SET_CONFIGURATION",
        (0, 10) => "GET_INTERFACE",
        (0, 11) => "SET_INTERFACE",
        (0, 12) => "SYNCH_FRAME",
        (0x20, 1) => "GET_REPORT",
        (0x20, 2) => "GET_IDLE",
        (0x20, 3) => "GET_PROTOCOL",
        (0x20, 9) => "SET_REPORT",
        (0x20, 10) => "SET_IDLE",
        (0x20, 11) => "SET_PROTOCOL",
        (0x20, 254) => "GET_MAX_LUN",
        (0x20, 255) => "BULK_ONLY_RESET",
        _ => "",
    };
    let alias = match (bytes[0] & 0x60, bytes[1]) {
        (0, 12) => "Synchronize frame",
        (0x20, 255) => "Storage reset",
        _ => "",
    };
    let _ = write!(text, " {alias} {name} ");
    for part in name.split('_') {
        let _ = write!(text, "{part} ");
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::network_bus::Bus;
    use shared::{
        protocol::{Event, Message},
        usb::types::{TransferStage, UsbError},
    };

    pub(crate) fn send(bus: &Bus, event: UiEvent) {
        let record = UiRecord {
            timestamp_ms: 42,
            event,
        };
        send_record(bus, &record);
    }
    fn send_record(bus: &Bus, record: &UiRecord) {
        let mut bytes = [0; 1024];
        let size = serde_json_core::to_slice(record, &mut bytes).unwrap();
        let chunks = bytes[..size].chunks(240);
        let total = chunks.len();
        for (i, chunk) in chunks.enumerate() {
            let mut frame = heapless::Vec::<u8, 256>::new();
            frame
                .push(u8::from(i == 0) | (u8::from(i + 1 == total) << 1))
                .unwrap();
            frame.extend_from_slice(chunk).unwrap();
            bus.receive(&Message::with_payload(Event::Ui, &frame).unwrap());
        }
    }
    fn host(bus: &Bus, event: UsbEvent) {
        send(bus, UiEvent::Host { event });
    }
    pub(crate) fn context(endpoint: u8) -> UsbEvent {
        UsbEvent::TransferContext {
            transfer_id: 1,
            session_id: 2,
            endpoint,
            kind: TransferKind::Control,
            stage: TransferStage::Setup,
            elapsed_us: 1,
            retries: 0,
        }
    }
    pub(crate) fn transfer(endpoint: u8) -> UsbEvent {
        UsbEvent::Transfer {
            endpoint,
            direction: if endpoint & 128 == 0 {
                Direction::Out
            } else {
                Direction::In
            },
            bytes: 8,
            pid: 3,
            data: heapless::Vec::from_slice(&[0x80, 6, 0, 1, 0, 0, 18, 0]).unwrap(),
        }
    }
    fn query(q: &str) -> TraceQuery {
        TraceQuery {
            q: Some(String::try_from(q).unwrap()),
            ..Default::default()
        }
    }
    #[test]
    fn exact_scope_and_paired_search() {
        let bus = Bus::new();
        host(&bus, context(0));
        host(&bus, transfer(0));
        host(&bus, context(128));
        host(&bus, transfer(128));
        for text in [
            "get_descriptor",
            "GET DESCRIPTOR",
            "8006000100001200",
            "80 06 00 01",
            "sent endpoint 0",
        ] {
            let mut q = query(text);
            q.endpoint = Some(0);
            q.kind = Some(TraceKind::Control);
            q.direction = Some(TraceDirection::Out);
            let page = bus.trace(&q);
            assert!(page.contains("\"sequence\":1"), "{text}: {page}");
            assert!(page.contains("\"sequence\":2"));
            assert!(!page.contains("\"sequence\":3"));
        }
        let mut q = query("received endpoint 128");
        q.endpoint = Some(128);
        q.direction = Some(TraceDirection::In);
        assert!(bus.trace(&q).contains("\"sequence\":4"));
        q.kind = Some(TraceKind::Bulk);
        assert!(bus.trace(&q).contains("\"records\":[]"));
    }
    #[test]
    fn unmatched_tail_context_waits_for_searched_payload() {
        let bus = Bus::new();
        host(&bus, context(0));
        let q = query("get descriptor");
        let waiting = bus.trace(&q);
        assert_eq!(waiting, "{\"records\":[],\"cursor\":0,\"dropped\":0}");
        assert_eq!(bus.trace(&q), waiting);
        host(&bus, transfer(0));
        let page = bus.trace(&q);
        assert!(page.contains("\"sequence\":1,"));
        assert!(page.contains("\"sequence\":2,"));
        assert!(page.contains("\"cursor\":2"));
        // A matching context need not wait for a following record.
        host(&bus, context(0));
        assert!(
            bus.trace(&query("transfer context"))
                .contains("\"sequence\":3,")
        );
    }

    #[test]
    fn errors_only_tail_context_waits_for_diagnostic() {
        let bus = Bus::new();
        host(&bus, context(0));
        let q = TraceQuery {
            errors: true,
            ..Default::default()
        };
        assert_eq!(bus.trace(&q), "{\"records\":[],\"cursor\":0,\"dropped\":0}");
        host(
            &bus,
            UsbEvent::Diagnostic {
                code: UsbError::Timeout,
                setup: [0; 8],
                session_id: 2,
                transfer_id: 1,
                stage: TransferStage::Setup,
                endpoint: 0,
                elapsed_us: 1,
                retries: 0,
                hcint: 0,
                hcchar: 0,
                hctsiz: 0,
                hprt: 0,
            },
        );
        let page = bus.trace(&q);
        assert!(page.contains("\"sequence\":1,"));
        assert!(page.contains("\"sequence\":2,"));
        assert!(page.contains("\"cursor\":2"));
    }

    #[test]
    fn adjacency_and_page_boundaries() {
        let bus = Bus::new();
        for _ in 0..2 {
            host(
                &bus,
                UsbEvent::Error {
                    code: UsbError::Stall,
                },
            );
        }
        host(&bus, context(0));
        host(&bus, transfer(0));
        let page = bus.trace(&TraceQuery::default());
        assert!(page.contains("\"cursor\":3"));
        let mut q = query("get_descriptor");
        q.after = 3;
        q.kind = Some(TraceKind::Control);
        assert!(bus.trace(&q).contains("\"sequence\":4"));
        host(&bus, context(0));
        send(
            &bus,
            UiEvent::Error {
                message: String::try_from("not USB").unwrap(),
            },
        );
        host(&bus, transfer(0));
        q.after = 4;
        assert!(bus.trace(&q).contains("\"records\":[]"));
        q.kind = None;
        assert!(bus.trace(&q).contains("\"sequence\":7"));
        assert!(!bus.trace(&q).contains("\"sequence\":5"));
    }
    #[test]
    fn errors_and_diagnostic_fields() {
        let bus = Bus::new();
        host(&bus, context(0));
        host(
            &bus,
            UsbEvent::Diagnostic {
                code: UsbError::Timeout,
                setup: [0x21, 255, 0, 0, 0, 0, 0, 0],
                session_id: 2,
                transfer_id: 1,
                stage: TransferStage::Setup,
                endpoint: 0,
                elapsed_us: 10,
                retries: 2,
                hcint: 4,
                hcchar: 0,
                hctsiz: 0,
                hprt: 0,
            },
        );
        host(
            &bus,
            UsbEvent::Error {
                code: UsbError::Stall,
            },
        );
        host(&bus, transfer(0));
        let q = TraceQuery {
            errors: true,
            ..Default::default()
        };
        let page = bus.trace(&q);
        for expected in ["\"sequence\":1", "\"sequence\":2", "\"sequence\":3"] {
            assert!(page.contains(expected));
        }
        assert!(!page.contains("\"sequence\":4"));
        assert!(
            bus.trace(&query("bulk_only_reset"))
                .contains("\"sequence\":1")
        );
        assert!(bus.trace(&query("hcint: 4")).contains("\"sequence\":2"));
        let q = TraceQuery {
            endpoint: Some(0),
            errors: true,
            ..Default::default()
        };
        assert!(!bus.trace(&q).contains("\"sequence\":3"));
    }
    #[test]
    fn late_matches_no_matches_and_retention() {
        let bus = Bus::new();
        for _ in 0..68 {
            send(
                &bus,
                UiEvent::Hello {
                    protocol_version: 1,
                },
            );
        }
        host(&bus, transfer(128));
        let page = bus.trace(&TraceQuery::default());
        assert!(page.contains("\"sequence\":69"));
        assert!(page.contains("\"dropped\":5"));
        let mut q = query("absent");
        assert!(bus.trace(&q).contains("\"cursor\":69"));
        q.after = 69;
        assert!(bus.trace(&q).contains("\"dropped\":0"));
        q.after = 100;
        assert!(bus.trace(&q).contains("\"dropped\":0"));
    }
    #[test]
    fn trimmed_search_and_human_labels() {
        let bus = Bus::new();
        host(&bus, context(0));
        host(&bus, transfer(0));
        for q in [
            " \tGET DESCRIPTOR\n",
            " transfer context ",
            "Sent · endpoint 0",
        ] {
            let page = bus.trace(&query(q));
            assert!(page.contains("\"sequence\":1"), "{q}: {page}");
            assert!(page.contains("\"sequence\":2"));
        }
        assert_eq!(
            bus.trace(&query(" \t\n")),
            bus.trace(&TraceQuery::default())
        );
        for (request_type, request, label) in [
            (0, 12, "Synchronize frame"),
            (0x21, 255, "Storage reset"),
            (0xa1, 254, "Get max LUN"),
        ] {
            let event = UsbEvent::Transfer {
                endpoint: 0,
                direction: Direction::Out,
                bytes: 8,
                pid: 3,
                data: heapless::Vec::from_slice(&[request_type, request, 0, 0, 0, 0, 0, 0])
                    .unwrap(),
            };
            assert!(searchable(&event, label));
        }
        let diagnostic = UsbEvent::Diagnostic {
            code: UsbError::InvalidDescriptor,
            setup: [0; 8],
            session_id: 0,
            transfer_id: 0,
            stage: TransferStage::Data,
            endpoint: 0,
            elapsed_us: 0,
            retries: 0,
            hcint: 0,
            hcchar: 0,
            hctsiz: 0,
            hprt: 0,
        };
        assert!(searchable(&diagnostic, "invalid descriptor · data"));
        assert!(searchable(
            &UsbEvent::Error {
                code: UsbError::InvalidDescriptor
            },
            "invalid descriptor"
        ));
    }

    #[test]
    fn idle_diagnostic_does_not_invent_a_get_status_request() {
        let diagnostic = UsbEvent::Diagnostic {
            code: UsbError::Controller,
            setup: [0; 8],
            session_id: 1,
            transfer_id: 0,
            stage: TransferStage::Data,
            endpoint: 0,
            elapsed_us: 10,
            retries: 0,
            hcint: 0,
            hcchar: 0,
            hctsiz: 0,
            hprt: 0,
        };
        assert!(searchable(&diagnostic, "controller"));
        assert!(!searchable(&diagnostic, "get status"));
        assert!(!searchable(&diagnostic, "GET_STATUS"));
    }

    fn maximum_transfer() -> UsbEvent {
        UsbEvent::Transfer {
            endpoint: u8::MAX,
            direction: Direction::In,
            bytes: u16::MAX,
            pid: u8::MAX,
            data: heapless::Vec::from_slice(&[255; 64]).unwrap(),
        }
    }

    #[test]
    fn maximum_payload_search_is_complete_in_1024_bytes() {
        let event = maximum_transfer();
        let text = search_text(&event);
        let mut expected = String::<2048>::new();
        write!(
            expected,
            "{event:?} Received endpoint 255  Received · endpoint 255 "
        )
        .unwrap();
        for _ in 0..64 {
            expected.push_str("ff").unwrap();
        }
        expected.push(' ').unwrap();
        for _ in 0..64 {
            expected.push_str("ff ").unwrap();
        }
        assert!(expected.len() < 1024);
        assert_eq!(text.as_str(), expected.as_str());
        assert!(searchable(&event, "ff ff ff ff ff ff ff ff"));
        let bus = Bus::new();
        host(&bus, event);
        assert!(
            bus.trace(&query(" FF FF FF FF "))
                .contains("\"sequence\":1")
        );
    }

    #[test]
    fn three_maximum_trace_records_are_complete_json_below_capacity() {
        #[derive(Deserialize)]
        struct Page {
            records: heapless::Vec<Entry, 3>,
            cursor: u32,
            dropped: u32,
        }
        #[derive(Deserialize)]
        struct Entry {
            sequence: u32,
            record: Record,
        }
        #[derive(Deserialize)]
        struct Record {
            timestamp_ms: u64,
            event: Host,
        }
        #[derive(Deserialize)]
        struct Host {
            #[serde(rename = "type")]
            tag: String<8>,
            event: UsbEvent,
        }
        for event in [
            maximum_transfer(),
            UsbEvent::Diagnostic {
                code: UsbError::InvalidDescriptor,
                setup: [255; 8],
                session_id: u32::MAX,
                transfer_id: u32::MAX,
                stage: TransferStage::Status,
                endpoint: u8::MAX,
                elapsed_us: u64::MAX,
                retries: u32::MAX,
                hcint: u32::MAX,
                hcchar: u32::MAX,
                hctsiz: u32::MAX,
                hprt: u32::MAX,
            },
            UsbEvent::TransferContext {
                session_id: u32::MAX,
                transfer_id: u32::MAX,
                endpoint: u8::MAX,
                kind: TransferKind::Interrupt,
                stage: TransferStage::Status,
                elapsed_us: u64::MAX,
                retries: u32::MAX,
            },
            UsbEvent::Error {
                code: UsbError::InvalidDescriptor,
            },
        ] {
            let bus = Bus::new();
            let record = UiRecord {
                timestamp_ms: u64::MAX,
                event: UiEvent::Host {
                    event: event.clone(),
                },
            };
            for _ in 0..3 {
                send_record(&bus, &record);
            }
            let json = bus.trace(&TraceQuery::default());
            assert!(json.len() < 4096);
            // Also budget the widest possible sequence/cursor/drop decimal fields.
            assert!(json.len() + 5 * 9 < 4096);
            let (page, used) = serde_json_core::from_slice::<Page>(json.as_bytes()).unwrap();
            assert_eq!(used, json.len());
            assert_eq!(page.records.len(), 3);
            assert_eq!(page.cursor, 3);
            assert_eq!(page.dropped, 0);
            for (index, entry) in page.records.iter().enumerate() {
                assert_eq!(entry.sequence, index as u32 + 1);
                assert_eq!(entry.record.timestamp_ms, u64::MAX);
                assert_eq!(entry.record.event.tag, "host");
                assert_eq!(entry.record.event.event, event);
            }
        }
    }

    #[test]
    fn non_host_envelopes_cannot_supply_trace_events() {
        assert!(decode(b"{\"timestamp_ms\":0,\"event\":{\"type\":\"error\",\"event\":{\"error\":{\"code\":\"stall\"}}}}").is_none());
    }

    #[test]
    fn strict_query_and_utf8_limits() {
        for json in [
            b"{\"direction\":\"sideways\"}".as_slice(),
            b"{\"kind\":\"iso\"}",
            b"{\"endpoint\":256}",
            b"{\"errors\":\"yes\"}",
            b"{\"unknown\":1}",
            b"{\"q\":\"\xff\"}",
        ] {
            assert!(serde_json_core::from_slice::<TraceQuery>(json).is_err());
        }
        let mut json = String::<256>::new();
        json.push_str("{\"q\":\"").unwrap();
        for _ in 0..48 {
            json.push('é').unwrap();
        }
        json.push_str("\"}").unwrap();
        assert!(serde_json_core::from_slice::<TraceQuery>(json.as_bytes()).is_ok());
        json.clear();
        json.push_str("{\"q\":\"").unwrap();
        for _ in 0..49 {
            json.push('é').unwrap();
        }
        json.push_str("\"}").unwrap();
        assert!(serde_json_core::from_slice::<TraceQuery>(json.as_bytes()).is_err());
    }
}
