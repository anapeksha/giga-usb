//! M7-local proxy authority. No HID parsing or device-controller access here.
use core::cell::RefCell;
use heapless::Deque;
use shared::usb::proxy::*;

pub struct Proxy(RefCell<State>);
struct State {
    status: ProxyStatus,
    input_was_authorized: bool,
    reset: Option<HostToProxy>,
    queue: Deque<HostToProxy, FORWARD_QUEUE_CAPACITY>,
    outstanding: Deque<(u32, u64), FORWARD_QUEUE_CAPACITY>,
    sequence: u32,
    heartbeat_at: u64,
    ready_at: u64,
    leds: Option<u8>,
    changed: bool,
    snapshot: Option<ProxyStatus>,
    support_notice: Option<HostToProxy>,
}
impl Default for Proxy {
    fn default() -> Self {
        Self::new()
    }
}
impl Proxy {
    pub const fn new() -> Self {
        Self(RefCell::new(State {
            status: ProxyStatus {
                enabled: false,
                output_connected: false,
                ready: false,
                release: ProxyRelease::Pending,
                session: 0,
                generation: 0,
                keyboard: ProxySupport::Absent,
                mouse: ProxySupport::Absent,
                fault: ProxyFault::None,
                faults: 0,
                rejected_reports: 0,
                led_failures: 0,
            },
            input_was_authorized: false,
            reset: None,
            queue: Deque::new(),
            outstanding: Deque::new(),
            sequence: 0,
            heartbeat_at: 0,
            ready_at: 0,
            leds: None,
            changed: true,
            snapshot: None,
            support_notice: None,
        }))
    }
    pub fn input_was_authorized(&self) -> bool {
        self.0.borrow().input_was_authorized
    }
    pub fn status(&self) -> ProxyStatus {
        self.0.borrow().status
    }
    pub fn snapshot(&self) {
        let mut s = self.0.borrow_mut();
        s.snapshot = Some(s.status);
    }
    pub fn take_changed(&self) -> Option<ProxyStatus> {
        let mut s = self.0.borrow_mut();
        if !s.changed {
            return None;
        }
        s.changed = false;
        Some(s.status)
    }
    pub fn disable(&self) {
        self.0.borrow_mut().reset(false);
    }
    pub fn session(&self, session: u32) {
        let mut s = self.0.borrow_mut();
        s.status.session = session;
        s.status.keyboard = ProxySupport::Absent;
        s.status.mouse = ProxySupport::Absent;
        s.reset(false);
    }
    pub fn support(&self, keyboard: ProxySupport, mouse: ProxySupport) {
        let mut s = self.0.borrow_mut();
        s.status.keyboard = keyboard;
        s.status.mouse = mouse;
        s.changed = true;
    }
    pub fn support_notice(&self, unsupported_interfaces: u8) {
        let mut s = self.0.borrow_mut();
        s.support_notice = Some(HostToProxy::Support {
            session: s.status.session,
            generation: s.status.generation,
            keyboard: s.status.keyboard,
            mouse: s.status.mouse,
            unsupported_interfaces,
        });
    }
    pub fn enable(&self, generation: u32, now: u64) {
        let mut s = self.0.borrow_mut();
        // A delayed command must not become fresh intent merely because its
        // transport or local queue survived a reset, disable or session change.
        if generation != s.status.generation {
            return;
        }
        if !s.status.output_connected {
            s.fault(ProxyFault::Output);
            return;
        }
        if !matches!(s.status.keyboard, ProxySupport::Boot | ProxySupport::Absent)
            || !matches!(s.status.mouse, ProxySupport::Boot | ProxySupport::Absent)
            || (s.status.keyboard != ProxySupport::Boot && s.status.mouse != ProxySupport::Boot)
        {
            s.fault(ProxyFault::Unsupported);
            return;
        }
        s.status.fault = ProxyFault::None;
        s.reset(true);
        s.ready_at = now;
    }
    pub fn fault(&self, fault: ProxyFault) {
        self.0.borrow_mut().fault(fault);
    }
    pub fn tick(&self, now: u64) {
        let mut s = self.0.borrow_mut();
        if !s.status.enabled {
            return;
        }
        if (!s.status.ready && now.saturating_sub(s.ready_at) >= HEARTBEAT_TIMEOUT_MS)
            || s.outstanding
                .front()
                .is_some_and(|(_, at)| now.saturating_sub(*at) >= HEARTBEAT_TIMEOUT_MS)
        {
            s.fault(ProxyFault::Backpressure);
            return;
        }
        if now >= s.heartbeat_at {
            s.heartbeat_at = now.saturating_add(HEARTBEAT_INTERVAL_MS);
            let frame = HostToProxy::Heartbeat {
                session: s.status.session,
                generation: s.status.generation,
            };
            if s.queue.push_back(frame).is_err() {
                s.fault(ProxyFault::Backpressure);
            }
        }
    }
    pub fn report(&self, kind: BootKind, data: &[u8], now: u64) {
        let mut s = self.0.borrow_mut();
        if !s.status.enabled || !s.status.ready {
            return;
        }
        if s.status.release == ProxyRelease::Released {
            s.status.release = ProxyRelease::Pending;
            s.changed = true;
        }
        let length = data.len();
        // M4 validates contents. Never truncate an oversized physical report.
        if length > 8 {
            s.status.rejected_reports = s.status.rejected_reports.saturating_add(1);
            s.fault(ProxyFault::InvalidReport);
            return;
        }
        let mut bytes = [0; 8];
        bytes[..length].copy_from_slice(data);
        let sequence = s.sequence;
        let frame = HostToProxy::Report {
            session: s.status.session,
            generation: s.status.generation,
            sequence,
            kind,
            length: length as u8,
            bytes,
        };
        if s.queue.is_full() || s.outstanding.is_full() {
            s.fault(ProxyFault::Backpressure);
            return;
        }
        s.sequence = s.sequence.wrapping_add(1);
        let _ = s.queue.push_back(frame);
        let _ = s.outstanding.push_back((sequence, now));
    }
    pub fn next_frame(&self) -> Option<HostToProxy> {
        let mut s = self.0.borrow_mut();
        s.reset
            .take()
            .or_else(|| {
                s.snapshot
                    .take()
                    .map(|status| HostToProxy::Status { status })
            })
            .or_else(|| s.support_notice.take())
            .or_else(|| s.queue.pop_front())
    }
    pub fn feedback(&self, frame: ProxyToHost) {
        let mut s = self.0.borrow_mut();
        if let ProxyToHost::Output { connected } = frame {
            if connected && s.status.release == ProxyRelease::Failed {
                return;
            }
            s.status.output_connected = connected;
            s.reset(false);
            return;
        }
        if let ProxyToHost::Release {
            state: ProxyRelease::Failed,
            ..
        } = frame
        {
            // An output failure must remain observable even if M7 has already
            // disabled or advanced generations during cancellation/reconnect.
            if s.status.release != ProxyRelease::Failed || s.status.enabled {
                s.fault(ProxyFault::Output);
            }
            s.status.release = ProxyRelease::Failed;
            s.status.output_connected = false;
            s.changed = true;
            return;
        }
        // Transport faults can originate before M4 has adopted our generation.
        // They must invalidate authorization even if the reported epoch is stale.
        if matches!(
            frame,
            ProxyToHost::Fault {
                fault: ProxyFault::Transport,
                ..
            }
        ) {
            s.fault(ProxyFault::Transport);
            return;
        }
        let (session, generation) = match frame {
            ProxyToHost::Release {
                session,
                generation,
                ..
            } => (session, generation),
            ProxyToHost::Ready {
                session,
                generation,
            }
            | ProxyToHost::Consumed {
                session,
                generation,
                ..
            }
            | ProxyToHost::Leds {
                session,
                generation,
                ..
            }
            | ProxyToHost::Fault {
                session,
                generation,
                ..
            } => (session, generation),
            ProxyToHost::Output { .. } => unreachable!(),
        };
        if session != s.status.session || generation != s.status.generation {
            return;
        }
        match frame {
            ProxyToHost::Release { state, .. } if s.status.release != ProxyRelease::Failed => {
                s.status.release = state;
                s.changed = true;
            }
            ProxyToHost::Ready { .. } if s.status.enabled && s.status.output_connected => {
                s.status.ready = true;
                s.status.release = ProxyRelease::Released;
                s.changed = true;
            }
            ProxyToHost::Consumed { sequence, .. } if s.status.enabled => {
                if s.outstanding.front().map(|(seq, _)| *seq) != Some(sequence) {
                    s.fault(ProxyFault::Sequence);
                } else {
                    s.outstanding.pop_front();
                }
            }
            ProxyToHost::Leds { bits, .. } if s.status.enabled && s.status.ready => {
                if bits & !7 != 0 {
                    s.fault(ProxyFault::InvalidReport);
                } else {
                    s.leds = Some(bits);
                }
            }
            ProxyToHost::Fault { fault, .. } => {
                if fault == ProxyFault::InvalidReport {
                    s.status.rejected_reports = s.status.rejected_reports.saturating_add(1);
                }
                s.fault(fault);
            }
            _ => {}
        }
    }
    pub fn take_leds(&self) -> Option<(u32, u8)> {
        let mut s = self.0.borrow_mut();
        s.leds.take().map(|bits| (s.status.generation, bits))
    }
    pub fn led_failed(&self) {
        let mut s = self.0.borrow_mut();
        s.status.led_failures = s.status.led_failures.saturating_add(1);
        s.fault(ProxyFault::LedTransfer);
    }
}
impl State {
    fn reset(&mut self, enabled: bool) {
        self.input_was_authorized |= enabled;
        self.status.enabled = enabled;
        self.status.ready = false;
        if self.status.release != ProxyRelease::Failed {
            self.status.release = ProxyRelease::Pending;
        }
        self.status.generation = self.status.generation.wrapping_add(1);
        self.sequence = 0;
        self.leds = None;
        self.snapshot = None;
        self.support_notice = None;
        self.queue.clear();
        self.outstanding.clear();
        self.reset = Some(HostToProxy::Reset {
            session: self.status.session,
            generation: self.status.generation,
            enabled,
        });
        self.changed = true;
    }
    fn fault(&mut self, fault: ProxyFault) {
        self.status.fault = fault;
        self.status.faults = self.status.faults.saturating_add(1);
        self.reset(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> Proxy {
        let p = Proxy::new();
        p.feedback(ProxyToHost::Output { connected: true });
        p.session(1);
        p.support(ProxySupport::Boot, ProxySupport::Boot);
        p.enable(p.status().generation, 0);
        let s = p.status();
        p.feedback(ProxyToHost::Ready {
            session: s.session,
            generation: s.generation,
        });
        while p.next_frame().is_some() {}
        p
    }
    #[test]
    fn stale_enable_from_http_or_cdc_queue_cannot_execute_after_output_reset() {
        use shared::{
            protocol::{Event, Message},
            usb::types::HostCommand,
        };
        let p = ready();
        let old = p.status().generation;
        let bus = crate::network_bus::Bus::new();
        let delivery = crate::ipc_delivery::Delivery::new();
        assert!(bus.command(HostCommand::ProxyEnable { generation: old }));
        delivery
            .incoming
            .push_back(
                Message::encode(
                    Event::Command,
                    &HostCommand::ProxyEnable { generation: old },
                )
                .unwrap(),
            )
            .unwrap();
        p.feedback(ProxyToHost::Output { connected: false });
        p.feedback(ProxyToHost::Output { connected: true });
        let current = p.status();
        assert_ne!(old, current.generation);
        // Deliberately leave both old commands queued: execution must be safe
        // even without the eager source-queue cancellation optimization.
        let mut turn = crate::ipc_delivery::CommandTurn::default();
        for _ in 0..2 {
            let command = match turn.next(Some(&bus), &delivery).unwrap() {
                crate::ipc_delivery::Work::LocalCommand(command) => command,
                crate::ipc_delivery::Work::Incoming(message) => {
                    message.decode::<HostCommand>().unwrap()
                }
            };
            let HostCommand::ProxyEnable { generation } = command else {
                panic!("wrong command")
            };
            p.enable(generation, 1);
            assert_eq!(p.status(), current);
        }
        assert!(!p.status().enabled);
        p.enable(current.generation, 2);
        assert!(p.status().enabled);
        let fresh = p.status();
        p.enable(old, 3);
        assert_eq!(p.status(), fresh);
    }
    #[test]
    fn enable_generation_is_exact_across_u32_wrap() {
        let p = ready();
        p.0.borrow_mut().status.generation = u32::MAX;
        p.enable(u32::MAX, 0);
        let wrapped = p.status();
        assert!(wrapped.enabled);
        assert_eq!(wrapped.generation, 0);
        p.enable(u32::MAX, 1);
        assert_eq!(p.status(), wrapped);
        p.disable();
        let disabled = p.status();
        p.enable(0, 2);
        assert_eq!(p.status(), disabled);
    }
    #[test]
    fn off_is_not_release_confirmation_and_stale_failure_is_still_observable() {
        let p = ready();
        let old = p.status();
        p.disable();
        let disabled = p.status();
        assert_eq!(disabled.release, ProxyRelease::Pending);
        p.feedback(ProxyToHost::Release {
            session: old.session,
            generation: old.generation,
            state: ProxyRelease::Released,
        });
        assert_eq!(p.status().release, ProxyRelease::Pending);
        p.feedback(ProxyToHost::Release {
            session: old.session,
            generation: old.generation,
            state: ProxyRelease::Failed,
        });
        let failed = p.status();
        assert!(!failed.enabled);
        assert_eq!(failed.release, ProxyRelease::Failed);
        assert_eq!(failed.fault, ProxyFault::Output);
        assert!(failed.faults > disabled.faults);
        p.feedback(ProxyToHost::Release {
            session: old.session,
            generation: old.generation,
            state: ProxyRelease::Failed,
        });
        assert_eq!(p.status().generation, failed.generation);
        assert_eq!(p.status().faults, failed.faults);
        p.feedback(ProxyToHost::Release {
            session: failed.session,
            generation: failed.generation,
            state: ProxyRelease::Released,
        });
        assert_eq!(p.status().release, ProxyRelease::Failed);
        p.feedback(ProxyToHost::Output { connected: true });
        p.feedback(ProxyToHost::Ready {
            session: failed.session,
            generation: failed.generation,
        });
        p.enable(p.status().generation, 1);
        assert!(!p.status().enabled);
        assert!(!p.status().ready);
        assert!(!p.status().output_connected);
        assert_eq!(p.status().release, ProxyRelease::Failed);
    }
    #[test]
    fn transport_fault_before_peer_epoch_adoption_invalidates_enable() {
        let p = ready();
        p.feedback(ProxyToHost::Fault {
            session: 0,
            generation: 0,
            fault: ProxyFault::Transport,
        });
        assert!(!p.status().enabled);
        assert_eq!(p.status().fault, ProxyFault::Transport);
    }
    #[test]
    fn pre_ready_input_is_discarded_and_led_failure_is_observable() {
        let p = ready();
        p.disable();
        let stale = p.status();
        p.enable(p.status().generation, 1);
        let s = p.status();
        p.report(BootKind::Mouse, &[0, 10, 0], 1);
        p.feedback(ProxyToHost::Ready {
            session: stale.session,
            generation: stale.generation,
        });
        assert!(!p.status().ready);
        p.feedback(ProxyToHost::Ready {
            session: s.session,
            generation: s.generation,
        });
        p.report(BootKind::Keyboard, &[0; 8], 2);
        assert!(matches!(
            p.next_frame(),
            Some(HostToProxy::Reset { enabled: true, .. })
        ));
        assert!(matches!(
            p.next_frame(),
            Some(HostToProxy::Report {
                sequence: 0,
                kind: BootKind::Keyboard,
                ..
            })
        ));
        p.feedback(ProxyToHost::Leds {
            session: stale.session,
            generation: stale.generation,
            bits: 7,
        });
        assert_eq!(p.take_leds(), None);
        p.feedback(ProxyToHost::Leds {
            session: s.session,
            generation: s.generation,
            bits: 3,
        });
        assert_eq!(p.take_leds(), Some((s.generation, 3)));
        p.led_failed();
        assert_eq!(p.status().led_failures, 1);
        assert_eq!(p.status().fault, ProxyFault::LedTransfer);
        assert!(!p.status().enabled);
    }
    #[test]
    fn saturated_diagnostics_do_not_evict_reports_or_starve_capture() {
        use shared::{
            ipc::SendError,
            ipc_delivery::Pending,
            protocol::{Event, Message},
        };
        let delivery = crate::ipc_delivery::Delivery::new();
        let p = &delivery.proxy;
        p.feedback(ProxyToHost::Output { connected: true });
        p.session(1);
        p.support(ProxySupport::Boot, ProxySupport::Boot);
        p.enable(p.status().generation, 0);
        let s = p.status();
        p.feedback(ProxyToHost::Ready {
            session: s.session,
            generation: s.generation,
        });
        p.report(BootKind::Keyboard, &[0, 0, 4, 0, 0, 0, 0, 0], 1);
        p.report(BootKind::Keyboard, &[0; 8], 1);
        for _ in 0..64 {
            delivery.captures.push_back(Message::ping()).unwrap();
        }
        let mut pending = Pending::new();
        let mut reports = 0;
        let mut captures = 0;
        for _ in 0..6 {
            delivery.fill_pending(&mut pending);
            for _ in 0..3 {
                pending.retry(|_| Err(SendError::Full)).unwrap();
            }
            pending
                .retry(|message| {
                    if message.event == Event::Proxy {
                        if let HostToProxy::Report { sequence, .. } = message.decode().unwrap() {
                            assert_eq!(sequence, reports);
                            reports += 1;
                        }
                    } else {
                        captures += 1;
                    }
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(reports, 2);
        assert_eq!(captures, 3);
        assert_eq!(delivery.failed.get(), 0);
    }
    #[test]
    fn opt_in_and_output_reconnect_never_replays_input() {
        let p = ready();
        p.report(BootKind::Keyboard, &[0, 0, 4, 0, 0, 0, 0, 0], 0);
        p.feedback(ProxyToHost::Output { connected: false });
        p.feedback(ProxyToHost::Output { connected: true });
        assert!(!p.status().enabled);
        assert!(matches!(
            p.next_frame(),
            Some(HostToProxy::Reset { enabled: false, .. })
        ));
        assert_eq!(p.next_frame(), None);
        p.report(BootKind::Mouse, &[0, 10, 0], 1);
        assert_eq!(p.next_frame(), None);
    }
    #[test]
    fn pressure_and_missing_ack_fail_closed() {
        let p = ready();
        for _ in 0..FORWARD_QUEUE_CAPACITY {
            p.report(BootKind::Mouse, &[0, 1, 0], 0);
        }
        p.report(BootKind::Keyboard, &[0; 8], 0);
        assert_eq!(p.status().fault, ProxyFault::Backpressure);
        assert!(!p.status().enabled);
        assert!(matches!(
            p.next_frame(),
            Some(HostToProxy::Reset { enabled: false, .. })
        ));
        assert_eq!(p.next_frame(), None);
        let p = ready();
        p.report(BootKind::Keyboard, &[0; 8], 0);
        p.tick(HEARTBEAT_TIMEOUT_MS);
        assert!(!p.status().enabled);
    }
    #[test]
    fn quiet_hid_is_not_a_release_and_ack_is_ordered() {
        let p = ready();
        let s = p.status();
        p.report(BootKind::Keyboard, &[0, 0, 4, 0, 0, 0, 0, 0], 0);
        p.feedback(ProxyToHost::Consumed {
            session: s.session,
            generation: s.generation,
            sequence: 0,
        });
        for now in (100..10000).step_by(100) {
            p.tick(now);
            while p.next_frame().is_some() {}
        }
        assert!(p.status().ready);
        p.report(BootKind::Keyboard, &[0; 8], 10000);
        p.feedback(ProxyToHost::Consumed {
            session: s.session,
            generation: s.generation,
            sequence: 2,
        });
        assert_eq!(p.status().fault, ProxyFault::Sequence);
    }
    #[test]
    fn stale_feedback_and_ambiguous_devices_are_rejected() {
        let p = ready();
        let old = p.status();
        p.session(2);
        p.feedback(ProxyToHost::Ready {
            session: old.session,
            generation: old.generation,
        });
        assert!(!p.status().ready);
        p.support(ProxySupport::Ambiguous, ProxySupport::Boot);
        p.enable(p.status().generation, 0);
        assert_eq!(p.status().fault, ProxyFault::Unsupported);
    }
}
