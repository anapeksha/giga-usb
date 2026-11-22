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
pub struct Bus(RefCell<State>, [SubscriberSlot; 2]);

struct SubscriberSlot {
    active: core::cell::Cell<bool>,
    token: RefCell<Option<String<32>>>,
    proof: RefCell<Option<String<32>>>,
    changed: shared::ipc_delivery::Queue<(), 1>,
}

impl SubscriberSlot {
    const fn new() -> Self {
        Self {
            active: core::cell::Cell::new(false),
            token: RefCell::new(None),
            proof: RefCell::new(None),
            changed: shared::ipc_delivery::Queue::new(),
        }
    }
}

/// A coalescing, per-client Embassy channel notification. Dropping releases admission.
/// Like the bus, this is only for the cooperative M7 executor.
pub struct Subscriber<'a> {
    slot: &'a SubscriberSlot,
    pub resumed: bool,
    pub proof: Option<String<32>>,
}

impl Subscriber<'_> {
    pub async fn changed(&self) {
        self.slot.changed.receive().await;
    }
}

impl Drop for Subscriber<'_> {
    fn drop(&mut self) {
        self.slot.active.set(false);
        self.slot.changed.clear();
    }
}
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
        Self(
            RefCell::new(State {
                commands: Deque::new(),
                records: Deque::new(),
                assembly: Vec::new(),
                assembling: false,
                cursor: 0,
                dropped: 0,
                network: None,
                led: Input::new(),
            }),
            [SubscriberSlot::new(), SubscriberSlot::new()],
        )
    }

    pub fn subscribe(&self) -> Option<Subscriber<'_>> {
        self.subscribe_client(None, None, None)
    }

    /// Tokens only remember cursor continuity; they are not authentication.
    /// Resume requires acknowledging a reset proof, not merely admission or a write.
    pub fn subscribe_client(
        &self,
        token: Option<&str>,
        attempt: Option<&str>,
        resume: Option<&str>,
    ) -> Option<Subscriber<'_>> {
        // Validate every bounded input before changing remembered state.
        let token = token.map(String::<32>::try_from).transpose().ok()?;
        let attempt = attempt.map(String::<32>::try_from).transpose().ok()?;
        let resume = resume.map(String::<32>::try_from).transpose().ok()?;
        let remembered = token.as_deref().and_then(|token| {
            self.1
                .iter()
                .find(|slot| slot.token.borrow().as_deref() == Some(token))
        });

        let slot = if let Some(slot) = remembered {
            if slot.active.get() {
                return None;
            }
            slot
        } else {
            self.1.iter().find(|slot| !slot.active.get())?
        };
        let resumed = remembered.is_some()
            && slot
                .proof
                .borrow()
                .as_ref()
                .is_some_and(|proof| !proof.is_empty() && Some(proof) == resume.as_ref());
        if !resumed {
            *slot.token.borrow_mut() = token;
            *slot.proof.borrow_mut() = attempt;
        }
        let proof = slot.proof.borrow().clone();
        slot.changed.clear();
        slot.active.set(true);
        Some(Subscriber {
            slot,
            resumed,
            proof,
        })
    }

    fn notify(&self) {
        for slot in &self.1 {
            if slot.active.get() {
                let _ = slot.changed.push_back(());
            }
        }
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
        let published = state.records.push_back((cursor, bytes)).is_ok();
        drop(state);
        if published {
            self.notify();
        }
        published
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
            let previous = s.dropped;
            s.dropped = s.dropped.saturating_add(1);
            let changed = previous != s.dropped;
            drop(s);
            if changed {
                self.notify();
            }
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
            drop(s);
            self.notify();
        }
    }
    /// Filter retained history without changing event delivery or counting exclusions as loss.
    pub fn trace(&self, query: &crate::trace_query::TraceQuery) -> String<4096> {
        let mut out = String::new();
        self.trace_into(query, &mut out);
        out
    }

    pub fn trace_into(&self, query: &crate::trace_query::TraceQuery, out: &mut String<4096>) {
        out.clear();
        let s = self.0.borrow();
        let age = s.cursor.wrapping_sub(query.after);
        let restart = age > i32::MAX as u32;
        let missed = if restart {
            0
        } else {
            age.saturating_sub(s.records.len() as u32)
        };
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
    }

    /// At most three records per page; callers continue from the returned cursor.
    /// JSON `dropped` combines history missed by this query with cumulative record
    /// assembly overflows. It is not a per-page delta or a host/M4 capture-drop counter;
    /// do not repeatedly sum it across polls.
    pub fn events(&self, after: u32) -> String<4096> {
        let mut out = String::new();
        self.events_into(after, &mut out);
        out
    }

    pub fn events_into(&self, after: u32, out: &mut String<4096>) {
        out.clear();
        let s = self.0.borrow();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::protocol::Event;
    extern crate std;
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Wake,
    };

    #[derive(Default)]
    struct Wakes(AtomicUsize);
    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn every_pending_subscriber_wakes_and_a_released_slot_can_wait_again() {
        let bus = Bus::new();
        let first = bus.subscribe().unwrap();
        let second = bus.subscribe().unwrap();
        let wakes = [Arc::new(Wakes::default()), Arc::new(Wakes::default())];
        let wakers = [Waker::from(wakes[0].clone()), Waker::from(wakes[1].clone())];
        {
            let mut waits = [pin!(first.changed()), pin!(second.changed())];
            for (wait, waker) in waits.iter_mut().zip(&wakers) {
                assert!(
                    wait.as_mut()
                        .poll(&mut Context::from_waker(waker))
                        .is_pending()
                );
            }
            assert!(bus.publish_ipc_error(1, 0));
            for ((wait, waker), count) in waits.iter_mut().zip(&wakers).zip(&wakes) {
                assert!(count.0.load(Ordering::Relaxed) > 0);
                assert_eq!(
                    wait.as_mut().poll(&mut Context::from_waker(waker)),
                    Poll::Ready(())
                );
            }
        }
        // Cancel an outstanding receive exactly as disconnect cancels EventSource.
        {
            let mut wait = pin!(first.changed());
            assert!(
                wait.as_mut()
                    .poll(&mut Context::from_waker(&wakers[0]))
                    .is_pending()
            );
        }
        drop(first);
        assert!(bus.publish_ipc_error(2, 0));
        let replacement = bus.subscribe().unwrap();
        let mut wait = pin!(replacement.changed());
        let mut cx = Context::from_waker(&wakers[0]);
        assert!(wait.as_mut().poll(&mut cx).is_pending());
        assert!(bus.publish_ipc_error(3, 0));
        assert_eq!(wait.as_mut().poll(&mut cx), Poll::Ready(()));
    }

    #[test]
    fn partial_chunks_do_not_notify_but_completion_and_loss_metadata_do() {
        let bus = Bus::new();
        let subscriber = bus.subscribe().unwrap();
        bus.receive(&Message::with_payload(Event::Ui, b"\x01{").unwrap());
        assert!(subscriber.slot.changed.is_empty());
        bus.receive(&Message::with_payload(Event::Ui, b"\x02}").unwrap());
        assert_eq!(subscriber.slot.changed.pop_front(), Some(()));
        assert_eq!(crate::event_stream::position(&bus.events(0)).cursor, 1);

        let mut stream = crate::event_stream::StreamPages::new(crate::event_stream::StreamQuery {
            after: 1,
            trace_after: 1,
            trace: true,
            ..Default::default()
        });
        let initial = stream.next(&bus);
        assert!(initial.events.is_some());
        assert!(initial.trace.is_some());
        assert!(!stream.next(&bus).pending);
        let chunk = [b'x'; 200];
        let mut data = Vec::<u8, 256>::new();
        data.push(1).unwrap();
        data.extend_from_slice(&chunk).unwrap();
        bus.receive(&Message::with_payload(Event::Ui, &data).unwrap());
        data[0] = 0;
        for _ in 0..4 {
            bus.receive(&Message::with_payload(Event::Ui, &data).unwrap());
        }
        assert!(subscriber.slot.changed.is_empty());
        bus.receive(&Message::with_payload(Event::Ui, &data).unwrap());
        assert_eq!(subscriber.slot.changed.pop_front(), Some(()));
        let page = crate::event_stream::position(&bus.events(1));
        assert_eq!(page.cursor, 1);
        assert_eq!(page.dropped, 1);
        let metadata = stream.next(&bus);
        assert!(!metadata.pending);
        assert_eq!(
            crate::event_stream::position(&metadata.events.unwrap()).dropped,
            1
        );
        assert_eq!(
            crate::event_stream::position(&metadata.trace.unwrap()).dropped,
            1
        );
        let idle = stream.next(&bus);
        assert!(idle.events.is_none());
        assert!(idle.trace.is_none());
        bus.receive(&Message::with_payload(Event::Ui, &data).unwrap());
        assert!(subscriber.slot.changed.is_empty());
    }

    #[test]
    fn dropped_reset_is_not_acknowledged_and_each_stale_retry_rotates_the_proof() {
        let bus = Bus::new();
        let first = bus
            .subscribe_client(Some("client"), Some("attempt-1"), Some("old-boot"))
            .unwrap();
        assert!(!first.resumed);
        assert_eq!(first.proof.as_deref(), Some("attempt-1"));
        let slot = first.slot;
        drop(first); // Reset never reached the application.
        for attempt in ["attempt-2", "attempt-3"] {
            let retry = bus
                .subscribe_client(Some("client"), Some(attempt), Some("old-boot"))
                .unwrap();
            assert!(!retry.resumed);
            assert!(core::ptr::eq(retry.slot, slot));
            assert_eq!(retry.proof.as_deref(), Some(attempt));
        }
        let acknowledged = bus
            .subscribe_client(Some("client"), Some("attempt-4"), Some("attempt-3"))
            .unwrap();
        assert!(acknowledged.resumed);
        assert_eq!(acknowledged.proof.as_deref(), Some("attempt-3"));
        drop(acknowledged);
        // Successful resumes retain the acknowledged proof rather than rotating it.
        assert!(
            bus.subscribe_client(Some("client"), Some("attempt-5"), Some("attempt-3"))
                .unwrap()
                .resumed
        );
        let rebooted = Bus::new();
        let retry = rebooted
            .subscribe_client(Some("client"), Some("new-boot-attempt"), Some("attempt-3"))
            .unwrap();
        assert!(!retry.resumed);
        assert_eq!(retry.proof.as_deref(), Some("new-boot-attempt"));
    }

    #[test]
    fn missing_or_empty_proof_never_resumes_and_invalid_inputs_leave_proof_intact() {
        let bus = Bus::new();
        for attempt in [None, Some("")] {
            drop(bus.subscribe_client(Some("client"), attempt, None).unwrap());
            let retry = bus
                .subscribe_client(Some("client"), attempt, attempt)
                .unwrap();
            assert!(!retry.resumed);
        }
        drop(
            bus.subscribe_client(Some("client"), Some("acknowledged"), None)
                .unwrap(),
        );
        let too_long = "0123456789abcdef0123456789abcdef0";
        for (client, attempt, resume) in [
            (Some(too_long), Some("replacement"), Some("acknowledged")),
            (Some("client"), Some(too_long), Some("acknowledged")),
            (Some("client"), Some("replacement"), Some(too_long)),
        ] {
            assert!(bus.subscribe_client(client, attempt, resume).is_none());
        }
        assert!(
            bus.subscribe_client(Some("client"), Some("fresh"), Some("acknowledged"))
                .unwrap()
                .resumed
        );
    }

    #[test]
    fn remembered_client_resumes_only_after_the_active_stream_exits() {
        let bus = Bus::new();
        let first = bus
            .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
            .unwrap();
        assert!(!first.resumed);
        assert!(
            bus.subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
                .is_none()
        );
        // A duplicate must not occupy the other free slot.
        let other = bus
            .subscribe_client(Some("client-b"), Some("attempt-b"), Some("attempt-b"))
            .unwrap();
        assert!(!other.resumed);
        assert!(bus.publish_ipc_error(1, 0));
        drop(first);
        let resumed = bus
            .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
            .unwrap();
        assert!(resumed.resumed);
        assert!(resumed.slot.changed.is_empty());
        assert!(core::ptr::eq(resumed.slot, &bus.1[0]));
        assert!(
            bus.subscribe_client(Some("client-c"), Some("attempt-c"), Some("attempt-c"))
                .is_none()
        );
    }

    #[test]
    fn eviction_replaces_only_inactive_tokens_and_anonymous_clients_never_resume() {
        let bus = Bus::new();
        let first = bus
            .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
            .unwrap();
        let other = bus
            .subscribe_client(Some("client-b"), Some("attempt-b"), Some("attempt-b"))
            .unwrap();
        drop(first);
        let replacement = bus
            .subscribe_client(Some("client-c"), Some("attempt-c"), Some("attempt-c"))
            .unwrap();
        assert!(!replacement.resumed);
        assert!(
            bus.subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
                .is_none()
        );
        assert_eq!(other.slot.token.borrow().as_deref(), Some("client-b"));
        drop(replacement);
        let forgotten = bus
            .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
            .unwrap();
        assert!(!forgotten.resumed);
        drop(forgotten);
        let anonymous = bus.subscribe().unwrap();
        assert!(!anonymous.resumed);
        assert!(anonymous.slot.token.borrow().is_none());
        drop(anonymous);
        let anonymous = bus.subscribe().unwrap();
        assert!(!anonymous.resumed);
    }

    #[test]
    fn cancelling_a_pending_client_wait_releases_admission_but_keeps_the_token() {
        let bus = Bus::new();
        let client = bus
            .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
            .unwrap();
        {
            let mut pending = pin!(client.changed());
            assert!(
                pending
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        }
        drop(client);
        let resumed = bus
            .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
            .unwrap();
        assert!(resumed.resumed);
        let mut pending = pin!(resumed.changed());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(pending.as_mut().poll(&mut cx).is_pending());
        assert!(bus.publish_ipc_error(1, 0));
        assert_eq!(pending.as_mut().poll(&mut cx), Poll::Ready(()));
    }

    #[test]
    fn subscribers_are_bounded_broadcast_coalesced_and_released() {
        let bus = Bus::new();
        let first = bus.subscribe().unwrap();
        let second = bus.subscribe().unwrap();
        assert!(bus.subscribe().is_none());
        assert!(bus.command(HostCommand::Status));
        assert!(first.slot.changed.is_empty());
        assert!(bus.publish_ipc_error(1, 0));
        assert!(bus.publish_ipc_error(2, 0));
        assert_eq!(first.slot.changed.len(), 1);
        assert_eq!(second.slot.changed.len(), 1);
        drop(first);
        let replacement = bus.subscribe().unwrap();
        assert!(replacement.slot.changed.is_empty());
        assert!(bus.subscribe().is_none());
        drop(second);
        drop(replacement);
        assert!(bus.subscribe().is_some());
    }

    #[test]
    fn slow_subscriber_never_blocks_history_and_resynchronizes() {
        let bus = Bus::new();
        let subscriber = bus.subscribe().unwrap();
        for i in 0..100 {
            assert!(bus.publish_ipc_error(i, 0));
        }
        assert_eq!(subscriber.slot.changed.len(), 1);
        let page = crate::event_stream::position(&bus.events(0));
        assert_eq!(page.cursor, 39);
        assert_eq!(page.dropped, 36);
        assert!(bus.events(0).len() <= 4096);
    }
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
