//! Local status indication; no wire-format or credential data.
use shared::usb::types::{HostState, UsbEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rgb {
    Off,
    Red,
    Green,
    Blue,
    Yellow,
    Cyan,
    Magenta,
    White,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkMode {
    Setup,
    Connecting,
    Connected,
    Error,
}

#[derive(Clone, Copy)]
pub struct Input {
    pub host: HostState,
    pub network: Option<NetworkMode>,
    pub saving: bool,
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
            saving: false,
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
    if input.fault || input.network == Some(NetworkMode::Error) {
        return pulse(phase < 600 && phase % 200 < 100, Rgb::Red);
    }
    if input.saving {
        return pulse(now_ms % 200 < 100, Rgb::Magenta);
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
    match input.network {
        None => return pulse(phase < 500, Rgb::White),
        Some(NetworkMode::Setup) => {
            return pulse(phase < 100 || (250..350).contains(&phase), Rgb::Blue);
        }
        Some(NetworkMode::Connecting) => return pulse(now_ms % 1000 < 500, Rgb::Cyan),
        _ => {}
    }
    match input.host {
        HostState::Ready => Rgb::Green,
        HostState::Waiting => pulse(now_ms % 1000 < 150, Rgb::Yellow),
        _ => pulse(phase < 75, Rgb::Green),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lifecycle(input: &mut Input, state: HostState, session_id: u32) {
        input.observe(&UsbEvent::Lifecycle { state, session_id }, 0);
    }

    #[test]
    fn pattern_boundaries() {
        let mut input = Input::new();
        assert_eq!(select(&input, 499), Rgb::White);
        assert_eq!(select(&input, 500), Rgb::Off);
        input.network = Some(NetworkMode::Setup);
        for (time, expected) in [
            (0, Rgb::Blue),
            (100, Rgb::Off),
            (250, Rgb::Blue),
            (350, Rgb::Off),
            (2000, Rgb::Blue),
        ] {
            assert_eq!(select(&input, time), expected);
        }
        input.network = Some(NetworkMode::Connecting);
        assert_eq!(select(&input, 499), Rgb::Cyan);
        assert_eq!(select(&input, 500), Rgb::Off);
        input.network = Some(NetworkMode::Connected);
        assert_eq!(select(&input, 74), Rgb::Green);
        assert_eq!(select(&input, 75), Rgb::Off);
        input.host = HostState::Waiting;
        assert_eq!(select(&input, 149), Rgb::Yellow);
        assert_eq!(select(&input, 150), Rgb::Off);
        input.host = HostState::Ready;
        assert_eq!(select(&input, 0), Rgb::Green);
        input.activity_ms = Some(1000);
        assert_eq!(select(&input, 999), Rgb::Green);
        assert_eq!(select(&input, 1079), Rgb::Cyan);
        assert_eq!(select(&input, 1080), Rgb::Green);
    }

    #[test]
    fn priority_and_recovery() {
        let mut input = Input::new();
        input.network = Some(NetworkMode::Setup);
        lifecycle(&mut input, HostState::Ready, 1);
        input.activity_ms = Some(0);
        assert_eq!(select(&input, 0), Rgb::Cyan);
        lifecycle(&mut input, HostState::Enumerating, 1);
        assert_eq!(select(&input, 0), Rgb::Yellow);
        input.saving = true;
        assert_eq!(select(&input, 0), Rgb::Magenta);
        assert_eq!(select(&input, 100), Rgb::Off);
        lifecycle(&mut input, HostState::Error, 1);
        for (time, expected) in [
            (0, Rgb::Red),
            (100, Rgb::Off),
            (200, Rgb::Red),
            (400, Rgb::Red),
            (500, Rgb::Off),
            (600, Rgb::Off),
            (2000, Rgb::Red),
        ] {
            assert_eq!(select(&input, time), expected);
        }
        lifecycle(&mut input, HostState::Enumerating, 1);
        assert_eq!(select(&input, 0), Rgb::Red);
        lifecycle(&mut input, HostState::Enumerating, 2);
        assert_eq!(select(&input, 0), Rgb::Magenta);
        input.saving = false;
        for state in [HostState::Ready, HostState::Waiting, HostState::Stopped] {
            lifecycle(&mut input, HostState::Error, 2);
            lifecycle(&mut input, state, 2);
            assert_eq!(select(&input, 0), Rgb::Blue);
        }
        input.network = Some(NetworkMode::Error);
        assert_eq!(select(&input, 0), Rgb::Red);
        input.network = Some(NetworkMode::Setup);
        assert_eq!(select(&input, 0), Rgb::Blue);
    }
}
