//! Bounded M7 HTTP/IPC bridge, accessed only by cooperative M7 tasks.

use crate::status::{Input, NetworkMode};
use core::{cell::RefCell, fmt::Write};
use heapless::{Deque, String, Vec};
use shared::{
    protocol::Message,
    usb::types::{HostCommand, NetworkInfo, UsbEvent},
};

pub use shared::protocol::RECORD_BYTES;
const HISTORY: usize = 64;
/// Single-executor command queue and complete-record history; not cross-core synchronization.
pub struct Bus(RefCell<State>);
struct State {
    commands: Deque<HostCommand, 4>,
    records: Deque<(u32, Vec<u8, RECORD_BYTES>), HISTORY>,
    assembly: Vec<u8, RECORD_BYTES>,
    assembling: bool,
    cursor: u32,
    dropped: u32,
    network: Option<NetworkInfo>,
    led: Input,
}
impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}
impl Bus {
    pub const fn new() -> Self {
        Self(RefCell::new(State {
            commands: Deque::new(),
            records: Deque::new(),
            assembly: Vec::new(),
            assembling: false,
            cursor: 0,
            dropped: 0,
            network: None,
            led: Input::new(),
        }))
    }
    pub fn set_network(&self, info: NetworkInfo) {
        let mut state = self.0.borrow_mut();
        state.led.network = if info.error.is_some() || info.mode.as_str() == "error" {
            Some(NetworkMode::Error)
        } else if info.mode.as_str() == "setup" && info.ip.is_some() {
            Some(NetworkMode::AccessPoint)
        } else {
            None
        };
        state.network = Some(info);
    }

    pub fn led_snapshot(&self) -> Input {
        self.0.borrow().led
    }

    pub fn observe_host(&self, event: &UsbEvent, now_ms: u64) {
        self.0.borrow_mut().led.observe(event, now_ms);
    }

    pub fn network(&self) -> Option<NetworkInfo> {
        self.0.borrow().network.clone()
    }

    /// Queue a command without waiting; `true` means accepted, not completed.
    /// On `false` the caller must retry or report backpressure.
    pub fn command(&self, command: HostCommand) -> bool {
        self.0.borrow_mut().commands.push_back(command).is_ok()
    }
    /// Publish a bounded generic diagnostic without observing a USB host event.
    pub fn publish_ipc_error(&self, total: u32, timestamp_ms: u64) -> bool {
        let mut message = String::new();
        let _ = write!(message, "IPC transport errors: {total} (cumulative)");
        let record = shared::usb::types::UiRecord {
            timestamp_ms,
            event: shared::usb::types::UiEvent::Error { message },
        };
        let mut bytes = Vec::<u8, RECORD_BYTES>::new();
        let mut buffer = [0; RECORD_BYTES];
        let Ok(len) = serde_json_core::to_slice(&record, &mut buffer) else {
            return false;
        };
        if bytes.extend_from_slice(&buffer[..len]).is_err() {
            return false;
        }
        let mut state = self.0.borrow_mut();
        state.cursor = state.cursor.wrapping_add(1);
        let cursor = state.cursor;
        if state.records.is_full() {
            state.records.pop_front();
        }
        state.records.push_back((cursor, bytes)).is_ok()
    }

    pub fn next_command(&self) -> Option<HostCommand> {
        self.0.borrow_mut().commands.pop_front()
    }
    /// Chunk header flags: bit 0 starts, bit 1 finishes. Frames are ordered and
    /// retried on IPC backpressure; an incomplete record is never published.
    pub fn receive(&self, message: &Message) {
        let Some(bytes) = &message.value else { return };
        let Some((&flags, bytes)) = bytes.split_first() else {
            return;
        };
        let mut s = self.0.borrow_mut();
        if flags & 1 != 0 {
            s.assembly.clear();
            s.assembling = true;
        }
        if !s.assembling {
            return;
        }
        if s.assembly.extend_from_slice(bytes).is_err() {
            s.assembling = false;
            s.dropped = s.dropped.saturating_add(1);
            return;
        }
        if flags & 2 != 0 {
            s.assembling = false;
            s.cursor = s.cursor.wrapping_add(1);
            let cursor = s.cursor;
            let record = core::mem::take(&mut s.assembly);
            if s.records.is_full() {
                s.records.pop_front();
            }
            let _ = s.records.push_back((cursor, record));
        }
    }
    /// Filter retained history without changing event delivery or counting exclusions as loss.
    pub fn trace(&self, query: &crate::trace_query::TraceQuery) -> String<4096> {
        let s = self.0.borrow();
        let age = s.cursor.wrapping_sub(query.after);
        let restart = age > i32::MAX as u32;
        let missed = if restart {
            0
        } else {
            age.saturating_sub(s.records.len() as u32)
        };
        let mut out = String::new();
        let _ = out.push_str("{\"records\":[");
        let mut count = 0;
        let mut cursor = if restart || missed != 0 {
            s.records
                .front()
                .map_or(s.cursor, |(id, _)| id.wrapping_sub(1))
        } else {
            query.after
        };
        for (index, (id, bytes)) in s.records.iter().enumerate() {
            let distance = id.wrapping_sub(query.after);
            if !restart && (distance == 0 || distance > i32::MAX as u32) {
                continue;
            }
            let (matches, defer) =
                crate::trace_query::decode(bytes).map_or((false, false), |record| {
                    let forward = matches!(
                        &record.event,
                        shared::usb::types::UiEvent::Host {
                            event: UsbEvent::TransferContext { .. }
                        }
                    );
                    let neighbor_index = if forward {
                        Some(index + 1)
                    } else {
                        index.checked_sub(1)
                    };
                    let neighbor = neighbor_index
                        .and_then(|i| s.records.get(i))
                        .and_then(|(_, b)| crate::trace_query::decode(b));
                    let matches = if forward {
                        query.matches(&record, None, neighbor.as_ref())
                    } else {
                        query.matches(&record, neighbor.as_ref(), None)
                    };
                    (matches, !matches && forward && index + 1 == s.records.len())
                });
            // A later payload/diagnostic can make this context match. Keep it
            // eligible for the next poll, but never infer a context from invalid JSON.
            if defer {
                break;
            }
            if matches {
                if count == 3 {
                    break;
                }
                if count != 0 {
                    let _ = out.push(',');
                }
                let _ = write!(out, "{{\"sequence\":{id},\"record\":");
                // Successfully decoded JSON is valid UTF-8 and bounded by RECORD_BYTES.
                let _ = out.push_str(core::str::from_utf8(bytes).unwrap_or("null"));
                let _ = out.push('}');
                count += 1;
            }
            cursor = *id;
        }
        let _ = write!(
            out,
            "],\"cursor\":{},\"dropped\":{}}}",
            cursor,
            missed.saturating_add(s.dropped)
        );
        out
    }

    /// At most three records per page; callers continue from the returned cursor.
    /// JSON `dropped` combines history missed by this query with cumulative record
    /// assembly overflows. It is not a per-page delta or a host/M4 capture-drop counter;
    /// do not repeatedly sum it across polls.
    pub fn events(&self, after: u32) -> String<4096> {
        let s = self.0.borrow();
        let mut out = String::new();
        let _ = out.push_str("{\"records\":[");
        let mut cursor = after;
        let mut count = 0;
        let age = s.cursor.wrapping_sub(after);
        // A future cursor (including a previous boot) restarts at retained history.
        let restart = age > i32::MAX as u32;
        let missed = if restart {
            0
        } else {
            age.saturating_sub(s.records.len() as u32)
        };
        for (id, bytes) in &s.records {
            if !restart && (id.wrapping_sub(after) == 0 || id.wrapping_sub(after) > i32::MAX as u32)
            {
                continue;
            }
            if count == 3 {
                break;
            }
            if let Ok(text) = core::str::from_utf8(bytes) {
                if count != 0 {
                    let _ = out.push(',');
                }
                let _ = out.push_str(text);
                count += 1;
                cursor = *id;
            }
        }
        if count == 0 {
            cursor = s.cursor;
        }
        let _ = write!(
            out,
            "],\"cursor\":{},\"dropped\":{}}}",
            cursor,
            missed.saturating_add(s.dropped)
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::protocol::Event;
    #[test]
    fn ap_network_diagnostics_and_led_survive_status_requests() {
        let bus = Bus::new();
        let setup = NetworkInfo {
            mode: String::try_from("setup").unwrap(),
            mac: [2, 1, 2, 3, 4, 5],
            ip: Some(String::try_from("192.168.4.1").unwrap()),
            error: None,
        };
        bus.set_network(setup.clone());
        assert_eq!(bus.led_snapshot().network, Some(NetworkMode::AccessPoint));
        assert!(bus.command(HostCommand::Status));
        assert_eq!(bus.next_command(), Some(HostCommand::Status));
        let current = bus.network().unwrap();
        assert_eq!(current.mac, setup.mac);
        assert_eq!(current.ip, setup.ip);
        assert_eq!(current.mode, "setup");
        bus.set_network(NetworkInfo {
            mode: String::try_from("error").unwrap(),
            mac: [0; 6],
            ip: None,
            error: Some(String::try_from("Wi-Fi SDIO startup failed").unwrap()),
        });
        assert_eq!(bus.led_snapshot().network, Some(NetworkMode::Error));
        assert!(bus.network().unwrap().error.is_some());
    }

    #[test]
    fn ap_led_requires_an_address_and_no_network_error() {
        let bus = Bus::new();
        let mut info = NetworkInfo {
            mode: String::try_from("setup").unwrap(),
            mac: [0; 6],
            ip: None,
            error: None,
        };
        bus.set_network(info.clone());
        assert_eq!(bus.led_snapshot().network, None);
        info.ip = Some(String::try_from("192.168.4.1").unwrap());
        bus.set_network(info.clone());
        assert_eq!(bus.led_snapshot().network, Some(NetworkMode::AccessPoint));
        info.error = Some(String::try_from("Network unavailable").unwrap());
        bus.set_network(info.clone());
        assert_eq!(bus.led_snapshot().network, Some(NetworkMode::Error));
        info.error = None;
        bus.set_network(info.clone());
        assert_eq!(bus.led_snapshot().network, Some(NetworkMode::AccessPoint));
        for mode in ["connecting", "connected"] {
            info.mode = String::try_from(mode).unwrap();
            bus.set_network(info.clone());
            assert_eq!(bus.led_snapshot().network, None);
        }
    }

    #[test]
    fn bounded_commands_overflow_recovery_and_wrap() {
        let bus = Bus::new();
        for _ in 0..4 {
            assert!(bus.command(HostCommand::Status));
        }
        assert!(!bus.command(HostCommand::Start));
        for _ in 0..4 {
            assert_eq!(bus.next_command(), Some(HostCommand::Status));
        }
        assert_eq!(bus.next_command(), None);
        let mut bytes = [255; 256];
        bytes[0] = 1;
        bus.receive(&Message::with_payload(Event::Ui, &bytes).unwrap());
        bytes[0] = 0;
        for _ in 0..4 {
            bus.receive(&Message::with_payload(Event::Ui, &bytes).unwrap());
        }
        assert!(bus.events(0).contains("\"dropped\":1"));
        bus.0.borrow_mut().cursor = u32::MAX - 1;
        bus.receive(&Message::with_payload(Event::Ui, b"\x03{}").unwrap());
        bus.receive(&Message::with_payload(Event::Ui, b"\x03{}").unwrap());
        let page = bus.events(u32::MAX - 1);
        assert!(page.contains("\"records\":[{},{}]"));
        assert!(page.contains("\"cursor\":0"));
    }

    #[test]
    fn trace_wrap_and_fourth_match_are_not_skipped() {
        use shared::usb::types::{UiEvent, UsbError};
        let bus = Bus::new();
        bus.0.borrow_mut().cursor = u32::MAX - 2;
        for _ in 0..4 {
            crate::trace_query::tests::send(
                &bus,
                UiEvent::Host {
                    event: UsbEvent::Error {
                        code: UsbError::Stall,
                    },
                },
            );
        }
        let query = crate::trace_query::TraceQuery {
            after: u32::MAX - 2,
            ..Default::default()
        };
        let page = bus.trace(&query);
        assert!(page.contains("\"sequence\":4294967294"));
        assert!(page.contains("\"sequence\":0"));
        assert!(page.contains("\"cursor\":0"));
        assert!(!page.contains("\"sequence\":1,"));
        let next = bus.trace(&crate::trace_query::TraceQuery::default());
        assert!(next.contains("\"sequence\":1,"));
        assert!(next.contains("\"dropped\":0"));
    }

    #[test]
    fn deferred_trace_tail_wrap_and_future_restart() {
        use crate::trace_query::{
            TraceQuery,
            tests::{context, send, transfer},
        };
        use shared::usb::types::UiEvent;
        for before in [0, u32::MAX - 1, u32::MAX] {
            for future in [false, true] {
                let bus = Bus::new();
                bus.0.borrow_mut().cursor = before;
                let q = TraceQuery {
                    after: if future {
                        before.wrapping_add(100)
                    } else {
                        before
                    },
                    q: Some(String::try_from("get descriptor").unwrap()),
                    ..Default::default()
                };
                send(&bus, UiEvent::Host { event: context(0) });
                let waiting = bus.trace(&q);
                let mut expected = String::<128>::new();
                write!(
                    expected,
                    "{{\"records\":[],\"cursor\":{before},\"dropped\":0}}"
                )
                .unwrap();
                assert_eq!(waiting, expected);
                let resumed = TraceQuery { after: before, ..q };
                assert_eq!(bus.trace(&resumed), waiting);
                send(&bus, UiEvent::Host { event: transfer(0) });
                let page = bus.trace(&resumed);
                for id in [before.wrapping_add(1), before.wrapping_add(2)] {
                    let mut sequence = String::<32>::new();
                    write!(sequence, "\"sequence\":{id},").unwrap();
                    assert!(page.contains(sequence.as_str()), "{page}");
                }
                let mut cursor = String::<32>::new();
                write!(cursor, "\"cursor\":{}", before.wrapping_add(2)).unwrap();
                assert!(page.contains(cursor.as_str()));
                assert!(page.contains("\"dropped\":0"));
            }
        }
    }

    #[test]
    fn trace_deferral_does_not_pin_invalid_or_non_tail_records() {
        use crate::trace_query::{
            TraceQuery,
            tests::{context, send},
        };
        use shared::usb::types::UiEvent;
        let bus = Bus::new();
        let q = TraceQuery {
            q: Some(String::try_from("get descriptor").unwrap()),
            ..Default::default()
        };
        assert_eq!(bus.trace(&q), "{\"records\":[],\"cursor\":0,\"dropped\":0}");
        let future = TraceQuery {
            after: 100,
            ..TraceQuery::default()
        };
        assert_eq!(
            bus.trace(&future),
            "{\"records\":[],\"cursor\":0,\"dropped\":0}"
        );
        send(&bus, UiEvent::Host { event: context(0) });
        bus.receive(&Message::with_payload(Event::Ui, b"\x03{\"transfer_context\":").unwrap());
        assert_eq!(bus.trace(&q), "{\"records\":[],\"cursor\":2,\"dropped\":0}");
        // Excluded records before a deferred tail still advance the cursor.
        send(&bus, UiEvent::Host { event: context(0) });
        assert_eq!(bus.trace(&q), "{\"records\":[],\"cursor\":2,\"dropped\":0}");
    }

    #[test]
    fn chunks_pages_and_overrun() {
        let bus = Bus::new();
        bus.receive(&Message::with_payload(Event::Ui, b"\x01{\"timestamp_ms\":0,").unwrap());
        assert!(bus.events(0).contains("\"records\":[]"));
        bus.receive(
            &Message::with_payload(
                Event::Ui,
                b"\x02\"event\":{\"type\":\"hello\",\"protocol_version\":5}}",
            )
            .unwrap(),
        );
        assert!(bus.events(0).contains("\"cursor\":1"));
        for _ in 0..67 {
            bus.receive(&Message::with_payload(Event::Ui, b"\x03{}").unwrap());
        }
        let page = bus.events(0);
        assert!(page.contains("\"dropped\":4"));
        assert!(page.contains("\"cursor\":7"));
        assert!(bus.events(68).contains("\"records\":[]"));
        assert!(bus.events(99).contains("\"cursor\":7"));
    }
}
