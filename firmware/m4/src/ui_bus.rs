//! Core-local, independently bounded network and USB-C record delivery.
//!
//! Each sink retains 64 typed records; network also retains a 1024-byte JSON
//! assembly buffer and the IPC task retains one exact 256-byte-payload chunk.
//! Serial writes remain in their own task with a 250 ms deadline. Serial loss
//! never increments network loss. Full queues prefer semantics by evicting the
//! oldest raw record, otherwise reject the newcomer. No retry counts as loss.
use core::{cell::RefCell, mem::take};
use heapless::{String, Vec};
use shared::{
    protocol::{Event, Message, RECORD_BYTES},
    usb::types::*,
};
struct Queues {
    output: shared::ipc_delivery::Queue<UiRecord, 64>,
    network: shared::ipc_delivery::Queue<UiRecord, 64>,
    network_json: Vec<u8, { RECORD_BYTES }>,
    network_offset: usize,
    network_dropped: u32,
    network_loss_weight: u32,
    input: shared::ipc_delivery::Queue<HostCommand, 4>,
    connected: bool,
    replay: bool,
    generation: u32,
    dropped: u32,
}
fn raw_trace(record: &UiRecord) -> bool {
    matches!(
        &record.event,
        UiEvent::Host {
            event: UsbEvent::Transfer { .. } | UsbEvent::TransferContext { .. }
        }
    )
}

/// Returns the number of discarded records, whether evicted or not admitted.
fn queue_record(queue: &shared::ipc_delivery::Queue<UiRecord, 64>, record: UiRecord) -> u32 {
    if !queue.is_full() {
        let _ = queue.push_back(record);
        return 0;
    }
    if !raw_trace(&record) {
        let mut evicted = false;
        for _ in 0..queue.len() {
            let queued = queue
                .pop_front()
                .expect("queue length fixed during rotation");
            if !evicted && raw_trace(&queued) {
                evicted = true;
            } else {
                let _ = queue.push_back(queued);
            }
        }
        if evicted {
            let _ = queue.push_back(record);
        }
    }
    // A full all-semantic queue keeps its existing records and drops the new one.
    1
}

// All tasks run cooperatively on the M4 executor. No borrow crosses an await.
pub struct Bus(RefCell<Queues>, fn() -> u64);
#[cfg(target_os = "none")]
const _: () = assert!(core::mem::size_of::<Bus>() <= 24 * 1024);
impl Bus {
    pub const fn new(clock: fn() -> u64) -> Self {
        Self(
            RefCell::new(Queues {
                output: shared::ipc_delivery::Queue::new(),
                network: shared::ipc_delivery::Queue::new(),
                network_json: Vec::new(),
                network_offset: 0,
                network_dropped: 0,
                network_loss_weight: 0,
                input: shared::ipc_delivery::Queue::new(),
                connected: false,
                replay: false,
                generation: 0,
                dropped: 0,
            }),
            clock,
        )
    }
    pub fn emit(&self, timestamp_ms: u64, event: UiEvent) {
        let mut q = self.0.borrow_mut();

        let record = UiRecord {
            timestamp_ms,
            event: event.clone(),
        };
        let dropped = queue_record(&q.network, record);
        q.network_dropped = q.network_dropped.saturating_add(dropped);
        drop(q);
        self.emit_serial(timestamp_ms, event);
    }
    pub fn emit_serial(&self, timestamp_ms: u64, event: UiEvent) {
        let mut q = self.0.borrow_mut();
        if !q.connected {
            return;
        }
        let dropped = queue_record(
            &q.output,
            UiRecord {
                timestamp_ms,
                event,
            },
        );
        q.dropped = q.dropped.saturating_add(dropped);
    }
    pub fn error(&self, text: &str) {
        let mut message = String::new();
        let _ = message.push_str(text);
        self.emit((self.1)(), UiEvent::Error { message });
    }
    pub fn serial_failed(&self, record: UiRecord) {
        let lost = match record.event {
            UiEvent::Dropped { count } => count,
            _ => 1,
        };
        let mut q = self.0.borrow_mut();
        q.dropped = q.dropped.saturating_add(lost);
    }
    pub fn network_failed(&self) {
        let mut q = self.0.borrow_mut();
        q.network_json.clear();
        let lost = take(&mut q.network_loss_weight);
        q.network_dropped = q.network_dropped.saturating_add(lost);
    }
    /// Acknowledge only after Pending has accepted the exact chunk, not on Full.
    pub fn network_frame_sent(&self) {
        let mut q = self.0.borrow_mut();
        if q.network_json.is_empty() {
            q.network_loss_weight = 0;
        }
    }
    pub fn replay_serial(&self, processor: &crate::usb::processor::Processor) {
        processor.snapshot(|event| self.emit_serial((self.1)(), event));
    }
    /// IPC Status requests an M4 snapshot; it must not echo back to M7.
    pub fn accept_ipc_command(
        &self,
        command: HostCommand,
        processor: &crate::usb::processor::Processor,
    ) -> Result<(), HostCommand> {
        if command == HostCommand::Status {
            processor.snapshot(|event| self.emit((self.1)(), event));
            Ok(())
        } else {
            self.accept_command(command)
        }
    }
    pub fn command(&self) -> Option<HostCommand> {
        self.0.borrow().input.pop_front()
    }
    pub fn take_replay(&self) -> bool {
        take(&mut self.0.borrow_mut().replay)
    }
    pub fn accept_command(&self, command: HostCommand) -> Result<(), HostCommand> {
        self.0.borrow().input.push_back(command)
    }
    pub fn network_frame(&self) -> Option<Message> {
        let mut q = self.0.borrow_mut();
        if q.network_json.is_empty() {
            let record = if q.network_dropped != 0 {
                let count = take(&mut q.network_dropped);
                UiRecord {
                    timestamp_ms: (self.1)(),
                    event: UiEvent::Dropped { count },
                }
            } else {
                q.network.pop_front()?
            };
            q.network_loss_weight = match record.event {
                UiEvent::Dropped { count } => count,
                _ => 1,
            };
            q.network_json.resize(RECORD_BYTES, 0).ok()?;
            match serde_json_core::to_slice(&record, &mut q.network_json) {
                Ok(n) => q.network_json.truncate(n),
                Err(_) => {
                    q.network_json.clear();
                    let lost = take(&mut q.network_loss_weight);
                    q.network_dropped = q.network_dropped.saturating_add(lost);
                    return None;
                }
            }
            q.network_offset = 0;
        }
        let start = q.network_offset;
        let end = (start + 255).min(q.network_json.len());
        let mut bytes = Vec::<u8, 256>::new();
        let flags = u8::from(start == 0) | (u8::from(end == q.network_json.len()) << 1);
        let _ = bytes.push(flags);
        let _ = bytes.extend_from_slice(&q.network_json[start..end]);
        q.network_offset = end;
        if end == q.network_json.len() {
            q.network_json.clear();
        }
        Message::with_payload(Event::Ui, &bytes).ok()
    }
    pub fn connection(&self, connected: bool) {
        let mut q = self.0.borrow_mut();
        if q.connected != connected {
            q.connected = connected;
            q.generation = q.generation.wrapping_add(1);
            // DTR changes dispose of the old serial session's queued history.
            // Unexpected in-flight delivery failures are counted separately.
            q.output.clear();
            q.replay = connected;
        }
    }
    pub fn generation(&self) -> u32 {
        self.0.borrow().generation
    }
    pub fn next(&self) -> Option<UiRecord> {
        let mut q = self.0.borrow_mut();
        if q.dropped != 0 {
            let count = take(&mut q.dropped);
            return Some(UiRecord {
                timestamp_ms: (self.1)(),
                event: UiEvent::Dropped { count },
            });
        }
        q.output.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::ipc::SendError;
    use shared::ipc_delivery::Pending;

    fn raw(value: u8) -> UiEvent {
        UiEvent::Host {
            event: UsbEvent::Transfer {
                endpoint: 0x81,
                direction: Direction::In,
                bytes: 64,
                pid: 2,
                data: Vec::from_slice(&[value; 64]).unwrap(),
            },
        }
    }
    fn record(sequence: u64, event: UiEvent) -> Vec<u8, RECORD_BYTES> {
        let mut bytes = Vec::new();
        bytes.resize(RECORD_BYTES, 0).unwrap();
        let len = serde_json_core::to_slice(
            &UiRecord {
                timestamp_ms: sequence,
                event,
            },
            &mut bytes,
        )
        .unwrap();
        bytes.truncate(len);
        bytes
    }
    fn next_record(bus: &Bus) -> Vec<u8, RECORD_BYTES> {
        let mut bytes = Vec::<u8, RECORD_BYTES>::new();
        let mut started = false;
        loop {
            let frame = bus.network_frame().expect("record queued");
            bus.network_frame_sent();
            let payload = frame.value.unwrap();
            assert_eq!(payload[0] & 1 != 0, !started);
            started = true;
            bytes.extend_from_slice(&payload[1..]).unwrap();
            if payload[0] & 2 != 0 {
                break;
            }
        }
        bytes
    }

    #[test]
    fn unavailable_serial_does_not_hold_network_or_mix_loss_counters() {
        let bus = Bus::new(|| 0);
        bus.connection(true);
        // A connected CDC client that never reads: its queue fills while the
        // independent network consumer continues to receive exact records.
        for sequence in 0..200 {
            bus.emit(sequence, raw(255));
            assert_eq!(next_record(&bus), record(sequence, raw(255)));
        }
        assert_eq!(bus.0.borrow().network_dropped, 0);
        assert_eq!(bus.0.borrow().network.len(), 0);
        assert_eq!(bus.0.borrow().output.len(), 64);
        assert_eq!(bus.next().unwrap().event, UiEvent::Dropped { count: 136 });
        bus.connection(false);
        bus.emit(201, raw(17));
        assert_eq!(next_record(&bus), record(201, raw(17)));
    }

    #[test]
    fn multichunk_retry_keeps_exact_bytes_and_no_duplicate_record() {
        let bus = Bus::new(|| 0);
        bus.emit(1, raw(255));
        bus.emit(2, raw(254));
        let mut pending = Pending::new();
        let first = bus.network_frame().unwrap();
        assert_eq!(first.value.as_ref().unwrap()[0], 1); // not final
        pending.set(Some(first.clone()));
        for _ in 0..1000 {
            pending
                .retry(|frame| {
                    assert_eq!(frame, &first);
                    Err(SendError::Full)
                })
                .unwrap();
        }
        let mut assembly = Vec::<u8, RECORD_BYTES>::new();
        pending
            .retry(|frame| {
                assembly
                    .extend_from_slice(&frame.value.as_ref().unwrap()[1..])
                    .unwrap();
                Ok(())
            })
            .unwrap();
        bus.network_frame_sent();
        loop {
            let frame = bus.network_frame().unwrap();
            bus.network_frame_sent();
            let payload = frame.value.unwrap();
            assert_eq!(payload[0] & 1, 0);
            assembly.extend_from_slice(&payload[1..]).unwrap();
            if payload[0] & 2 != 0 {
                break;
            }
        }
        assert_eq!(assembly, record(1, raw(255)));
        assert_eq!(next_record(&bus), record(2, raw(254)));
        assert!(bus.network_frame().is_none());
    }

    #[test]
    fn priority_eviction_preserves_survivor_order_and_counts_only_real_loss() {
        let bus = Bus::new(|| 0);
        bus.emit(
            0,
            UiEvent::Hello {
                protocol_version: 1,
            },
        );
        for sequence in 1..64 {
            bus.emit(sequence, raw(0));
        }
        bus.emit(
            64,
            UiEvent::Hello {
                protocol_version: 2,
            },
        );
        assert_eq!(bus.0.borrow().network_dropped, 1);
        assert_eq!(next_record(&bus), record(0, UiEvent::Dropped { count: 1 }));
        assert_eq!(
            next_record(&bus),
            record(
                0,
                UiEvent::Hello {
                    protocol_version: 1
                }
            )
        );
        for sequence in 2..64 {
            assert_eq!(next_record(&bus), record(sequence, raw(0)));
        }
        assert_eq!(
            next_record(&bus),
            record(
                64,
                UiEvent::Hello {
                    protocol_version: 2
                }
            )
        );
        assert!(bus.network_frame().is_none());
        for sequence in 0..64 {
            bus.emit(
                sequence,
                UiEvent::Hello {
                    protocol_version: 1,
                },
            );
        }
        bus.emit(
            64,
            UiEvent::Hello {
                protocol_version: 2,
            },
        );
        assert_eq!(bus.0.borrow().network_dropped, 1);
        assert_eq!(bus.0.borrow().network.len(), 64);
    }

    #[test]
    fn command_pressure_retains_caller_owned_stop_once() {
        let bus = Bus::new(|| 0);
        for _ in 0..4 {
            bus.accept_command(HostCommand::Status).unwrap();
        }
        let stop = bus.accept_command(HostCommand::Stop).unwrap_err();
        assert_eq!(bus.command(), Some(HostCommand::Status));
        bus.accept_command(stop).unwrap();
        for _ in 0..3 {
            assert_eq!(bus.command(), Some(HostCommand::Status));
        }
        assert_eq!(bus.command(), Some(HostCommand::Stop));
        assert!(bus.command().is_none());
    }

    #[test]
    fn terminal_failure_at_each_chunk_restores_exact_loss_weight() {
        for failed_chunk in 0..3 {
            let bus = Bus::new(|| 0);
            // Trailing JSON whitespace exercises three chunks without changing
            // the record, including a genuinely intermediate chunk.
            {
                let mut q = bus.0.borrow_mut();
                q.network_json = record(1, raw(255));
                q.network_json.resize(700, b' ').unwrap();
                q.network_loss_weight = 1;
            }
            bus.emit(2, raw(254));
            for chunk in 0..=failed_chunk {
                let frame = bus.network_frame().unwrap();
                let mut pending = Pending::new();
                pending.set(Some(frame.clone()));
                for _ in 0..10 {
                    pending
                        .retry(|retry| {
                            assert_eq!(retry, &frame);
                            Err(SendError::Full)
                        })
                        .unwrap();
                    assert!(!pending.is_none());
                    assert_eq!(bus.0.borrow().network_loss_weight, 1);
                    assert_eq!(bus.0.borrow().network_dropped, 0);
                }
                if chunk == failed_chunk {
                    assert!(
                        pending
                            .retry(|_| Err(SendError::Transport(
                                shared::ipc::QueueError::StaleEndpoint
                            )))
                            .is_err()
                    );
                    assert!(pending.is_none());
                    bus.network_failed();
                } else {
                    pending.retry(|_| Ok(())).unwrap();
                    bus.network_frame_sent();
                }
            }
            assert_eq!(next_record(&bus), record(0, UiEvent::Dropped { count: 1 }));
            assert_eq!(next_record(&bus), record(2, raw(254)));
            assert!(bus.network_frame().is_none());
        }
    }

    #[test]
    fn failed_drop_marker_preserves_original_count_and_new_losses() {
        let bus = Bus::new(|| 0);
        bus.0.borrow_mut().network_dropped = 37;
        let frame = bus.network_frame().unwrap();
        assert_eq!(frame.value.as_ref().unwrap()[0], 3);
        let mut pending = Pending::new();
        pending.set(Some(frame.clone()));
        for _ in 0..10 {
            pending
                .retry(|retry| {
                    assert_eq!(retry, &frame);
                    Err(SendError::Full)
                })
                .unwrap();
        }
        bus.0.borrow_mut().network_dropped = 2;
        assert!(
            pending
                .retry(|_| Err(SendError::Transport(shared::ipc::QueueError::StaleEndpoint)))
                .is_err()
        );
        assert!(pending.is_none());
        bus.network_failed();
        assert_eq!(next_record(&bus), record(0, UiEvent::Dropped { count: 39 }));
        assert!(bus.network_frame().is_none());
        assert_eq!(bus.0.borrow().network_loss_weight, 0);
    }

    #[test]
    fn origin_routing_keeps_replay_serial_and_ipc_status_local() {
        let bus = Bus::new(|| 0);
        let processor = crate::usb::processor::Processor::new();
        bus.connection(true);
        bus.replay_serial(&processor);
        assert!(bus.next().is_some());
        assert!(bus.network_frame().is_none());
        bus.accept_command(HostCommand::Stop).unwrap();
        bus.accept_ipc_command(HostCommand::Status, &processor)
            .unwrap();
        bus.accept_command(HostCommand::Status).unwrap();
        bus.accept_ipc_command(HostCommand::Start, &processor)
            .unwrap();
        assert!(bus.network_frame().is_some());
        assert_eq!(bus.command(), Some(HostCommand::Stop));
        assert_eq!(bus.command(), Some(HostCommand::Status));
        assert_eq!(bus.command(), Some(HostCommand::Start));
        assert!(bus.command().is_none());
    }

    #[test]
    fn serial_abandonment_restores_marker_without_network_loss() {
        let bus = Bus::new(|| 0);
        bus.serial_failed(UiRecord {
            timestamp_ms: 0,
            event: UiEvent::Dropped { count: 17 },
        });
        bus.serial_failed(UiRecord {
            timestamp_ms: 0,
            event: raw(1),
        });
        assert_eq!(bus.next().unwrap().event, UiEvent::Dropped { count: 18 });
        assert!(bus.network_frame().is_none());
    }

    #[test]
    fn queue_memory_is_bounded() {
        assert!(core::mem::size_of::<Bus>() <= 64 * 1024);
    }
}
