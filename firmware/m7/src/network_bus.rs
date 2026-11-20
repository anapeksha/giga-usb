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
        state.led.network = Some(match info.mode.as_str() {
            "setup" => NetworkMode::Setup,
            "connected" => NetworkMode::Connected,
            "error" => NetworkMode::Error,
            _ => NetworkMode::Connecting,
        });
        state.network = Some(info);
    }

    pub fn led_snapshot(&self) -> Input {
        self.0.borrow().led
    }

    pub fn set_saving(&self, saving: bool) {
        self.0.borrow_mut().led.saving = saving;
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
