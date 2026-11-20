//! Local USB and AP status indication; independent of the wire format.
use shared::usb::types::{HostState, UsbEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rgb {
    Off,
    Red,
    Green,
    Blue,
    Yellow,
    Cyan,
    White,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkMode {
    AccessPoint,
    Error,
}

#[derive(Clone, Copy)]
pub struct Input {
    pub host: HostState,
    pub network: Option<NetworkMode>,
    pub activity_ms: Option<u64>,
    session: u32,
    fault: bool,
}

impl Default for Input {
    fn default() -> Self {
        Self::new()
    }
}

impl Input {
    pub const fn new() -> Self {
        Self {
            host: HostState::Stopped,
            network: None,
            activity_ms: None,
            session: 0,
            fault: false,
        }
    }

    pub fn observe(&mut self, event: &UsbEvent, now_ms: u64) {
        match event {
            UsbEvent::Lifecycle { state, session_id } => {
                if self.session != *session_id {
                    self.session = *session_id;
                    self.fault = false;
                    self.activity_ms = None;
                }
                if self.host != *state {
                    self.activity_ms = None;
                    if matches!(
                        state,
                        HostState::Ready | HostState::Waiting | HostState::Stopped
                    ) {
                        self.fault = false;
                    }
                }
                self.host = *state;
                if *state == HostState::Error {
                    self.fault = true;
                }
            }
            UsbEvent::Transfer { .. } => self.activity_ms = Some(now_ms),
            _ => {}
        }
    }
}

fn pulse(on: bool, color: Rgb) -> Rgb {
    if on { color } else { Rgb::Off }
}

/// Fixed on/off patterns, not PWM. Time and activity use the same monotonic clock.
pub fn select(input: &Input, now_ms: u64) -> Rgb {
    let phase = now_ms % 2000;
    if input.fault || input.host == HostState::Error || input.network == Some(NetworkMode::Error) {
        return pulse(phase < 600 && phase % 200 < 100, Rgb::Red);
    }
    if matches!(input.host, HostState::Attached | HostState::Enumerating) {
        return pulse(now_ms % 200 < 100, Rgb::Yellow);
    }
    if input.host == HostState::Ready
        && input
            .activity_ms
            .is_some_and(|at| now_ms.checked_sub(at).is_some_and(|age| age < 80))
    {
        return Rgb::Cyan;
    }
    match input.host {
        HostState::Ready => Rgb::Green,
        HostState::Waiting => pulse(now_ms % 1000 < 150, Rgb::Yellow),
        _ => match input.network {
            Some(NetworkMode::AccessPoint) => {
                pulse(phase < 100 || (250..350).contains(&phase), Rgb::Blue)
            }
            _ => pulse(phase < 500, Rgb::White),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::usb::types::Direction;

    fn lifecycle(input: &mut Input, state: HostState, session_id: u32) {
        input.observe(&UsbEvent::Lifecycle { state, session_id }, 0);
    }

    #[test]
    fn startup_and_ap_idle_boundaries() {
        let mut input = Input::new();
        for (time, expected) in [
            (0, Rgb::White),
            (499, Rgb::White),
            (500, Rgb::Off),
            (1999, Rgb::Off),
            (2000, Rgb::White),
        ] {
            assert_eq!(select(&input, time), expected);
        }
        input.network = Some(NetworkMode::AccessPoint);
        for (time, expected) in [
            (0, Rgb::Blue),
            (99, Rgb::Blue),
            (100, Rgb::Off),
            (249, Rgb::Off),
            (250, Rgb::Blue),
            (349, Rgb::Blue),
            (350, Rgb::Off),
            (1999, Rgb::Off),
            (2000, Rgb::Blue),
        ] {
            assert_eq!(select(&input, time), expected);
        }
    }

    #[test]
    fn usb_states_are_visible_while_the_ap_is_available() {
        let mut input = Input::new();
        for network in [None, Some(NetworkMode::AccessPoint)] {
            input.network = network;
            lifecycle(&mut input, HostState::Waiting, 1);
            for (time, expected) in [
                (0, Rgb::Yellow),
                (149, Rgb::Yellow),
                (150, Rgb::Off),
                (999, Rgb::Off),
                (1000, Rgb::Yellow),
            ] {
                assert_eq!(select(&input, time), expected);
            }
            for state in [HostState::Attached, HostState::Enumerating] {
                lifecycle(&mut input, state, 1);
                assert_eq!(select(&input, 99), Rgb::Yellow);
                assert_eq!(select(&input, 100), Rgb::Off);
                assert_eq!(select(&input, 200), Rgb::Yellow);
            }
            lifecycle(&mut input, HostState::Ready, 1);
            for time in [0, 100, 250, 350, 1999] {
                assert_eq!(select(&input, time), Rgb::Green);
            }
        }
    }

    #[test]
    fn completed_packet_activity_expires_and_does_not_override_enumeration() {
        let mut input = Input::new();
        input.network = Some(NetworkMode::AccessPoint);
        lifecycle(&mut input, HostState::Ready, 1);
        input.observe(
            &UsbEvent::Transfer {
                endpoint: 0x81,
                direction: Direction::In,
                bytes: 0,
                pid: 0,
                data: heapless::Vec::new(),
            },
            1000,
        );
        // Periodic lifecycle snapshots must not erase a recent activity indication.
        lifecycle(&mut input, HostState::Ready, 1);
        assert_eq!(select(&input, 999), Rgb::Green);
        assert_eq!(select(&input, 1000), Rgb::Cyan);
        assert_eq!(select(&input, 1079), Rgb::Cyan);
        assert_eq!(select(&input, 1080), Rgb::Green);
        lifecycle(&mut input, HostState::Enumerating, 1);
        input.activity_ms = Some(1000);
        assert_eq!(select(&input, 1000), Rgb::Yellow);
        lifecycle(&mut input, HostState::Stopped, 1);
        assert_eq!(input.activity_ms, None);
        assert_eq!(select(&input, 2000), Rgb::Blue);
    }

    #[test]
    fn faults_have_priority_and_clear_on_recovery() {
        let mut input = Input::new();
        input.network = Some(NetworkMode::AccessPoint);
        lifecycle(&mut input, HostState::Error, 1);
        for (time, expected) in [
            (0, Rgb::Red),
            (99, Rgb::Red),
            (100, Rgb::Off),
            (200, Rgb::Red),
            (400, Rgb::Red),
            (499, Rgb::Red),
            (500, Rgb::Off),
            (600, Rgb::Off),
            (2000, Rgb::Red),
        ] {
            assert_eq!(select(&input, time), expected);
        }
        lifecycle(&mut input, HostState::Enumerating, 1);
        assert_eq!(select(&input, 0), Rgb::Red);
        lifecycle(&mut input, HostState::Enumerating, 2);
        assert_eq!(select(&input, 0), Rgb::Yellow);
        for (state, expected) in [
            (HostState::Ready, Rgb::Green),
            (HostState::Waiting, Rgb::Yellow),
            (HostState::Stopped, Rgb::Blue),
        ] {
            lifecycle(&mut input, HostState::Error, 2);
            lifecycle(&mut input, state, 2);
            assert_eq!(select(&input, 0), expected);
        }
        input.network = Some(NetworkMode::Error);
        for state in [HostState::Ready, HostState::Waiting, HostState::Enumerating] {
            lifecycle(&mut input, state, 2);
            input.activity_ms = Some(0);
            assert_eq!(select(&input, 0), Rgb::Red);
        }
        input.network = Some(NetworkMode::AccessPoint);
        lifecycle(&mut input, HostState::Stopped, 2);
        assert_eq!(select(&input, 0), Rgb::Blue);
    }
}
