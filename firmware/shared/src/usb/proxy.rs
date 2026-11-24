//! Active boot-HID proxy contracts. These are deliberately separate from capture
//! records: losing diagnostic history must never lose a forwarded key release.
//!
//! Protocol 10 appends `ProxyEnable`, `ProxyDisable` to `HostCommand`, `Proxy`
//! to `protocol::Event`, and `Proxy { status: ProxyStatus }` to `UiEvent`.
//! Enable is `{"command":{"proxy_enable":{"generation":N}}}` using the latest
//! snapshot generation. Both M4 admission and M7 execution reject stale intent;
//! M4 also rejects enables until M7 acknowledges a USB link/reset transition.
//! This is a concurrency guard, not authentication. Disable remains unconditional.
//!
//! M7 owns physical host transfers and the authoritative enable latch. M4 owns
//! report validation and USB-C output. Neither direction travels through UI/CDC
//! record queues. Each direction uses one ordered sequence space per generation.
//! Generations are changed on enable, session change, output reconnect or fault;
//! receivers reject stale generations. Sequence gaps fail closed, never replay
//! relative motion. An acknowledgment means consumption, not merely enqueueing.
//!
//! A local fault or output-link change requires a disabled reset before a later
//! enabled reset can arm M4; a queued pre-fault enable is never authorization.
//! A reset handshake and successful neutral keyboard/mouse writes must precede
//! `Ready`. Reports observed before Ready are discarded, not replayed. A new
//! physical report is required after Ready; a held state from before reconnect
//! must not be synthesized. Heartbeats attest to host service, not changing HID
//! input. Missing heartbeats fail closed; unchanged held keys alone do not.
//!
//! Uncertain USB-C output is terminal for the current firmware boot: the sole
//! lifecycle owner stops all I/O futures, physically disconnects the composite
//! device and never polls the old driver again. Its private OUT/SETUP buffers
//! cannot be safely reset through the locked HAL API. Only a full board reset
//! (not the USB host Reset command) clears the BSS-owned driver and terminal
//! latch. CDC is unavailable until then. `release` is independent of `enabled`:
//! pending/failed never asserts successful keyboard/button release.

use serde::{Deserialize, Serialize};

/// Host service heartbeat cadence and maximum receiver silence, in milliseconds.
/// Deadlines apply even when the physical device only NAKs interrupt polls.
pub const HEARTBEAT_INTERVAL_MS: u64 = 100;
pub const HEARTBEAT_TIMEOUT_MS: u64 = 500;
/// Private per-core forwarding capacity, excluding one retained transport frame.
/// Exhaustion is a fault, not permission to evict or coalesce relative reports.
pub const FORWARD_QUEUE_CAPACITY: usize = 16;
/// Maximum queued age and USB-C endpoint submission/retirement time.
pub const OUTPUT_REPORT_DEADLINE_MS: u64 = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxySupport {
    Absent,
    Boot,
    Unsupported,
    /// More than one interface of the same boot type; no aggregation is allowed.
    Ambiguous,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyFault {
    None,
    Unsupported,
    InvalidReport,
    Backpressure,
    Transport,
    Sequence,
    Heartbeat,
    Output,
    LedTransfer,
}

/// Neutralization proof, independent of the user's enable latch. Failed is
/// terminal until a full board reset; USB host Reset and reconnect cannot clear it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyRelease {
    Pending,
    Released,
    Failed,
}

/// Retained UI snapshot. Counters saturate and are cumulative since core boot;
/// they are not capture/IPC-UI/browser history loss counters. `enabled` is the
/// user's M7 latch, not proof that reports can flow. A fault clears that latch
/// and requires another explicit enable. USB host Start never enables proxying.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProxyStatus {
    pub enabled: bool,
    /// USB-C configured and not suspended; independent of CDC DTR.
    pub output_connected: bool,
    /// Reset acknowledged and both neutral reports completed for this generation.
    pub ready: bool,
    pub release: ProxyRelease,
    pub session: u32,
    pub generation: u32,
    pub keyboard: ProxySupport,
    pub mouse: ProxySupport,
    pub fault: ProxyFault,
    pub faults: u32,
    pub rejected_reports: u32,
    pub led_failures: u32,
}

/// M7 -> M4 dedicated payload. The enclosing IPC event will be Event::Proxy.
/// Full transport retries retain the exact frame; terminal failure invalidates
/// the generation. Session is the host's existing lifecycle session identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum HostToProxy {
    /// Coalesced authority snapshot; M4 retains it for CDC and publishes UI state.
    Status {
        status: ProxyStatus,
    },
    /// Invalidates every queued report and initiates neutralization of previously
    /// armed output. Diagnostics-only boot submits no HID interrupt reports;
    /// after an enable, disabling still requires confirmed neutral output.
    Reset {
        session: u32,
        generation: u32,
        enabled: bool,
    },
    Heartbeat {
        session: u32,
        generation: u32,
    },
    /// Raw physical boot packet, independent of expendable capture publication.
    /// Keyboard: exactly 8 bytes. Mouse: exactly 3 bytes; no wheel/report IDs.
    /// Only bytes below length are meaningful; remaining bytes must be zero.
    Report {
        session: u32,
        generation: u32,
        sequence: u32,
        kind: BootKind,
        length: u8,
        bytes: [u8; 8],
    },
    Support {
        session: u32,
        generation: u32,
        keyboard: ProxySupport,
        mouse: ProxySupport,
        unsupported_interfaces: u8,
    },
}

impl HostToProxy {
    pub fn generation(&self) -> u32 {
        match self {
            Self::Status { status } => status.generation,
            Self::Reset { generation, .. }
            | Self::Heartbeat { generation, .. }
            | Self::Report { generation, .. }
            | Self::Support { generation, .. } => *generation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum BootKind {
    Keyboard,
    Mouse,
}

/// M4 -> M7 dedicated payload. LED state is a latest-state value, unlike mouse
/// motion. M7 alone sends class SET_REPORT(Output, report ID 0), one byte with
/// bits 0..2 (Num/Caps/Scroll), to the selected physical keyboard interface.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProxyToHost {
    Release {
        session: u32,
        generation: u32,
        state: ProxyRelease,
    },
    /// Link changes invalidate the old generation before accepting new input.
    Output {
        connected: bool,
    },
    Ready {
        session: u32,
        generation: u32,
    },
    /// Written once to USB-C, or validated unchanged keyboard state consumed.
    /// Never retry a report after this acknowledgment, especially relative motion.
    Consumed {
        session: u32,
        generation: u32,
        sequence: u32,
    },
    Leds {
        session: u32,
        generation: u32,
        bits: u8,
    },
    Fault {
        session: u32,
        generation: u32,
        fault: ProxyFault,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Event, MAX_FRAME_SIZE, Message, RECORD_BYTES};

    fn roundtrip<T>(value: T)
    where
        T: Serialize + serde::de::DeserializeOwned + Eq + core::fmt::Debug,
    {
        let message = Message::encode(Event::Proxy, &value).unwrap();
        assert_eq!(message.decode::<T>().unwrap(), value);
        let mut bytes = [0; MAX_FRAME_SIZE];
        postcard::to_slice(&message, &mut bytes).unwrap();
    }

    #[test]
    fn dedicated_frames_fit_ipc_without_ui_chunking() {
        for frame in [
            HostToProxy::Reset {
                session: u32::MAX,
                generation: u32::MAX,
                enabled: true,
            },
            HostToProxy::Heartbeat {
                session: u32::MAX,
                generation: u32::MAX,
            },
            HostToProxy::Report {
                session: u32::MAX,
                generation: u32::MAX,
                sequence: u32::MAX,
                kind: BootKind::Keyboard,
                length: 8,
                bytes: [255; 8],
            },
            HostToProxy::Support {
                session: u32::MAX,
                generation: u32::MAX,
                keyboard: ProxySupport::Ambiguous,
                mouse: ProxySupport::Unsupported,
                unsupported_interfaces: 4,
            },
        ] {
            roundtrip(frame);
        }
        for frame in [
            ProxyToHost::Release {
                session: u32::MAX,
                generation: u32::MAX,
                state: ProxyRelease::Failed,
            },
            ProxyToHost::Output { connected: true },
            ProxyToHost::Ready {
                session: u32::MAX,
                generation: u32::MAX,
            },
            ProxyToHost::Consumed {
                session: u32::MAX,
                generation: u32::MAX,
                sequence: u32::MAX,
            },
            ProxyToHost::Leds {
                session: u32::MAX,
                generation: u32::MAX,
                bits: 7,
            },
            ProxyToHost::Fault {
                session: u32::MAX,
                generation: u32::MAX,
                fault: ProxyFault::Backpressure,
            },
        ] {
            roundtrip(frame);
        }
    }

    #[test]
    fn snapshot_json_is_bounded_and_has_stable_names() {
        let status = ProxyStatus {
            enabled: false,
            output_connected: true,
            ready: false,
            release: ProxyRelease::Pending,
            session: u32::MAX,
            generation: u32::MAX,
            keyboard: ProxySupport::Ambiguous,
            mouse: ProxySupport::Unsupported,
            fault: ProxyFault::LedTransfer,
            faults: u32::MAX,
            rejected_reports: u32::MAX,
            led_failures: u32::MAX,
        };
        let mut bytes = [0; RECORD_BYTES - 128];
        let size = serde_json_core::to_slice(&status, &mut bytes).unwrap();
        let json = core::str::from_utf8(&bytes[..size]).unwrap();
        assert!(json.contains("\"fault\":\"led_transfer\""));
        assert!(json.contains("\"keyboard\":\"ambiguous\""));
        let (decoded, used) = serde_json_core::from_slice::<ProxyStatus>(&bytes[..size]).unwrap();
        assert_eq!(decoded, status);
        assert_eq!(used, size);
    }
}
