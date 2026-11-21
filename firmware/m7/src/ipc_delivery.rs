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
}
const _: () = assert!(core::mem::size_of::<Delivery>() <= 20 * 1024);

impl Default for Delivery {
    fn default() -> Self {
        Self::new()
    }
}
impl Delivery {
    pub const fn new() -> Self {
        Self {
            captures: Queue::new(),
            incoming: Queue::new(),
            failed: Cell::new(0),
            transport_errors: Cell::new(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{protocol::Event, usb::types::HostCommand};
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
