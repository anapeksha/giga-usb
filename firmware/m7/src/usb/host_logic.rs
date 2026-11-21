#![forbid(unsafe_code)]

//! Pure host descriptor selection and channel register encoding.

use super::types::{CapturedEvent, UsbError, UsbEvent};
use shared::protocol::{Event, Message};

fn raw_trace(message: &Message) -> bool {
    message.event == Event::Usb
        && message.decode::<CapturedEvent>().is_ok_and(|capture| {
            matches!(
                capture.event,
                UsbEvent::Transfer { .. } | UsbEvent::TransferContext { .. }
            )
        })
}

/// Preserve semantic/command ordering, sacrificing only the oldest raw trace.
/// Rejections do not count as losses until the caller abandons publication.
pub(super) fn queue_capture<const N: usize>(
    pending: &shared::ipc_delivery::Queue<Message, N>,
    message: Message,
    dropped: &mut u32,
) -> Result<(), ()> {
    if pending.is_full() && !raw_trace(&message) {
        let mut evicted = false;
        for _ in 0..pending.len() {
            let queued = pending
                .pop_front()
                .expect("queue length is unchanged while rotating");
            if !evicted && raw_trace(&queued) {
                evicted = true;
                *dropped = dropped.saturating_add(1);
            } else {
                let _ = pending.push_back(queued);
            }
        }
    }
    pending.push_back(message).map_err(|_| ())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BootEndpoint {
    pub(super) interface: u8,
    pub(super) endpoint: u8,
    pub(super) packet: u16,
    pub(super) interval: u8,
    pub(super) boot: bool,
    pub(super) report_length: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StorageEndpoint {
    pub(super) interface: u8,
    pub(super) in_endpoint: u8,
    pub(super) out_endpoint: u8,
    pub(super) in_mps: u16,
    pub(super) out_mps: u16,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct SelectedConfig {
    pub(super) configuration: u8,
    pub(super) hid: heapless::Vec<BootEndpoint, 4>,
    pub(super) storage: Option<StorageEndpoint>,
}

/// Private channel programming parameters, not a capture/wire contract.
#[derive(Clone, Copy)]
pub(super) struct Packet {
    pub(super) endpoint: u8,
    pub(super) kind: u32,
    pub(super) pid: u32,
    pub(super) mps: u16,
}

impl Packet {
    pub(super) fn control(endpoint: u8, pid: u32, mps: u16) -> Self {
        Self {
            endpoint,
            kind: 0,
            pid,
            mps,
        }
    }

    pub(super) fn transfer_size(self, out_length: usize) -> u32 {
        // IN always reserves one MPS in the controller, even for status ZLPs.
        // OUT ZLPs have XFRSIZ=0 but still require PKTCNT=1 and DATA1.
        let size = if self.endpoint & 0x80 != 0 {
            u32::from(self.mps)
        } else {
            out_length as u32
        };
        size | (1 << 19) | (self.pid << 29)
    }
}

/// Poll raw HCINT without mistaking the normal completion halt for an error.
/// RXFLVL may become visible after the caller's last FIFO drain. Keep polling
/// on XFRC until all receive statuses (including the payload) have been popped.
pub(super) fn channel_outcome(flags: u32, rx_pending: bool) -> Option<Result<(), UsbError>> {
    if flags & (1 << 3) != 0 {
        Some(Err(UsbError::Stall))
    } else if flags & (1 << 8) != 0 {
        Some(Err(UsbError::Babble))
    } else if flags & ((1 << 2) | (1 << 7) | (1 << 9) | (1 << 10)) != 0 {
        Some(Err(UsbError::Transaction))
    } else if flags & 1 != 0 {
        (!rx_pending).then_some(Ok(()))
    } else if flags & (1 << 4) != 0 {
        Some(Err(UsbError::Nak))
    } else if flags & 2 != 0 {
        Some(Err(UsbError::Transaction))
    } else {
        None
    }
}

/// This driver supports only a directly attached root device, never a hub.
/// HCCHAR.LSDEV is for a low-speed target behind a faster root port; the root
/// port's HCFG clock selection already handles a directly attached LS device.
pub(super) fn root_channel_characteristics(packet: Packet, address: u8, frame: u32) -> u32 {
    u32::from(packet.mps)
        | (u32::from(packet.endpoint & 15) << 11)
        | if packet.endpoint & 0x80 != 0 {
            1 << 15
        } else {
            0
        }
        | (packet.kind << 18)
        | (1 << 20)
        | (u32::from(address) << 22)
        | if packet.kind == 3 {
            ((frame & 1) ^ 1) << 29
        } else {
            0
        }
}

/// Validate the chain and select alternate zero's HID and read-only BOT transports.
/// Each HID interface contributes at most one interrupt-IN endpoint.
pub(super) fn select_configuration(
    bytes: &[u8],
    low_speed: bool,
) -> Result<SelectedConfig, UsbError> {
    if bytes.len() < 9
        || bytes[0] != 9
        || bytes[1] != 2
        || usize::from(u16::from_le_bytes([bytes[2], bytes[3]])) != bytes.len()
        || bytes[5] == 0
    {
        return Err(UsbError::InvalidDescriptor);
    }
    let mut offset = 0;
    let mut interface = None;
    let mut selected = heapless::Vec::<BootEndpoint, 4>::new();
    let mut boot = false;
    let mut report_length = 0;
    let mut storage_interface = None;
    let mut storage = None;
    while offset < bytes.len() {
        if bytes.len() - offset < 2 {
            return Err(UsbError::InvalidDescriptor);
        }
        let len = usize::from(bytes[offset]);
        if len < 2 || offset + len > bytes.len() {
            return Err(UsbError::InvalidDescriptor);
        }
        let d = &bytes[offset..offset + len];
        match d[1] {
            4 => {
                if len < 9 {
                    return Err(UsbError::InvalidDescriptor);
                }
                interface = (d[3] == 0 && d[5] == 3).then_some(d[2]);
                boot = d[6] == 1 && matches!(d[7], 1 | 2);
                report_length = 0;
                storage_interface =
                    (d[3] == 0 && d[5] == 8 && d[6] == 6 && d[7] == 0x50 && !low_speed)
                        .then_some(d[2]);
            }
            0x21 if interface.is_some() => {
                if len < 6 || len < 6 + usize::from(d[5]) * 3 {
                    return Err(UsbError::InvalidDescriptor);
                }
                for entry in d[6..6 + usize::from(d[5]) * 3].as_chunks::<3>().0 {
                    if entry[0] == 0x22 {
                        report_length = u16::from_le_bytes([entry[1], entry[2]]);
                        if report_length == 0 || report_length > 1024 {
                            return Err(UsbError::InvalidDescriptor);
                        }
                    }
                }
            }
            5 => {
                if len < 7 {
                    return Err(UsbError::InvalidDescriptor);
                }
                let packet = u16::from_le_bytes([d[4], d[5]]);
                if let Some(number) = storage_interface
                    && d[3] & 3 == 2
                {
                    if d[2] & 0x70 != 0 || d[2] & 15 == 0 || packet == 0 || packet > 64 {
                        return Err(UsbError::InvalidDescriptor);
                    }
                    let candidate = storage.get_or_insert(StorageEndpoint {
                        interface: number,
                        in_endpoint: 0,
                        out_endpoint: 0,
                        in_mps: 0,
                        out_mps: 0,
                    });
                    if candidate.interface == number {
                        if d[2] & 0x80 != 0 && candidate.in_endpoint == 0 {
                            candidate.in_endpoint = d[2];
                            candidate.in_mps = packet;
                        } else if d[2] & 0x80 == 0 && candidate.out_endpoint == 0 {
                            candidate.out_endpoint = d[2];
                            candidate.out_mps = packet;
                        }
                    }
                }
                if let Some(number) = interface
                    && !selected.iter().any(|ep| ep.interface == number)
                    && d[2] & 0x80 != 0
                    && d[3] & 3 == 3
                {
                    if d[2] & 0x70 != 0
                        || d[2] & 15 == 0
                        || packet == 0
                        || packet > if low_speed { 8 } else { 64 }
                        || d[6] == 0
                        || (low_speed && d[6] < 10)
                    {
                        return Err(UsbError::InvalidDescriptor);
                    }
                    if !boot && report_length == 0 {
                        return Err(UsbError::InvalidDescriptor);
                    }
                    selected
                        .push(BootEndpoint {
                            interface: number,
                            endpoint: d[2],
                            packet,
                            interval: d[6],
                            boot,
                            report_length,
                        })
                        .map_err(|_| UsbError::Overflow)?;
                }
            }
            _ => {}
        }
        offset += len;
    }
    Ok(SelectedConfig {
        configuration: bytes[5],
        hid: selected,
        storage: storage.filter(|ep| ep.in_endpoint != 0 && ep.out_endpoint != 0),
    })
}

#[cfg(test)]
fn boot_endpoint(bytes: &[u8], low_speed: bool) -> Result<(u8, Option<BootEndpoint>), UsbError> {
    let selected = select_configuration(bytes, low_speed)?;
    Ok((
        selected.configuration,
        selected.hid.into_iter().find(|ep| ep.boot),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(timestamp_us: u64, event: UsbEvent) -> Message {
        Message::encode(
            Event::Usb,
            &CapturedEvent {
                timestamp_us,
                event,
            },
        )
        .unwrap()
    }

    fn trace(timestamp_us: u64) -> Message {
        captured(
            timestamp_us,
            UsbEvent::Transfer {
                endpoint: 0x81,
                direction: super::super::types::Direction::In,
                bytes: 0,
                pid: 0,
                data: heapless::Vec::new(),
            },
        )
    }

    #[test]
    fn boundary_backpressure_drains_burst_larger_than_both_old_and_new_queue() {
        use crate::ipc_delivery::{Delivery, drain_deadline, should_drain};
        use shared::{ipc::SendError, ipc_delivery::Pending};
        let delivery = Delivery::new();
        let mut pending = Pending::new();
        let mut produced = 0;
        let mut delivered = 0;
        let mut dropped = 0;
        let mut waiting_until = None;
        let mut waited = false;
        for now in (0..25_000).step_by(25) {
            // Independent delivery task keeps retrying while the producer is
            // throttled at a safe boundary. The peer recovers after saturation.
            if pending.is_none() {
                pending.set(delivery.captures.pop_front());
            }
            pending
                .retry(|message| {
                    if now < 2_500 {
                        return Err(SendError::Full);
                    }
                    assert_eq!(message, &trace(delivered));
                    delivered += 1;
                    Ok(())
                })
                .unwrap();
            if produced < 96 {
                if delivery.captures.len() > 60 {
                    waited = true;
                    let until = *waiting_until.get_or_insert_with(|| drain_deadline(now, 25_000));
                    assert!(
                        should_drain(delivery.captures.len(), now, until),
                        "consumer must recover within the bounded wait in this scenario"
                    );
                } else {
                    waiting_until = None;
                    queue_capture(&delivery.captures, trace(produced), &mut dropped).unwrap();
                    produced += 1;
                }
            }
            if delivered == 96 {
                break;
            }
        }
        assert!(waited);
        assert_eq!(produced, 96);
        assert_eq!(delivered, 96);
        assert_eq!(dropped, 0);
        assert!(delivery.captures.is_empty());
        assert!(pending.is_none());
    }

    #[test]
    fn critical_evicts_oldest_trace_without_reordering_survivors() {
        let queue = shared::ipc_delivery::Queue::<Message, 5>::new();
        let mut dropped = 0;
        let first = captured(
            0,
            UsbEvent::Error {
                code: UsbError::Timeout,
            },
        );
        let context = captured(
            1,
            UsbEvent::TransferContext {
                transfer_id: 1,
                session_id: 1,
                endpoint: 0x81,
                kind: super::super::types::TransferKind::Interrupt,
                stage: super::super::types::TransferStage::Data,
                elapsed_us: 1,
                retries: 0,
            },
        );
        let packet = trace(2);
        let command =
            Message::encode(Event::Command, &super::super::types::HostCommand::Status).unwrap();
        let last = captured(
            4,
            UsbEvent::HidReport {
                endpoint: 0x81,
                data: heapless::Vec::new(),
            },
        );
        for message in [
            first.clone(),
            context,
            packet.clone(),
            command.clone(),
            last.clone(),
        ] {
            queue_capture(&queue, message, &mut dropped).unwrap();
        }
        let incoming = captured(
            5,
            UsbEvent::Configured {
                address: 1,
                configuration: 1,
            },
        );
        queue_capture(&queue, incoming.clone(), &mut dropped).unwrap();
        assert_eq!(dropped, 1);
        for expected in [first, packet, command, last, incoming] {
            assert_eq!(queue.pop_front(), Some(expected));
        }
    }

    #[test]
    fn raw_rejection_and_critical_retry_do_not_evict_semantics_or_count_retries() {
        let queue = shared::ipc_delivery::Queue::<Message, 2>::new();
        let mut dropped = u32::MAX - 1;
        let critical = captured(
            1,
            UsbEvent::Configured {
                address: 1,
                configuration: 1,
            },
        );
        queue_capture(&queue, trace(0), &mut dropped).unwrap();
        queue_capture(&queue, critical.clone(), &mut dropped).unwrap();
        let rejected = trace(2);
        assert_eq!(
            queue_capture(&queue, rejected.clone(), &mut dropped),
            Err(())
        );
        // Non-retrying callers count this rejected record once, not each attempt.
        dropped = dropped.saturating_add(1);
        let hid = captured(
            3,
            UsbEvent::HidReport {
                endpoint: 0x81,
                data: heapless::Vec::new(),
            },
        );
        queue_capture(&queue, hid.clone(), &mut dropped).unwrap();
        assert_eq!(dropped, u32::MAX);
        let next = captured(
            4,
            UsbEvent::Error {
                code: UsbError::Overflow,
            },
        );
        assert_eq!(queue_capture(&queue, next.clone(), &mut dropped), Err(()));
        assert_eq!(queue.pop_front(), Some(critical));
        queue_capture(&queue, next.clone(), &mut dropped).unwrap();
        assert_eq!(queue.pop_front(), Some(hid));
        assert_eq!(queue.pop_front(), Some(next));
        assert_eq!(dropped, u32::MAX);
    }

    #[test]
    fn descriptor_burst_survives_trace_saturation_and_all_critical_backpressure() {
        let queue = shared::ipc_delivery::Queue::<Message, 32>::new();
        let mut dropped = 0;
        for i in 0..32 {
            queue_capture(&queue, trace(i), &mut dropped).unwrap();
        }
        let mut expected = heapless::Vec::<Message, 34>::new();
        for interface in 0..4 {
            for chunk in 0..8 {
                expected
                    .push(captured(
                        32 + expected.len() as u64,
                        UsbEvent::Descriptor {
                            descriptor_type: 0x22,
                            index: interface,
                            offset: chunk * 128,
                            total_length: 1024,
                            data: heapless::Vec::from_slice(&[42; 128]).unwrap(),
                        },
                    ))
                    .unwrap();
            }
        }
        expected
            .push(captured(
                64,
                UsbEvent::Configured {
                    address: 1,
                    configuration: 1,
                },
            ))
            .unwrap();
        expected
            .push(captured(
                65,
                UsbEvent::HidReport {
                    endpoint: 0x81,
                    data: heapless::Vec::new(),
                },
            ))
            .unwrap();
        let mut delivered = heapless::Vec::<Message, 34>::new();
        for message in &expected {
            if queue_capture(&queue, message.clone(), &mut dropped).is_err() {
                delivered.push(queue.pop_front().unwrap()).unwrap();
                queue_capture(&queue, message.clone(), &mut dropped).unwrap();
            }
        }
        while let Some(message) = queue.pop_front() {
            delivered.push(message).unwrap();
        }
        assert_eq!(delivered, expected);
        assert_eq!(dropped, 32);
    }

    #[test]
    fn composite_report_hid_and_storage_selection() {
        let config = [
            9, 2, 57, 0, 2, 1, 0, 0x80, 50, 9, 4, 2, 0, 1, 3, 0, 0, 0, 9, 0x21, 0x11, 1, 0, 1,
            0x22, 32, 0, 7, 5, 0x83, 3, 64, 0, 5, 9, 4, 3, 0, 2, 8, 6, 0x50, 0, 7, 5, 0x84, 2, 64,
            0, 0, 7, 5, 0x05, 2, 64, 0, 0,
        ];
        let selected = select_configuration(&config, false).unwrap();
        assert_eq!(selected.hid.len(), 1);
        assert!(!selected.hid[0].boot);
        assert_eq!(selected.hid[0].report_length, 32);
        let storage = selected.storage.unwrap();
        assert_eq!(storage.interface, 3);
        assert_eq!(storage.in_endpoint, 0x84);
        assert_eq!(storage.out_endpoint, 5);
        let mut bad = config;
        bad[26] = 5;
        assert_eq!(
            select_configuration(&bad, false),
            Err(UsbError::InvalidDescriptor)
        );
    }

    #[test]
    fn bulk_encoding_keeps_single_packet_and_data_pid() {
        for pid in [0, 2] {
            let packet = Packet {
                endpoint: 0x82,
                kind: 2,
                pid,
                mps: 64,
            };
            assert_eq!(packet.transfer_size(0), 64 | (1 << 19) | (pid << 29));
            assert_eq!((root_channel_characteristics(packet, 1, 0) >> 18) & 3, 2);
        }
    }

    #[test]
    fn completion_waits_for_rx_even_when_channel_is_halted() {
        let complete_halted = 1 | 2;
        assert_eq!(channel_outcome(complete_halted, true), None);
        assert_eq!(channel_outcome(complete_halted, false), Some(Ok(())));
        assert_eq!(channel_outcome(2, false), Some(Err(UsbError::Transaction)));
        assert_eq!(
            channel_outcome(2 | (1 << 4), false),
            Some(Err(UsbError::Nak))
        );
        assert_eq!(
            channel_outcome(complete_halted | (1 << 3), true),
            Some(Err(UsbError::Stall))
        );
        assert_eq!(
            channel_outcome(complete_halted | (1 << 8), false),
            Some(Err(UsbError::Babble))
        );
        assert_eq!(
            channel_outcome(1 << 10, false),
            Some(Err(UsbError::Transaction))
        );
        assert_eq!(channel_outcome(0, true), None);
    }

    #[test]
    fn root_channel_has_no_hub_low_speed_flag_and_periodic_uses_next_frame() {
        let control = root_channel_characteristics(Packet::control(0x80, 2, 8), 1, 0);
        assert_eq!(control, 8 | (1 << 15) | (1 << 20) | (1 << 22));
        let interrupt = Packet {
            endpoint: 0x83,
            kind: 3,
            pid: 0,
            mps: 8,
        };
        let even = root_channel_characteristics(interrupt, 1, 0);
        let odd = root_channel_characteristics(interrupt, 1, 1);
        assert_eq!(even & (1 << 17), 0);
        assert_eq!(even & (1 << 29), 1 << 29);
        assert_eq!(odd & (1 << 29), 0);
        assert_eq!((even >> 11) & 15, 3);
        assert_eq!((even >> 18) & 3, 3);
    }

    #[test]
    fn single_packet_setup_and_status_sizes() {
        let setup_packet = Packet::control(0, 3, 8);
        assert_eq!(setup_packet.kind, 0);
        let setup = setup_packet.transfer_size(8);
        assert_eq!(setup & 0x7ffff, 8);
        assert_eq!((setup >> 19) & 0x3ff, 1);
        assert_eq!((setup >> 29) & 3, 3);

        let out_status = Packet::control(0, 2, 64).transfer_size(0);
        assert_eq!(out_status & 0x7ffff, 0);
        assert_eq!((out_status >> 19) & 0x3ff, 1);
        assert_eq!((out_status >> 29) & 3, 2);

        let in_status = Packet::control(0x80, 2, 64).transfer_size(0);
        assert_eq!(in_status & 0x7ffff, 64);
        assert_eq!((in_status >> 19) & 0x3ff, 1);
        assert_eq!((in_status >> 29) & 3, 2);
    }

    #[test]
    fn boot_endpoint_and_malformed_chains() {
        let config = [
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0, 7, 5, 0x81, 3, 8, 0, 10,
        ];
        let (value, ep) = boot_endpoint(&config, true).unwrap();
        assert_eq!(value, 1);
        let ep = ep.unwrap();
        assert_eq!(ep.interface, 0);
        assert_eq!(ep.endpoint, 0x81);
        assert_eq!(ep.packet, 8);
        assert_eq!(ep.interval, 10);
        let mut invalid = config;
        invalid[18] = 0;
        assert_eq!(
            boot_endpoint(&invalid, false),
            Err(UsbError::InvalidDescriptor)
        );
        invalid = config;
        invalid[24] = 1;
        assert_eq!(
            boot_endpoint(&invalid, true),
            Err(UsbError::InvalidDescriptor)
        );
        invalid = config;
        invalid[14] = 0; // Not a HID interface.
        assert_eq!(boot_endpoint(&invalid, false).unwrap().1, None);
        assert_eq!(
            boot_endpoint(&config[..24], false),
            Err(UsbError::InvalidDescriptor)
        );
    }
}
