//! Watchdog admission and finite verification block schedule. Making progress
//! never confirms a trial or substitutes for current service/IPC health.
use core::ops::Range;

pub const VERIFY_BLOCK: usize = 256;
pub const READ_QUANTUM: usize = 16;

pub fn can_feed(
    confirmed: bool,
    authenticated: bool,
    services_healthy: bool,
    ipc_fresh: bool,
) -> bool {
    confirmed && authenticated && services_healthy && ipc_fresh
}

/// Each completed block permits one health recheck, not an unconditional feed.
pub fn blocks(range: Range<usize>) -> impl Iterator<Item = Range<usize>> {
    let end = range.end;
    range
        .step_by(VERIFY_BLOCK)
        .map(move |start| start..start.saturating_add(VERIFY_BLOCK).min(end))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_bus::Bus;
    use shared::protocol::{Event, Message};

    #[test]
    fn progress_does_not_replace_confirmation_or_any_health_input() {
        for bits in 0..16 {
            assert_eq!(
                can_feed(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0, bits & 8 != 0),
                bits == 15
            );
        }
        let bus = Bus::new();
        bus.expect_pong(42);
        bus.pong(
            &Message::with_payload(Event::Pong, &42u32.to_le_bytes()).unwrap(),
            100,
        );
        assert!(can_feed(true, true, true, bus.ipc_healthy(3099)));
        assert!(!can_feed(true, true, true, bus.ipc_healthy(3100)));
    }

    #[test]
    fn ready_and_commit_verification_have_finite_bounded_progress_pulses() {
        let bus = Bus::new();
        let mut completed = 0;
        let mut feeds = 0;
        let mut now = 0u64;
        for _phase in 0..2 {
            // Ready readback and explicit commit readback.
            for range in [
                0..crate::ota_manifest::ACTIVE_LENGTH as usize,
                crate::dashboard::HEADER_SIZE..crate::ota_manifest::DASHBOARD_LENGTH as usize,
            ] {
                let mut previous = range.start;
                let end = range.end;
                for block in blocks(range) {
                    assert_eq!(block.start, previous);
                    assert!((1..=VERIFY_BLOCK).contains(&block.len()));
                    previous = block.end;
                    now += 500; // Model contention long enough to exceed IWDG many times.
                    if now.is_multiple_of(1000) {
                        bus.expect_pong(now as u32);
                        bus.pong(
                            &Message::with_payload(Event::Pong, &(now as u32).to_le_bytes())
                                .unwrap(),
                            now,
                        );
                    }
                    completed += 1;
                    if can_feed(true, true, true, bus.ipc_healthy(now)) {
                        feeds += 1;
                    }
                    assert!(!can_feed(false, true, true, bus.ipc_healthy(now)));
                }
                assert_eq!(previous, end);
            }
        }
        assert_eq!(completed, 2 * (7680 + 4096));
        assert_eq!(feeds, completed - 1); // No Pong has arrived at the first block.
        assert!(now > 32_000);
        assert_eq!(blocks(0..0).count(), 0); // No progress means no callback/feed.
    }
}
