//! Local OTA admission/state contract. Tokens guard stale requests, not identity.
//! The open AP is trusted-only; Origin plus a non-simple header rejects browser
//! CSRF, not a malicious client on the LAN. No automatic update is authorized.
use core::cell::RefCell;
use serde::Serialize;
use shared::ipc_delivery::Queue;

pub const CHUNK_SIZE: usize = 4096;
pub const AUTHORIZATION_MS: u64 = 120_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Checking,
    Idle,
    Authorized,
    Quiescing,
    Receiving,
    Verifying,
    Ready,
    Committing,
    Armed,
    Cancelled,
    Fault,
}

#[derive(Clone, Copy, Serialize)]
pub struct Status {
    pub build_id: &'static str,
    pub phase: Phase,
    pub token: Option<u32>,
    pub expires_ms: u64,
    /// Next absolute byte offset in the .ota.bin file (manifest was sent to begin).
    pub offset: u32,
    pub total: u32,
    pub confirmed: bool,
    pub maintenance: bool,
    pub busy: bool,
    pub cancel_requested: bool,
    pub message: &'static str,
}

#[allow(
    clippy::large_enum_variant,
    reason = "One fixed 4-KiB queue slot; no allocator or borrowed HTTP buffers"
)]
pub enum Work {
    Begin {
        token: u32,
        manifest: [u8; 256],
    },
    Chunk {
        token: u32,
        offset: u32,
        bytes: heapless::Vec<u8, CHUNK_SIZE>,
    },
    Commit {
        token: u32,
    },
    Cancel {
        token: u32,
    },
}

pub struct Control {
    state: RefCell<Status>,
    serial: core::cell::Cell<u32>,
    pub work: Queue<Work, 1>,
}
impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}
impl Control {
    pub const fn new() -> Self {
        Self {
            state: RefCell::new(Status {
                build_id: crate::ota_manifest::BUILD_ID,
                phase: Phase::Checking,
                token: None,
                expires_ms: 0,
                offset: 0,
                total: 0,
                confirmed: false,
                maintenance: false,
                busy: false,
                cancel_requested: false,
                message: "checking signed boot assets",
            }),
            serial: core::cell::Cell::new(0),
            work: Queue::new(),
        }
    }
    pub fn status(&self) -> Status {
        *self.state.borrow()
    }
    pub fn set(&self, f: impl FnOnce(&mut Status)) {
        f(&mut self.state.borrow_mut());
    }
    pub fn fault(&self, message: &'static str) {
        self.set(|s| {
            s.phase = Phase::Fault;
            s.busy = false;
            s.message = message;
        });
    }
    /// Complete only after the owner has stopped physical work. An uncertain
    /// flash failure stays terminal even if cancellation was already queued.
    pub fn finish_cancel(&self) -> bool {
        let mut s = self.state.borrow_mut();
        s.busy = false;
        s.cancel_requested = false;
        if s.phase == Phase::Fault {
            return false;
        }
        s.phase = Phase::Cancelled;
        s.token = None;
        s.maintenance = false;
        s.message = "cancelled; inactive staging retained; host and proxy remain stopped";
        true
    }
    pub fn healthy(&self) {
        self.set(|s| {
            s.confirmed = true;
            s.phase = Phase::Idle;
            s.message = "ready for explicit update authorization";
        });
    }
    pub fn authorize(&self, now: u64) -> bool {
        let mut s = self.state.borrow_mut();
        if !s.confirmed
            || s.busy
            || !matches!(s.phase, Phase::Idle | Phase::Cancelled | Phase::Authorized)
        {
            return false;
        }
        let serial = self.serial.get().wrapping_add(1);
        self.serial.set(serial);
        s.token = Some(serial);
        s.expires_ms = now.saturating_add(AUTHORIZATION_MS);
        s.phase = Phase::Authorized;
        s.message = "trusted local update authorized for two minutes";
        true
    }
    pub fn admit(&self, work: Work, now: u64) -> bool {
        let mut s = self.state.borrow_mut();
        let cancelling = matches!(&work, Work::Cancel { .. });
        let committing = matches!(&work, Work::Commit { .. });
        if s.busy && !cancelling {
            return false;
        }
        let (token, allowed) = match &work {
            Work::Begin { token, .. } => {
                (*token, s.phase == Phase::Authorized && now < s.expires_ms)
            }
            Work::Chunk { token, .. } => {
                (*token, matches!(s.phase, Phase::Receiving | Phase::Ready))
            }
            Work::Commit { token } => (*token, s.phase == Phase::Ready),
            Work::Cancel { token } => (
                *token,
                matches!(
                    s.phase,
                    Phase::Authorized
                        | Phase::Quiescing
                        | Phase::Receiving
                        | Phase::Verifying
                        | Phase::Ready
                ),
            ),
        };
        if !allowed || s.token != Some(token) {
            return false;
        }
        if self.work.push_back(work).is_err() {
            return false;
        }
        s.busy = true;
        if committing {
            s.phase = Phase::Committing;
            s.message = "explicit commit accepted; cancellation is no longer available";
        }
        if cancelling {
            s.cancel_requested = true;
            s.message = "cancellation queued; current flash operation will finish first";
        }
        true
    }
}

pub fn csrf(origin: Option<&[u8]>, intent: Option<&[u8]>, content_type: Option<&[u8]>) -> bool {
    origin == Some(b"http://192.168.4.1")
        && intent == Some(b"1")
        && content_type == Some(b"application/octet-stream")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization_is_explicit_bounded_and_trial_locked() {
        let c = Control::new();
        assert!(!c.authorize(0));
        c.healthy();
        assert!(c.authorize(1));
        let old = c.status().token.unwrap();
        assert!(!c.admit(
            Work::Begin {
                token: old,
                manifest: [0; 256]
            },
            120001
        ));
        assert!(c.authorize(120001));
        assert!(!c.admit(
            Work::Begin {
                token: old,
                manifest: [0; 256]
            },
            120002
        ));
        let token = c.status().token.unwrap();
        assert!(c.admit(
            Work::Begin {
                token,
                manifest: [0; 256]
            },
            120002
        ));
        assert!(!c.authorize(120003));
        assert!(!c.admit(Work::Commit { token }, 120003));
    }
    #[test]
    fn cancellation_queues_behind_retained_flash_work_but_not_after_commit() {
        let c = Control::new();
        c.set(|s| {
            s.phase = Phase::Receiving;
            s.token = Some(7);
            s.confirmed = true;
        });
        assert!(c.admit(
            Work::Chunk {
                token: 7,
                offset: 256,
                bytes: heapless::Vec::from_slice(&[0x42; 4096]).unwrap()
            },
            0
        ));
        assert!(!c.admit(Work::Cancel { token: 7 }, 0)); // One queued work item.
        let Some(Work::Chunk { bytes, .. }) = c.work.pop_front() else {
            panic!("retained chunk")
        };
        assert!(bytes.iter().all(|b| *b == 0x42));
        assert!(c.status().busy); // Owner still completing the physical operation.
        assert!(c.admit(Work::Cancel { token: 7 }, 1));
        assert!(c.status().cancel_requested);
        assert!(matches!(
            c.work.pop_front(),
            Some(Work::Cancel { token: 7 })
        ));
        c.set(|s| {
            s.phase = Phase::Ready;
            s.busy = false;
            s.cancel_requested = false;
        });
        assert!(c.admit(Work::Commit { token: 7 }, 2));
        assert_eq!(c.status().phase, Phase::Committing);
        assert!(!c.admit(Work::Cancel { token: 7 }, 3));
    }

    #[test]
    fn cancellation_cannot_hide_uncertain_flash_failure_or_reauthorize_it() {
        let c = Control::new();
        c.set(|s| {
            s.maintenance = true;
            s.confirmed = true;
            s.token = Some(7);
            s.cancel_requested = true;
        });
        c.fault("uncertain program; no retry");
        assert!(!c.finish_cancel());
        assert_eq!(c.status().phase, Phase::Fault);
        assert!(c.status().maintenance);
        assert!(!c.status().cancel_requested);
        assert!(!c.authorize(0));
    }

    #[test]
    fn worst_case_status_fits_the_http_response_buffer() {
        let c = Control::new();
        assert_eq!(c.status().build_id, crate::ota_manifest::BUILD_ID);
        let mut json = [0; 512];
        let length = serde_json_core::to_slice(&c.status(), &mut json).unwrap();
        assert!(
            core::str::from_utf8(&json[..length])
                .unwrap()
                .contains(crate::ota_manifest::BUILD_ID)
        );
        c.set(|s| {
            s.build_id = "123456789012345678901234567890123456789012345678";
            s.phase = Phase::Committing;
            s.token = Some(u32::MAX);
            s.expires_ms = u64::MAX;
            s.offset = u32::MAX;
            s.total = u32::MAX;
            s.confirmed = true;
            s.maintenance = true;
            s.busy = true;
            s.cancel_requested = true;
            s.message = "cancelled; inactive staging retained; host and proxy remain stopped";
        });
        assert!(serde_json_core::to_slice(&c.status(), &mut [0; 512]).unwrap() < 512);
        assert!(core::mem::size_of::<Control>() < 8192);
    }

    #[test]
    fn csrf_requires_exact_origin_non_simple_header_and_binary_type() {
        assert!(csrf(
            Some(b"http://192.168.4.1"),
            Some(b"1"),
            Some(b"application/octet-stream")
        ));
        assert!(!csrf(None, Some(b"1"), Some(b"application/octet-stream")));
        assert!(!csrf(
            Some(b"http://evil.example"),
            Some(b"1"),
            Some(b"application/octet-stream")
        ));
        assert!(!csrf(
            Some(b"http://192.168.4.1"),
            None,
            Some(b"application/octet-stream")
        ));
    }
}
