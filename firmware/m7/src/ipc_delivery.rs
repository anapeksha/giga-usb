//! M7-local delivery state. 64 capture frames plus one in flight; four incoming
//! commands. The endpoint task retries the exact in-flight frame before dequeueing
//! another. Command traffic gets a turn between captures, never inside a frame.
//! Storage is private M7 RAM, compile-time bounded below 20 KiB (including
//! channel bookkeeping). Overflow evicts the oldest raw capture for semantics;
//! otherwise the new record is rejected and counted only when abandoned.
use core::cell::Cell;
use shared::ipc_delivery::Queue;
use shared::protocol::Message;

/// A packet-boundary wait never extends the caller's absolute deadline.
pub fn drain_deadline(now_us: u64, packet_deadline_us: u64) -> u64 {
    now_us.saturating_add(2_000).min(packet_deadline_us)
}
pub fn should_drain(queued: usize, now_us: u64, until_us: u64) -> bool {
    queued > 60 && now_us < until_us
}

pub struct Delivery {
    pub captures: Queue<Message, 64>,
    pub incoming: Queue<Message, 4>,
    pub failed: Cell<u32>,
    pub transport_errors: Cell<u32>,
    pub snapshot_requested: Cell<bool>,
    capture_turn: Cell<bool>,
    reported_errors: Cell<u32>,
    report_at_ms: Cell<u64>,
}
const _: () = assert!(core::mem::size_of::<Delivery>() <= 20 * 1024);

impl Default for Delivery {
    fn default() -> Self {
        Self::new()
    }
}
impl Delivery {
    pub fn note_transport_error(&self) {
        self.transport_errors
            .set(self.transport_errors.get().saturating_add(1));
    }

    pub fn request_snapshot(&self, local: bool) {
        if local {
            self.snapshot_requested.set(true);
        }
    }

    /// Only consume the coalesced request once the frame owns the in-flight slot.
    pub fn fill_pending(&self, pending: &mut shared::ipc_delivery::Pending) {
        if !pending.is_none() {
            return;
        }
        if self.capture_turn.get()
            && let Some(message) = self.captures.pop_front()
        {
            pending.set(Some(message));
            self.capture_turn.set(false);
            return;
        }
        if self.snapshot_requested.get() {
            match Message::encode(
                shared::protocol::Event::Command,
                &shared::usb::types::HostCommand::Status,
            ) {
                Ok(message) => {
                    pending.set(Some(message));
                    self.snapshot_requested.set(false);
                    self.capture_turn.set(true);
                    return;
                }
                Err(_) => self.note_transport_error(),
            }
        }
        if let Some(message) = self.captures.pop_front() {
            pending.set(Some(message));
            self.capture_turn.set(false);
        }
    }

    pub fn report_transport_errors(&self, bus: &crate::network_bus::Bus, now_ms: u64) {
        let total = self.transport_errors.get();
        if total != self.reported_errors.get() && now_ms >= self.report_at_ms.get() {
            if bus.publish_ipc_error(total, now_ms) {
                self.reported_errors.set(total);
            }
            self.report_at_ms.set(now_ms.saturating_add(1_000));
        }
    }

    pub const fn new() -> Self {
        Self {
            captures: Queue::new(),
            incoming: Queue::new(),
            failed: Cell::new(0),
            transport_errors: Cell::new(0),
            snapshot_requested: Cell::new(false),
            capture_turn: Cell::new(false),
            reported_errors: Cell::new(0),
            report_at_ms: Cell::new(0),
        }
    }
}

// One owned IPC frame avoids heap allocation or references into queue storage.
#[allow(clippy::large_enum_variant)]
pub enum Work {
    LocalCommand(shared::usb::types::HostCommand),
    Incoming(Message),
}

/// Alternate nonempty sources; an empty source lends its turn to the other.
#[derive(Default)]
pub struct CommandTurn {
    ipc_first: bool,
}
impl CommandTurn {
    pub fn next(
        &mut self,
        bus: Option<&crate::network_bus::Bus>,
        delivery: &Delivery,
    ) -> Option<Work> {
        let local = || {
            bus.and_then(crate::network_bus::Bus::next_command)
                .map(Work::LocalCommand)
        };
        let incoming = || delivery.incoming.pop_front().map(Work::Incoming);
        let work = if self.ipc_first {
            incoming().or_else(local)
        } else {
            local().or_else(incoming)
        };
        if let Some(ref work) = work {
            self.ipc_first = matches!(work, Work::LocalCommand(_));
        }
        work
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{protocol::Event, usb::types::HostCommand};
    #[test]
    fn dispatch_is_bounded_fair_and_fifo() {
        let bus = crate::network_bus::Bus::new();
        let delivery = Delivery::new();
        let commands = [
            HostCommand::Start,
            HostCommand::Stop,
            HostCommand::Reset,
            HostCommand::Status,
        ];
        for command in commands {
            assert!(bus.command(command));
            delivery
                .incoming
                .push_back(Message::encode(Event::Command, &command).unwrap())
                .unwrap();
        }
        let mut turn = CommandTurn::default();
        for expected in commands {
            match turn.next(Some(&bus), &delivery).unwrap() {
                Work::LocalCommand(command) => assert_eq!(command, expected),
                _ => panic!("expected local turn"),
            }
            match turn.next(Some(&bus), &delivery).unwrap() {
                Work::Incoming(frame) => {
                    assert_eq!(frame.decode::<HostCommand>().unwrap(), expected)
                }
                _ => panic!("expected IPC turn"),
            }
        }
        assert!(turn.next(Some(&bus), &delivery).is_none());
        assert!(bus.command(HostCommand::Stop));
        assert!(matches!(
            turn.next(Some(&bus), &delivery),
            Some(Work::LocalCommand(HostCommand::Stop))
        ));
    }

    #[test]
    fn snapshot_origin_full_capture_and_inflight_retention() {
        let delivery = Delivery::new();
        let mut pending = shared::ipc_delivery::Pending::new();
        delivery.request_snapshot(false);
        assert!(!delivery.snapshot_requested.get());
        for _ in 0..64 {
            delivery.captures.push_back(Message::ping()).unwrap();
        }
        pending.set(Some(Message::ping()));
        delivery.request_snapshot(true);
        delivery.request_snapshot(true);
        delivery.fill_pending(&mut pending);
        assert!(delivery.snapshot_requested.get());
        pending.retry(|_| Ok(())).unwrap();
        delivery.fill_pending(&mut pending);
        assert!(!delivery.snapshot_requested.get());
        assert_eq!(delivery.captures.len(), 64);
        pending
            .retry(|frame| {
                assert_eq!(frame.event, Event::Command);
                assert_eq!(frame.decode::<HostCommand>().unwrap(), HostCommand::Status);
                Err(shared::ipc::SendError::Full)
            })
            .unwrap();
        assert!(!pending.is_none());
        assert_eq!(delivery.failed.get(), 0);
    }

    #[test]
    fn repeated_snapshots_alternate_with_fifo_captures_and_lend_empty_turns() {
        let delivery = Delivery::new();
        let mut pending = shared::ipc_delivery::Pending::new();
        for value in 0..4u32 {
            delivery
                .captures
                .push_back(Message::encode(Event::Usb, &value).unwrap())
                .unwrap();
        }
        for value in 0..4u32 {
            delivery.request_snapshot(true);
            delivery.fill_pending(&mut pending);
            pending
                .retry(|frame| {
                    assert_eq!(frame.event, Event::Command);
                    assert_eq!(frame.decode::<HostCommand>().unwrap(), HostCommand::Status);
                    Ok(())
                })
                .unwrap();
            delivery.request_snapshot(true);
            delivery.fill_pending(&mut pending);
            pending
                .retry(|frame| {
                    assert_eq!(frame.event, Event::Usb);
                    assert_eq!(frame.decode::<u32>().unwrap(), value);
                    Err(shared::ipc::SendError::Full)
                })
                .unwrap();
            delivery.fill_pending(&mut pending);
            pending
                .retry(|frame| {
                    assert_eq!(frame.event, Event::Usb);
                    assert_eq!(frame.decode::<u32>().unwrap(), value);
                    Ok(())
                })
                .unwrap();
        }
        assert!(delivery.captures.is_empty());
        // The empty capture source lends consecutive turns to snapshots.
        for _ in 0..2 {
            delivery.request_snapshot(true);
            delivery.fill_pending(&mut pending);
            assert_eq!(pending.event(), Some(Event::Command));
            pending.retry(|_| Ok(())).unwrap();
        }
        // The empty snapshot source likewise lends consecutive turns to captures.
        for _ in 0..2 {
            delivery.captures.push_back(Message::ping()).unwrap();
            delivery.fill_pending(&mut pending);
            assert_eq!(pending.event(), Some(Event::Ping));
            pending.retry(|_| Ok(())).unwrap();
        }
        assert_eq!(delivery.failed.get(), 0);
        assert_eq!(delivery.transport_errors.get(), 0);
    }

    #[test]
    fn ipc_errors_are_generic_rate_limited_and_not_usb_trace() {
        let delivery = Delivery::new();
        let bus = crate::network_bus::Bus::new();
        delivery.note_transport_error();
        delivery.report_transport_errors(&bus, 0);
        assert!(bus.events(0).contains("IPC transport errors: 1"));
        assert!(
            !bus.trace(&crate::trace_query::TraceQuery::default())
                .contains("IPC transport")
        );
        delivery.note_transport_error();
        delivery.report_transport_errors(&bus, 25);
        assert!(!bus.events(0).contains("IPC transport errors: 2"));
        delivery.report_transport_errors(&bus, 1_000);
        assert!(bus.events(0).contains("IPC transport errors: 2"));
        assert_eq!(delivery.transport_errors.get(), 2);
        assert_eq!(delivery.failed.get(), 0);
        assert!(delivery.captures.is_empty());
    }

    #[test]
    fn stalled_peer_drain_budget_does_not_block_stop_or_extend_deadline() {
        let delivery = Delivery::new();
        for _ in 0..64 {
            delivery.captures.push_back(Message::ping()).unwrap();
        }
        let until = drain_deadline(100, 10_000);
        assert_eq!(until, 2_100);
        assert_eq!(drain_deadline(100, 350), 350);
        assert_eq!(drain_deadline(100, 50), 50);
        assert!(!should_drain(64, 2_100, until));
        assert!(!should_drain(60, 100, until));
        let mut enabled = true;
        for now in (100..=2_100).step_by(100) {
            if now == 400 {
                delivery
                    .incoming
                    .push_back(Message::encode(Event::Command, &HostCommand::Stop).unwrap())
                    .unwrap();
            }
            if let Some(frame) = delivery.incoming.pop_front() {
                assert_eq!(frame.decode::<HostCommand>().unwrap(), HostCommand::Stop);
                enabled = false;
            }
            if !enabled || !should_drain(delivery.captures.len(), now, until) {
                assert_eq!(now, 400);
                break;
            }
        }
        assert!(!enabled);
        assert_eq!(delivery.captures.len(), 64);
        assert_eq!(delivery.failed.get(), 0);
    }
}
