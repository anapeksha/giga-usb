//! M4-local boot report validation and output state, independent of diagnostics.
use core::cell::RefCell;
use heapless::Deque;
use shared::usb::proxy::*;
use shared::usb::types::HostCommand;

pub const KEYBOARD_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x03, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x03, 0x91, 0x02, 0x95, 0x01, 0x75, 0x05, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xc0,
];
pub const MOUSE_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
    0xc0, 0xc0,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Report {
    pub kind: BootKind,
    pub bytes: [u8; 8],
    pub sequence: Option<u32>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Work {
    Neutral,
    Report(Report),
}
/// A superseded write submitted no bytes; any companion neutral write retired.
/// A submitted transfer must retire within its original deadline, even if a
/// logical proxy generation changes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputResult {
    Retired,
    Superseded,
    Failed,
}
impl OutputResult {
    pub fn combine(self, other: Self) -> Self {
        match (self, other) {
            (Self::Failed, _) | (_, Self::Failed) => Self::Failed,
            (Self::Superseded, _) | (_, Self::Superseded) => Self::Superseded,
            _ => Self::Retired,
        }
    }
}
pub struct Proxy(RefCell<State>);
struct State {
    session: u32,
    generation: u32,
    have_generation: bool,
    blocked: bool,
    revision: u32,
    enabled: bool,
    connected: bool,
    ready: bool,
    neutral: bool,
    neutral_required: bool,
    release: ProxyRelease,
    release_changed: bool,
    terminal: bool,
    io_epoch: u32,
    heartbeat: u64,
    expected: u32,
    queue: Deque<(Report, u64), FORWARD_QUEUE_CAPACITY>,
    feedback: Deque<ProxyToHost, FORWARD_QUEUE_CAPACITY>,
    output: Option<bool>,
    fault: Option<ProxyToHost>,
    leds_pending: bool,
    leds: u8,
    keyboard: [u8; 8],
    buttons: u8,
    protocol: [u8; 2],
    idle: [u32; 2],
    sent_at: [u64; 2],
}
impl Default for Proxy {
    fn default() -> Self {
        Self::new()
    }
}
fn index(kind: BootKind) -> usize {
    match kind {
        BootKind::Keyboard => 0,
        BootKind::Mouse => 1,
    }
}
/// Strict subset: ordinary six-key boot keyboard and three-button boot mouse.
/// ErrorRollOver/POSTFail/ErrorUndefined, duplicate usages, reserved byte/bits,
/// out-of-descriptor keyboard usages and -128 mouse axes are rejected.
pub fn validate(kind: BootKind, length: u8, bytes: &[u8; 8]) -> bool {
    match kind {
        BootKind::Keyboard => {
            length == 8
                && bytes[1] == 0
                && bytes[2..].iter().enumerate().all(|(i, key)| {
                    *key == 0 || ((4..=0x65).contains(key) && !bytes[2..2 + i].contains(key))
                })
        }
        BootKind::Mouse => {
            length == 3
                && bytes[0] & !7 == 0
                && bytes[1] != 128
                && bytes[2] != 128
                && bytes[3..].iter().all(|v| *v == 0)
        }
    }
}
impl Proxy {
    pub const fn new() -> Self {
        Self(RefCell::new(State {
            session: 0,
            generation: 0,
            have_generation: false,
            blocked: true,
            revision: 0,
            enabled: false,
            connected: false,
            ready: false,
            neutral: true,
            neutral_required: false,
            release: ProxyRelease::Pending,
            release_changed: false,
            terminal: false,
            io_epoch: 0,
            heartbeat: 0,
            expected: 0,
            queue: Deque::new(),
            feedback: Deque::new(),
            output: Some(false),
            fault: None,
            leds_pending: false,
            leds: 0,
            keyboard: [0; 8],
            buttons: 0,
            protocol: [1; 2],
            idle: [0; 2],
            sent_at: [0; 2],
        }))
    }
    pub fn connected(&self) -> bool {
        self.0.borrow().connected
    }
    pub fn io_epoch(&self) -> Option<u32> {
        let s = self.0.borrow();
        (s.connected && !s.terminal).then_some(s.io_epoch)
    }
    pub fn io_valid(&self, epoch: u32) -> bool {
        self.io_epoch() == Some(epoch)
    }
    /// M7 remains authoritative. While reset/link notifications await its new
    /// disabled generation, even an equal old generation is not fresh intent.
    pub fn command_allowed(&self, command: HostCommand) -> bool {
        match command {
            HostCommand::ProxyEnable { generation } => {
                let s = self.0.borrow();
                s.have_generation
                    && generation == s.generation
                    && s.connected
                    && !s.blocked
                    && !s.terminal
            }
            _ => true,
        }
    }
    pub fn terminal(&self) -> bool {
        self.0.borrow().terminal
    }
    pub fn release(&self) -> ProxyRelease {
        self.0.borrow().release
    }
    pub fn revision(&self) -> u32 {
        self.0.borrow().revision
    }
    pub fn valid(&self, revision: u32) -> bool {
        let s = self.0.borrow();
        s.connected && !s.terminal && s.revision == revision
    }
    pub fn link(&self, connected: bool) {
        let mut s = self.0.borrow_mut();
        if s.terminal && connected {
            return;
        }
        if s.connected == connected {
            return;
        }
        s.io_epoch = s.io_epoch.wrapping_add(1);
        s.connected = connected;
        s.output = Some(connected);
        s.blocked = true;
        s.clear();
        s.enabled = false;
    }
    pub fn bus_reset(&self) {
        self.link(false);
        let mut s = self.0.borrow_mut();
        s.io_epoch = s.io_epoch.wrapping_add(1);
        s.blocked = true;
        s.protocol = [1; 2];
        s.idle = [0; 2];
        s.leds = 0;
        s.clear();
        s.enabled = false;
    }
    pub fn receive(&self, frame: HostToProxy, now: u64) {
        let mut s = self.0.borrow_mut();
        if let HostToProxy::Reset {
            session,
            generation,
            enabled,
        } = frame
        {
            // Delayed reset from a previous in-flight frame cannot roll back state.
            if s.have_generation
                && (generation == s.generation
                    || generation.wrapping_sub(s.generation) >= 0x8000_0000)
            {
                return;
            }
            s.clear();
            s.feedback.clear();
            s.fault = None;
            s.session = session;
            s.generation = generation;
            s.have_generation = true;
            s.heartbeat = now;
            s.expected = 0;
            if s.terminal {
                s.enabled = false;
                s.blocked = true;
                return;
            }
            // A queued enable predating a local fault/link change is not fresh
            // authorization. First require M7 to acknowledge a disabled reset.
            if enabled && s.blocked {
                s.fail(ProxyFault::Transport);
                return;
            }
            s.enabled = enabled && s.connected;
            if s.enabled {
                s.neutral_required = true;
            }
            if !enabled {
                s.blocked = false;
            }
            return;
        }
        let (session, generation) = match frame {
            HostToProxy::Status { .. } => return,
            HostToProxy::Heartbeat {
                session,
                generation,
            }
            | HostToProxy::Report {
                session,
                generation,
                ..
            }
            | HostToProxy::Support {
                session,
                generation,
                ..
            } => (session, generation),
            HostToProxy::Reset { .. } => unreachable!(),
        };
        if !s.have_generation || session != s.session || generation != s.generation {
            return;
        }
        match frame {
            HostToProxy::Heartbeat { .. } => s.heartbeat = now,
            HostToProxy::Report {
                kind,
                length,
                bytes,
                sequence,
                ..
            } if s.enabled && s.ready => {
                if sequence != s.expected {
                    s.fail(ProxyFault::Sequence);
                    return;
                }
                if !validate(kind, length, &bytes) {
                    s.fail(ProxyFault::InvalidReport);
                    return;
                }
                s.expected = s.expected.wrapping_add(1);
                if s.queue
                    .push_back((
                        Report {
                            kind,
                            bytes,
                            sequence: Some(sequence),
                        },
                        now,
                    ))
                    .is_err()
                {
                    s.fail(ProxyFault::Backpressure);
                }
            }
            _ => {}
        }
    }
    pub fn tick(&self, now: u64) {
        let mut s = self.0.borrow_mut();
        if s.enabled && now.saturating_sub(s.heartbeat) >= HEARTBEAT_TIMEOUT_MS {
            s.fail(ProxyFault::Heartbeat);
        }
    }
    pub fn output_failed(&self) {
        let mut s = self.0.borrow_mut();
        s.fail(ProxyFault::Output);
        // No bus reset, host command, cable reconnect or IPC generation may
        // reopen this driver. Only board reset clears this BSS-owned latch.
        s.terminal = true;
        s.connected = false;
        s.output = Some(false);
        s.io_epoch = s.io_epoch.wrapping_add(1);
        s.release = ProxyRelease::Failed;
        s.release_changed = true;
        // Release::Failed is a reserved, generation-independent output fault.
        s.fault = None;
    }
    pub fn fail(&self, fault: ProxyFault) {
        self.0.borrow_mut().fail(fault);
    }
    pub fn work(&self, now: u64) -> Option<(u32, Work)> {
        let mut s = self.0.borrow_mut();
        if !s.connected || s.terminal {
            return None;
        }
        if s.neutral {
            // Diagnostics-only boot has never submitted proxy input. Do not
            // start interrupt writes/timeouts merely because USB configured.
            if !s.neutral_required {
                return None;
            }
            return Some((s.revision, Work::Neutral));
        }
        if !s.enabled || !s.ready {
            return None;
        }
        if let Some((report, received_at)) = s.queue.pop_front() {
            if now.saturating_sub(received_at) >= OUTPUT_REPORT_DEADLINE_MS {
                s.fail(ProxyFault::Backpressure);
                return Some((s.revision, Work::Neutral));
            }
            return Some((s.revision, Work::Report(report)));
        }
        for kind in [BootKind::Keyboard, BootKind::Mouse] {
            let i = index(kind);
            if s.idle[i] != 0 && now.saturating_sub(s.sent_at[i]) >= u64::from(s.idle[i]) {
                let mut bytes = [0; 8];
                match kind {
                    BootKind::Keyboard => bytes = s.keyboard,
                    BootKind::Mouse => bytes[0] = s.buttons,
                }
                return Some((
                    s.revision,
                    Work::Report(Report {
                        kind,
                        bytes,
                        sequence: None,
                    }),
                ));
            }
        }
        None
    }
    pub fn completed(&self, revision: u32, work: Work, now: u64) {
        let mut s = self.0.borrow_mut();
        if revision != s.revision || !s.connected {
            return;
        }
        match work {
            Work::Neutral => {
                s.neutral = false;
                s.release = ProxyRelease::Released;
                s.release_changed = !s.enabled;
                s.neutral_required = s.enabled;
                s.keyboard = [0; 8];
                s.buttons = 0;
                s.sent_at = [now; 2];
                if s.enabled {
                    s.ready = true;
                    let frame = ProxyToHost::Ready {
                        session: s.session,
                        generation: s.generation,
                    };
                    s.send(frame);
                    s.leds_pending = true;
                }
            }
            Work::Report(report) => {
                s.release = ProxyRelease::Pending;
                match report.kind {
                    BootKind::Keyboard => s.keyboard = report.bytes,
                    BootKind::Mouse => s.buttons = report.bytes[0],
                }
                s.sent_at[index(report.kind)] = now;
                if let Some(sequence) = report.sequence {
                    let frame = ProxyToHost::Consumed {
                        session: s.session,
                        generation: s.generation,
                        sequence,
                    };
                    s.send(frame);
                }
            }
        }
    }
    pub fn next_frame(&self) -> Option<ProxyToHost> {
        let mut s = self.0.borrow_mut();
        if let Some(connected) = s.output.take() {
            return Some(ProxyToHost::Output { connected });
        }
        if s.release_changed {
            s.release_changed = false;
            return Some(ProxyToHost::Release {
                session: s.session,
                generation: s.generation,
                state: s.release,
            });
        }
        if let Some(fault) = s.fault.take() {
            return Some(fault);
        }
        if let Some(frame) = s.feedback.pop_front() {
            return Some(frame);
        }
        if s.leds_pending && s.ready && s.enabled {
            s.leds_pending = false;
            return Some(ProxyToHost::Leds {
                session: s.session,
                generation: s.generation,
                bits: s.leds,
            });
        }
        None
    }
    pub fn protocol(&self, kind: BootKind) -> u8 {
        self.0.borrow().protocol[index(kind)]
    }
    pub fn set_protocol(&self, kind: BootKind, mode: u8) {
        let mut s = self.0.borrow_mut();
        if s.protocol[index(kind)] == mode {
            return;
        }
        s.protocol[index(kind)] = mode;
        // Same report bytes in both protocols; still clear all input on a change.
        if s.enabled {
            s.fail(ProxyFault::Output);
        } else {
            s.clear();
        }
    }
    pub fn idle(&self, kind: BootKind) -> u32 {
        self.0.borrow().idle[index(kind)]
    }
    pub fn set_idle(&self, kind: BootKind, ms: u32) {
        self.0.borrow_mut().idle[index(kind)] = if ms == u32::MAX { 0 } else { ms };
    }
    pub fn set_leds(&self, bits: u8) -> bool {
        if bits & !7 != 0 {
            return false;
        }
        let mut s = self.0.borrow_mut();
        s.leds = bits;
        s.leds_pending = true;
        true
    }
    pub fn get_report(&self, kind: BootKind, output: bool, data: &mut [u8]) -> Option<usize> {
        let s = self.0.borrow();
        if output {
            if kind != BootKind::Keyboard || data.is_empty() {
                return None;
            }
            data[0] = s.leds;
            return Some(1);
        }
        let n = if kind == BootKind::Keyboard { 8 } else { 3 };
        if data.len() < n {
            return None;
        }
        data[..n].fill(0);
        if s.enabled && s.ready && !s.neutral {
            match kind {
                BootKind::Keyboard => data[..8].copy_from_slice(&s.keyboard),
                BootKind::Mouse => data[0] = s.buttons,
            }
        }
        Some(n)
    }
}
impl State {
    fn clear(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.ready = false;
        self.neutral = true;
        if self.release != ProxyRelease::Failed {
            self.release = ProxyRelease::Pending;
        }
        self.queue.clear();
        self.keyboard = [0; 8];
        self.buttons = 0;
        self.leds_pending = false;
    }
    fn fail(&mut self, fault: ProxyFault) {
        self.blocked = true;
        self.clear();
        self.enabled = false;
        self.feedback.clear();
        self.fault = Some(ProxyToHost::Fault {
            session: self.session,
            generation: self.generation,
            fault,
        });
    }
    fn send(&mut self, frame: ProxyToHost) {
        if self.feedback.push_back(frame).is_err() {
            self.fail(ProxyFault::Backpressure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ready() -> Proxy {
        let p = Proxy::new();
        p.link(true);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 0,
                enabled: false,
            },
            0,
        );
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 1,
                enabled: true,
            },
            0,
        );
        let (rev, work) = p.work(0).unwrap();
        assert_eq!(work, Work::Neutral);
        p.completed(rev, work, 0);
        while p.next_frame().is_some() {}
        p
    }
    fn report(p: &Proxy, sequence: u32, kind: BootKind, bytes: [u8; 8]) {
        p.receive(
            HostToProxy::Report {
                session: 1,
                generation: 1,
                sequence,
                kind,
                length: if kind == BootKind::Keyboard { 8 } else { 3 },
                bytes,
            },
            1,
        );
    }
    #[test]
    fn complete_old_cdc_enable_is_stale_after_ordinary_reset_configuration_and_dtr() {
        use shared::usb::types::CommandRequest;
        let p = Proxy::new();
        let bus = crate::ui_bus::Bus::new(|| 0);
        p.link(true);
        bus.connection(true);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 7,
                enabled: false,
            },
            0,
        );
        let old = CommandRequest {
            command: HostCommand::ProxyEnable { generation: 7 },
        };
        assert!(p.command_allowed(old.command));
        // Model a complete OUT packet retained privately by the driver. Decode
        // these exact old bytes only after a new configuration/DTR session.
        let mut packet = [0; 128];
        let n = serde_json_core::to_slice(&old, &mut packet).unwrap();
        let epoch = p.io_epoch().unwrap();
        p.bus_reset();
        bus.connection(false);
        p.link(true);
        bus.connection(true);
        assert!(p.io_epoch().is_some());
        assert!(!p.io_valid(epoch));
        assert!(!p.terminal());
        assert_eq!(p.work(1), None);
        let (delayed, used) = serde_json_core::from_slice::<CommandRequest>(&packet[..n]).unwrap();
        assert_eq!(used, n);
        assert_eq!(delayed.command, old.command);
        // Before M7 processes Output, the local generation is still 7, but the
        // link/reset gate is blocked. An old same-generation reset cannot open it.
        assert!(!p.command_allowed(delayed.command));
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 7,
                enabled: false,
            },
            1,
        );
        assert!(!p.command_allowed(delayed.command));
        // Ordered Output notification advances M7's authoritative generation.
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 8,
                enabled: false,
            },
            2,
        );
        assert!(!p.command_allowed(delayed.command));
        if p.command_allowed(delayed.command) {
            bus.accept_command(delayed.command).unwrap();
        }
        assert_eq!(bus.command(), None);
        assert!(!p.0.borrow().enabled);
        assert!(!p.command_allowed(HostCommand::ProxyEnable { generation: 9 }));
        assert!(p.command_allowed(HostCommand::ProxyEnable { generation: 8 }));
        assert!(p.command_allowed(HostCommand::ProxyDisable));
        bus.accept_command(HostCommand::Status).unwrap();
        assert_eq!(bus.command(), Some(HostCommand::Status));
        assert!(p.io_epoch().is_some());
        assert!(!p.terminal());
    }
    #[test]
    fn retained_ipc_enable_is_rechecked_after_authoritative_generation_changes() {
        use shared::{
            ipc::SendError,
            ipc_delivery::Pending,
            protocol::{Event, Message},
        };
        let p = Proxy::new();
        p.link(true);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 7,
                enabled: false,
            },
            0,
        );
        let command = HostCommand::ProxyEnable { generation: 7 };
        assert!(p.command_allowed(command));
        let mut pending = Pending::new();
        pending.set(Some(Message::encode(Event::Command, &command).unwrap()));
        pending.retry(|_| Err(SendError::Full)).unwrap();
        let epoch = p.io_epoch();
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 8,
                enabled: false,
            },
            1,
        );
        assert_eq!(p.io_epoch(), epoch); // not just the USB epoch cleanup policy
        assert!(pending.discard_if(|message| {
            message.event == Event::Command
                && message
                    .decode::<HostCommand>()
                    .is_ok_and(|command| !p.command_allowed(command))
        }));
        assert!(pending.is_none());
        assert!(!p.0.borrow().enabled);
        assert!(!p.terminal());
    }
    #[test]
    fn enable_before_initial_neutral_retirement_and_disable_do_not_latch() {
        let p = Proxy::new();
        p.link(true);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 0,
                enabled: false,
            },
            0,
        );
        let epoch = p.io_epoch().unwrap();
        assert_eq!(p.work(0), None);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 1,
                enabled: true,
            },
            1,
        );
        let (old_revision, old_work) = p.work(1).unwrap();
        assert_eq!(old_work, Work::Neutral);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 2,
                enabled: false,
            },
            1,
        );
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 3,
                enabled: true,
            },
            1,
        );
        // Before FIFO submission the old write is safely superseded. A different
        // neutral endpoint that already retired does not make this an error.
        assert!(!p.valid(old_revision));
        assert!(p.io_valid(epoch));
        assert_eq!(
            OutputResult::Superseded.combine(OutputResult::Retired),
            OutputResult::Superseded
        );
        p.completed(old_revision, old_work, 1);
        assert!(!p.0.borrow().ready);
        let (revision, work) = p.work(1).unwrap();
        assert_eq!(work, Work::Neutral);
        p.completed(revision, work, 2);
        assert!(p.0.borrow().ready);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 4,
                enabled: false,
            },
            3,
        );
        let (revision, work) = p.work(3).unwrap();
        p.completed(revision, work, 4);
        assert_eq!(p.release(), ProxyRelease::Released);
        assert!(p.io_valid(epoch));
        assert!(!p.terminal());
        assert!(!p.0.borrow().enabled);
    }
    #[test]
    fn default_diagnostics_never_start_hid_writes_or_output_deadlines() {
        let p = Proxy::new();
        p.link(true);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 0,
                enabled: false,
            },
            0,
        );
        for now in [0, 50, 500, 5000, 60000] {
            p.tick(now);
            assert_eq!(p.work(now), None);
            assert!(!p.terminal());
            assert!(p.io_epoch().is_some());
        }
        assert_eq!(p.release(), ProxyRelease::Pending);
        assert!(!p.0.borrow().enabled);
    }
    #[test]
    fn logical_transitions_quiesce_one_submitted_report_without_replay() {
        for kind in [BootKind::Keyboard, BootKind::Mouse] {
            for enable in [false, true] {
                let p = ready();
                let epoch = p.io_epoch().unwrap();
                let bytes = if kind == BootKind::Keyboard {
                    [0, 0, 4, 0, 0, 0, 0, 0]
                } else {
                    [1, 20, 255, 0, 0, 0, 0, 0]
                };
                report(&p, 0, kind, bytes);
                let (old_revision, old_work) = p.work(1).unwrap();
                p.receive(
                    HostToProxy::Reset {
                        session: 1,
                        generation: 2,
                        enabled: false,
                    },
                    2,
                );
                if enable {
                    p.receive(
                        HostToProxy::Reset {
                            session: 1,
                            generation: 3,
                            enabled: true,
                        },
                        2,
                    );
                }
                // Already-submitted bytes may drain under this unchanged USB
                // epoch. No new write is allowed under the obsolete revision.
                assert!(p.io_valid(epoch));
                assert!(!p.valid(old_revision));
                p.completed(old_revision, old_work, 3);
                let (revision, work) = p.work(3).unwrap();
                assert_eq!(work, Work::Neutral);
                p.completed(revision, work, 4);
                assert_eq!(p.work(4), None);
                assert!(!p.terminal());
                assert!(p.io_valid(epoch));
                assert_eq!(p.0.borrow().ready, enable);
            }
        }
    }
    #[test]
    fn partial_neutral_completion_never_hides_an_output_failure() {
        for first in [
            OutputResult::Retired,
            OutputResult::Superseded,
            OutputResult::Failed,
        ] {
            assert_eq!(first.combine(OutputResult::Failed), OutputResult::Failed);
            assert_eq!(OutputResult::Failed.combine(first), OutputResult::Failed);
        }
        assert_eq!(
            OutputResult::Retired.combine(OutputResult::Retired),
            OutputResult::Retired
        );
    }
    #[test]
    fn disabled_neutralization_failure_is_published_and_terminal_until_board_reset() {
        let p = ready();
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 2,
                enabled: false,
            },
            1,
        );
        assert_eq!(p.release(), ProxyRelease::Pending);
        let (revision, work) = p.work(1).unwrap();
        assert_eq!(work, Work::Neutral);
        p.output_failed();
        p.completed(revision, work, 2); // obsolete/partial completion is not proof
        assert_eq!(p.release(), ProxyRelease::Failed);
        assert_eq!(
            p.next_frame(),
            Some(ProxyToHost::Output { connected: false })
        );
        assert_eq!(
            p.next_frame(),
            Some(ProxyToHost::Release {
                session: 1,
                generation: 2,
                state: ProxyRelease::Failed
            })
        );
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 3,
                enabled: false,
            },
            3,
        );
        assert_eq!(p.release(), ProxyRelease::Failed);
        assert_eq!(p.work(3), None);
        assert!(p.terminal());
        assert!(!p.0.borrow().enabled);
        assert_eq!(p.release(), ProxyRelease::Failed);
        // A new BSS-initialized instance represents full board boot, not a host
        // Reset command or a reconstruction of the retained USB device.
        let rebooted = Proxy::new();
        assert!(!rebooted.terminal());
        assert_eq!(rebooted.release(), ProxyRelease::Pending);
    }
    #[test]
    fn terminal_latch_rejects_bus_reset_reconnect_commands_and_stale_completion() {
        let p = ready();
        let epoch = p.io_epoch().unwrap();
        report(&p, 0, BootKind::Keyboard, [0, 0, 4, 0, 0, 0, 0, 0]);
        let (revision, work) = p.work(1).unwrap();
        p.output_failed();
        assert!(!p.io_valid(epoch));
        assert_eq!(p.work(1), None);
        p.link(true); // stale configuration/resume is not a new USB reset
        assert_eq!(p.io_epoch(), None);
        p.completed(revision, work, 2);
        assert_eq!(p.release(), ProxyRelease::Failed);
        p.bus_reset();
        assert_eq!(p.io_epoch(), None);
        p.link(true);
        p.receive(
            HostToProxy::Reset {
                session: 2,
                generation: 2,
                enabled: false,
            },
            3,
        );
        p.receive(
            HostToProxy::Reset {
                session: 2,
                generation: 3,
                enabled: true,
            },
            4,
        );
        p.receive(
            HostToProxy::Report {
                session: 2,
                generation: 3,
                sequence: 0,
                kind: BootKind::Mouse,
                length: 3,
                bytes: [1, 20, 0, 0, 0, 0, 0, 0],
            },
            5,
        );
        p.bus_reset();
        p.link(true);
        assert_eq!(p.io_epoch(), None);
        assert!(p.terminal());
        assert_eq!(p.release(), ProxyRelease::Failed);
        assert_eq!(p.work(5), None);
        assert!(!p.0.borrow().enabled);
    }
    #[test]
    fn a_queued_enable_cannot_rearm_after_fault_or_output_reconnect() {
        for reconnect in [false, true] {
            let p = ready();
            if reconnect {
                p.link(false);
                p.link(true);
            } else {
                p.fail(ProxyFault::Transport);
            }
            p.receive(
                HostToProxy::Reset {
                    session: 1,
                    generation: 2,
                    enabled: true,
                },
                2,
            );
            let (rev, work) = p.work(2).unwrap();
            assert_eq!(work, Work::Neutral);
            p.completed(rev, work, 2);
            assert_eq!(p.work(2), None);
            let mut ready_seen = false;
            while let Some(frame) = p.next_frame() {
                ready_seen |= matches!(frame, ProxyToHost::Ready { .. });
            }
            assert!(!ready_seen);
            p.receive(
                HostToProxy::Reset {
                    session: 1,
                    generation: 3,
                    enabled: false,
                },
                3,
            );
            p.receive(
                HostToProxy::Reset {
                    session: 1,
                    generation: 4,
                    enabled: true,
                },
                4,
            );
            let (rev, work) = p.work(4).unwrap();
            p.completed(rev, work, 4);
            assert_eq!(
                p.next_frame(),
                Some(ProxyToHost::Ready {
                    session: 1,
                    generation: 4
                })
            );
        }
    }
    #[test]
    fn led_state_coalesces_and_protocol_reset_is_explicit() {
        let p = Proxy::new();
        p.link(true);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 0,
                enabled: false,
            },
            0,
        );
        p.set_protocol(BootKind::Keyboard, 0);
        p.receive(
            HostToProxy::Reset {
                session: 1,
                generation: 1,
                enabled: true,
            },
            1,
        );
        let (revision, work) = p.work(1).unwrap();
        p.completed(revision, work, 1);
        assert!(
            p.0.borrow().ready,
            "normal pre-enable protocol negotiation is not a fault"
        );
        let p = ready();
        assert!(p.set_leds(1));
        assert!(p.set_leds(7));
        assert!(!p.set_leds(8));
        assert_eq!(
            p.next_frame(),
            Some(ProxyToHost::Leds {
                session: 1,
                generation: 1,
                bits: 7
            })
        );
        assert_eq!(p.next_frame(), None);
        p.set_idle(BootKind::Keyboard, 20);
        assert_eq!(p.idle(BootKind::Keyboard), 20);
        p.set_protocol(BootKind::Keyboard, 0);
        assert_eq!(p.protocol(BootKind::Keyboard), 0);
        assert!(matches!(p.work(1), Some((_, Work::Neutral))));
        p.bus_reset();
        assert_eq!(p.protocol(BootKind::Keyboard), 1);
        assert_eq!(p.idle(BootKind::Keyboard), 0);
        let mut bytes = [255];
        p.get_report(BootKind::Keyboard, true, &mut bytes);
        assert_eq!(bytes, [0]);
    }
    #[test]
    fn expired_mouse_motion_is_neutralized_not_replayed() {
        let p = ready();
        report(&p, 0, BootKind::Mouse, [1, 20, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(
            p.work(1 + OUTPUT_REPORT_DEADLINE_MS),
            Some((_, Work::Neutral))
        ));
        assert_eq!(
            p.next_frame(),
            Some(ProxyToHost::Fault {
                session: 1,
                generation: 1,
                fault: ProxyFault::Backpressure
            })
        );
    }
    #[test]
    fn feedback_pressure_and_bad_report_emit_fault_and_neutral() {
        let p = ready();
        for sequence in 0..=FORWARD_QUEUE_CAPACITY as u32 {
            report(&p, sequence, BootKind::Keyboard, [0; 8]);
            let (revision, work) = p.work(1).unwrap();
            p.completed(revision, work, 1);
        }
        assert!(matches!(p.work(1), Some((_, Work::Neutral))));
        assert_eq!(
            p.next_frame(),
            Some(ProxyToHost::Fault {
                session: 1,
                generation: 1,
                fault: ProxyFault::Backpressure
            })
        );
        let p = ready();
        report(&p, 0, BootKind::Keyboard, [0, 0, 1, 1, 1, 1, 1, 1]);
        assert!(matches!(p.work(1), Some((_, Work::Neutral))));
        assert_eq!(
            p.next_frame(),
            Some(ProxyToHost::Fault {
                session: 1,
                generation: 1,
                fault: ProxyFault::InvalidReport
            })
        );
    }
    #[test]
    fn stale_heartbeat_cannot_keep_a_new_generation_armed() {
        let p = ready();
        p.receive(
            HostToProxy::Heartbeat {
                session: 0,
                generation: 0,
            },
            499,
        );
        p.tick(500);
        assert!(matches!(p.work(500), Some((_, Work::Neutral))));
        assert_eq!(
            p.next_frame(),
            Some(ProxyToHost::Fault {
                session: 1,
                generation: 1,
                fault: ProxyFault::Heartbeat
            })
        );
        assert!(core::mem::size_of::<Proxy>() <= 4096);
    }
    #[test]
    fn strict_boot_reports() {
        assert!(validate(BootKind::Keyboard, 8, &[2, 0, 4, 5, 0, 0, 0, 0]));
        for bytes in [
            [0, 1, 4, 0, 0, 0, 0, 0],
            [0, 0, 1, 1, 1, 1, 1, 1],
            [0, 0, 4, 4, 0, 0, 0, 0],
            [0, 0, 0x66, 0, 0, 0, 0, 0],
        ] {
            assert!(!validate(BootKind::Keyboard, 8, &bytes));
        }
        assert!(!validate(BootKind::Keyboard, 7, &[0; 8]));
        assert!(validate(BootKind::Mouse, 3, &[7, 255, 127, 0, 0, 0, 0, 0]));
        assert!(!validate(BootKind::Mouse, 4, &[0; 8]));
        assert!(!validate(BootKind::Mouse, 3, &[8, 0, 0, 0, 0, 0, 0, 0]));
        assert!(!validate(BootKind::Mouse, 3, &[0, 128, 0, 0, 0, 0, 0, 0]));
    }
    #[test]
    fn keyboard_and_button_releases_preserve_order() {
        let p = ready();
        for (seq, kind, bytes) in [
            (0, BootKind::Keyboard, [0, 0, 4, 0, 0, 0, 0, 0]),
            (1, BootKind::Keyboard, [0; 8]),
            (2, BootKind::Mouse, [1, 0, 0, 0, 0, 0, 0, 0]),
            (3, BootKind::Mouse, [0; 8]),
        ] {
            report(&p, seq, kind, bytes);
            let (rev, work) = p.work(1).unwrap();
            assert_eq!(
                work,
                Work::Report(Report {
                    kind,
                    bytes,
                    sequence: Some(seq)
                })
            );
            p.completed(rev, work, 1);
            assert_eq!(
                p.next_frame(),
                Some(ProxyToHost::Consumed {
                    session: 1,
                    generation: 1,
                    sequence: seq
                })
            );
        }
    }
    #[test]
    fn idle_mouse_never_replays_motion_and_get_report_is_stationary() {
        let p = ready();
        p.set_idle(BootKind::Mouse, 4);
        report(&p, 0, BootKind::Mouse, [1, 20, 255, 0, 0, 0, 0, 0]);
        let (rev, work) = p.work(1).unwrap();
        p.completed(rev, work, 1);
        let (_, work) = p.work(5).unwrap();
        assert_eq!(
            work,
            Work::Report(Report {
                kind: BootKind::Mouse,
                bytes: [1, 0, 0, 0, 0, 0, 0, 0],
                sequence: None
            })
        );
        let mut bytes = [0; 3];
        p.get_report(BootKind::Mouse, false, &mut bytes);
        assert_eq!(bytes, [1, 0, 0]);
    }
    #[test]
    fn detach_disable_protocol_change_and_watchdog_neutralize() {
        for action in 0..4 {
            let p = ready();
            report(&p, 0, BootKind::Keyboard, [0, 0, 4, 0, 0, 0, 0, 0]);
            let (rev, work) = p.work(1).unwrap();
            match action {
                0 => {
                    p.link(false);
                    p.link(true);
                }
                1 => p.receive(
                    HostToProxy::Reset {
                        session: 1,
                        generation: 2,
                        enabled: false,
                    },
                    2,
                ),
                2 => p.set_protocol(BootKind::Keyboard, 0),
                _ => p.tick(500),
            }
            p.completed(rev, work, 2); // obsolete completion must not acknowledge or restore held input
            assert!(matches!(p.work(2), Some((_, Work::Neutral))));
            let mut bytes = [255; 8];
            p.get_report(BootKind::Keyboard, false, &mut bytes);
            assert_eq!(bytes, [0; 8]);
        }
    }
    #[test]
    fn stale_frames_gaps_overflow_and_transport_fail_closed() {
        let p = ready();
        report(&p, 1, BootKind::Mouse, [0; 8]);
        assert!(matches!(p.work(1), Some((_, Work::Neutral))));
        let p = ready();
        for seq in 0..=FORWARD_QUEUE_CAPACITY {
            report(&p, seq as u32, BootKind::Keyboard, [0; 8]);
        }
        assert!(matches!(p.work(1), Some((_, Work::Neutral))));
        let p = ready();
        p.fail(ProxyFault::Transport);
        p.receive(
            HostToProxy::Reset {
                session: 0,
                generation: 0,
                enabled: true,
            },
            2,
        );
        report(&p, 0, BootKind::Mouse, [0, 10, 0, 0, 0, 0, 0, 0]);
        let (rev, work) = p.work(2).unwrap();
        assert_eq!(work, Work::Neutral);
        p.completed(rev, work, 2);
        assert_eq!(p.work(2), None);
    }
    #[test]
    fn heartbeat_does_not_release_held_keyboard() {
        let p = ready();
        report(&p, 0, BootKind::Keyboard, [0, 0, 4, 0, 0, 0, 0, 0]);
        let (rev, work) = p.work(1).unwrap();
        p.completed(rev, work, 1);
        for now in (100..10000).step_by(100) {
            p.receive(
                HostToProxy::Heartbeat {
                    session: 1,
                    generation: 1,
                },
                now,
            );
            p.tick(now);
        }
        let mut bytes = [0; 8];
        p.get_report(BootKind::Keyboard, false, &mut bytes);
        assert_eq!(bytes[2], 4);
    }
}
