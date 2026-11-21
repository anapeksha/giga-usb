//! Request and page bookkeeping shared by the HTTP stream and native tests.
use crate::trace_query::{TraceDirection, TraceKind, TraceQuery};
use heapless::String;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamQuery {
    pub client: Option<String<32>>,
    pub attempt: Option<String<32>>,
    pub resume: Option<String<32>>,
    #[serde(default)]
    pub after: u32,
    #[serde(default)]
    pub trace_after: u32,
    #[serde(default)]
    pub trace: bool,
    pub endpoint: Option<u8>,
    pub direction: Option<TraceDirection>,
    pub kind: Option<TraceKind>,
    #[serde(default)]
    pub errors: bool,
    pub q: Option<String<96>>,
}

impl StreamQuery {
    pub fn reset_unless_resumed(&mut self, resumed: bool) {
        if !resumed {
            self.after = 0;
            self.trace_after = 0;
        }
    }

    pub fn trace_query(&mut self) -> TraceQuery {
        TraceQuery {
            after: self.trace_after,
            endpoint: self.endpoint,
            direction: self.direction,
            kind: self.kind,
            errors: self.errors,
            q: self.q.take(),
        }
    }
}

#[derive(Serialize)]
pub struct Reset<'a> {
    pub resume: Option<&'a str>,
}

pub const DRAIN_BATCH: usize = 8;

/// One turn always visits both cursors before the HTTP task can yield or wait.
pub struct StreamPages {
    after: u32,
    trace: Option<TraceQuery>,
    initial: bool,
    event_loss: u32,
    trace_loss: u32,
}

pub struct Pages {
    pub events: Option<String<4096>>,
    pub trace: Option<String<4096>>,
    pub pending: bool,
}

impl StreamPages {
    pub fn new(mut query: StreamQuery) -> Self {
        let trace = query.trace.then(|| query.trace_query());
        Self {
            after: query.after,
            trace,
            initial: true,
            event_loss: 0,
            trace_loss: 0,
        }
    }

    pub fn next(&mut self, bus: &crate::network_bus::Bus) -> Pages {
        let events = bus.events(self.after);
        let next = position(&events);
        let mut pending = next.cursor != self.after;
        let send_events = self.initial || pending || next.dropped != self.event_loss;
        self.after = next.cursor;
        self.event_loss = next.dropped;
        let trace = self.trace.as_mut().and_then(|query| {
            let page = bus.trace(query);
            let next = position(&page);
            let progressed = next.cursor != query.after;
            let send = self.initial || progressed || next.dropped != self.trace_loss;
            pending |= progressed;
            query.after = next.cursor;
            self.trace_loss = next.dropped;
            send.then_some(page)
        });
        self.initial = false;
        Pages {
            events: send_events.then_some(events),
            trace,
            pending,
        }
    }
}

#[derive(Deserialize)]
pub struct PagePosition {
    pub cursor: u32,
    pub dropped: u32,
}

pub fn position(page: &str) -> PagePosition {
    // Unknown records are skipped by serde-json-core; the bus emits this envelope.
    serde_json_core::from_str::<PagePosition>(page)
        .expect("bus page envelope")
        .0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_independent_trace_cursor() {
        let (mut query, _) = serde_json_core::from_str::<StreamQuery>(
            r#"{"after":42,"trace_after":7,"trace":true,"endpoint":128,"errors":true,"q":"abc"}"#,
        )
        .unwrap();
        let trace = query.trace_query();
        assert_eq!(query.after, 42);
        assert_eq!(trace.after, 7);
        assert_eq!(trace.endpoint, Some(128));
        assert!(trace.errors);
        assert_eq!(trace.q.unwrap().as_str(), "abc");
        let (query, _) = serde_json_core::from_str::<StreamQuery>("{}").unwrap();
        assert_eq!(query.after, 0);
        assert_eq!(query.trace_after, 0);
        assert!(!query.trace);
    }

    #[test]
    fn client_tokens_are_optional_and_bounded() {
        let (query, _) = serde_json_core::from_str::<StreamQuery>(
            r#"{"client":"0123456789abcdef0123456789abcdef"}"#,
        )
        .unwrap();
        assert_eq!(query.client.unwrap().len(), 32);
        assert!(
            serde_json_core::from_str::<StreamQuery>(
                r#"{"client":"0123456789abcdef0123456789abcdef0"}"#,
            )
            .is_err()
        );
        for invalid in [
            r#"{"attempt":"0123456789abcdef0123456789abcdef0"}"#,
            r#"{"resume":"0123456789abcdef0123456789abcdef0"}"#,
        ] {
            assert!(serde_json_core::from_str::<StreamQuery>(invalid).is_err());
        }
        let (proofs, _) = serde_json_core::from_str::<StreamQuery>(
            r#"{"attempt":"0123456789abcdef0123456789abcdef","resume":"fedcba9876543210fedcba9876543210"}"#,
        ).unwrap();
        assert_eq!(proofs.attempt.unwrap().len(), 32);
        assert_eq!(proofs.resume.unwrap().len(), 32);
        assert!(
            serde_json_core::from_str::<StreamQuery>("{}")
                .unwrap()
                .0
                .client
                .is_none()
        );
    }

    #[test]
    fn reset_payload_serializes_proof_and_legacy_null_safely() {
        let mut bytes = [0; 256];
        let len = serde_json_core::to_slice(&Reset { resume: None }, &mut bytes).unwrap();
        assert_eq!(&bytes[..len], br#"{"resume":null}"#);
        let len = serde_json_core::to_slice(
            &Reset {
                resume: Some("proof\"\\\n"),
            },
            &mut bytes,
        )
        .unwrap();
        assert_eq!(&bytes[..len], br#"{"resume":"proof\"\\\n"}"#);
    }

    #[test]
    fn forgotten_tokens_and_a_new_bus_reset_both_cursors_but_resumes_preserve_them() {
        use crate::{
            network_bus::Bus,
            trace_query::tests::{send, transfer},
        };
        use shared::usb::types::UiEvent;
        let bus = Bus::new();
        let rebooted = Bus::new();
        for current in [&bus, &rebooted] {
            for _ in 0..3 {
                send(current, UiEvent::Host { event: transfer(0) });
            }
        }
        for (current, expected_resumed) in [(&bus, false), (&bus, true), (&rebooted, false)] {
            let client = current
                .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
                .unwrap();
            assert_eq!(client.resumed, expected_resumed);
            let mut query = StreamQuery {
                after: 1,
                trace_after: 2,
                trace: true,
                ..Default::default()
            };
            query.reset_unless_resumed(client.resumed);
            let expected_after = if expected_resumed { 1 } else { 0 };
            let expected_trace_after = if expected_resumed { 2 } else { 0 };
            assert_eq!(query.after, expected_after);
            assert_eq!(query.trace_after, expected_trace_after);
            let expected_events = current.events(expected_after);
            let expected_trace = current.trace(&TraceQuery {
                after: expected_trace_after,
                ..Default::default()
            });
            let initial = StreamPages::new(query).next(current);
            assert_eq!(initial.events.unwrap(), expected_events);
            assert_eq!(initial.trace.unwrap(), expected_trace);
        }
        // Replacing an inactive token also forces both cursors back to history start.
        drop(
            bus.subscribe_client(Some("client-b"), Some("attempt-b"), Some("attempt-b"))
                .unwrap(),
        );
        let evicted = bus
            .subscribe_client(Some("client-a"), Some("attempt-a"), Some("attempt-a"))
            .unwrap();
        assert!(!evicted.resumed);
        let mut query = StreamQuery {
            after: 1,
            trace_after: 2,
            ..Default::default()
        };
        query.reset_unless_resumed(evicted.resumed);
        assert_eq!((query.after, query.trace_after), (0, 0));
        drop(evicted);
        // Anonymous clients ignore supplied cursors even when they appear valid.
        let anonymous = bus.subscribe().unwrap();
        let mut query = StreamQuery {
            after: 1,
            trace_after: 2,
            ..Default::default()
        };
        query.reset_unless_resumed(anonymous.resumed);
        assert_eq!((query.after, query.trace_after), (0, 0));
    }

    #[test]
    fn initial_empty_pages_are_sent_once_and_trace_is_opt_in() {
        let bus = crate::network_bus::Bus::new();
        for trace in [false, true] {
            let mut pages = StreamPages::new(StreamQuery {
                trace,
                ..Default::default()
            });
            let first = pages.next(&bus);
            assert_eq!(first.events.unwrap(), bus.events(0));
            assert_eq!(first.trace.is_some(), trace);
            assert!(!first.pending);
            let idle = pages.next(&bus);
            assert!(idle.events.is_none());
            assert!(idle.trace.is_none());
            assert!(!idle.pending);
        }
    }

    #[test]
    fn busy_events_and_trace_both_advance_within_each_bounded_batch() {
        use crate::trace_query::tests::{send, transfer};
        use shared::usb::types::UiEvent;
        let bus = crate::network_bus::Bus::new();
        for _ in 0..40 {
            send(&bus, UiEvent::Host { event: transfer(0) });
        }
        let mut pages = StreamPages::new(StreamQuery {
            trace: true,
            ..Default::default()
        });
        for turn in 1..=DRAIN_BATCH {
            let next = pages.next(&bus);
            assert_eq!(position(&next.events.unwrap()).cursor, (turn * 3) as u32);
            assert_eq!(position(&next.trace.unwrap()).cursor, (turn * 3) as u32);
            assert!(next.pending);
        }
        // The HTTP task yields here, rather than draining all retained history.
        let next = pages.next(&bus);
        assert_eq!(position(&next.events.unwrap()).cursor, 27);
        assert_eq!(position(&next.trace.unwrap()).cursor, 27);
    }

    #[test]
    fn deferred_trace_waits_without_busy_looping_then_resumes_on_publication() {
        use crate::trace_query::tests::{context, send, transfer};
        use shared::usb::types::UiEvent;
        let bus = crate::network_bus::Bus::new();
        let mut pages = StreamPages::new(StreamQuery {
            trace: true,
            q: Some(String::try_from("get descriptor").unwrap()),
            ..Default::default()
        });
        send(&bus, UiEvent::Host { event: context(0) });
        let first = pages.next(&bus);
        assert_eq!(position(&first.events.unwrap()).cursor, 1);
        assert_eq!(position(&first.trace.unwrap()).cursor, 0);
        let waiting = pages.next(&bus);
        assert!(waiting.events.is_none());
        assert!(waiting.trace.is_none());
        assert!(!waiting.pending);
        send(&bus, UiEvent::Host { event: transfer(0) });
        let resumed = pages.next(&bus);
        assert_eq!(position(&resumed.events.unwrap()).cursor, 2);
        assert_eq!(position(&resumed.trace.unwrap()).cursor, 2);
        assert!(resumed.pending);
    }

    #[test]
    fn page_position_skips_records_and_preserves_wrap_loss() {
        let position =
            position(r#"{"records":[{"nested":[1,2,3]}],"cursor":4294967295,"dropped":9}"#);
        assert_eq!(position.cursor, u32::MAX);
        assert_eq!(position.dropped, 9);
    }
}
