#![forbid(unsafe_code)]

//! Project-owned USB commands, capture records and browser protocol.

use heapless::{String, Vec};
use serde::{Deserialize, Serialize};

pub const MAX_DESCRIPTOR_SIZE: usize = 1024;
pub const DESCRIPTOR_CHUNK_SIZE: usize = 128;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbSpeed {
    Low,
    Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbError {
    Timeout,
    Stall,
    Nak,
    Disconnected,
    Babble,
    Transaction,
    Overflow,
    InvalidDescriptor,
    Unsupported,
    Controller,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCommand {
    Start,
    Stop,
    Reset,
    Status,
    StorageList,
    /// Read up to 128 bytes using an index from the latest shared root listing.
    /// Indices are invalidated by relisting or a device-session change.
    StorageRead {
        file: u16,
        offset: u32,
    },
    /// Explicit intent bound to the current proxy snapshot, not authentication.
    ProxyEnable {
        generation: u32,
    },
    ProxyDisable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    Stopped,
    Waiting,
    Attached,
    Enumerating,
    /// The host configured the device; individual HID layouts may still be unsupported.
    Ready,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferKind {
    Control,
    Interrupt,
    Bulk,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStage {
    Setup,
    Data,
    Status,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: HostCommand,
}

/// Credential-free network diagnostics, available through USB-C even if Wi-Fi fails.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NetworkInfo {
    pub mode: String<12>,
    pub mac: [u8; 6],
    pub ip: Option<String<15>>,
    pub error: Option<String<64>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbEvent {
    Ready,
    Attached {
        speed: UsbSpeed,
    },
    Detached,
    PortReset,
    Descriptor {
        descriptor_type: u8,
        /// Interface number for HID report descriptors (0x22); zero for device/configuration.
        index: u8,
        offset: u16,
        total_length: u16,
        data: Vec<u8, DESCRIPTOR_CHUNK_SIZE>,
    },
    Configured {
        address: u8,
        configuration: u8,
    },
    /// One completed packet initiated by our host, not passive bus capture.
    /// Payload excludes token/handshake packets, CRC and physical-layer framing.
    Transfer {
        endpoint: u8,
        direction: Direction,
        bytes: u16,
        /// STM32 HCTSIZ DPID encoding: 0=DATA0, 2=DATA1, 3=SETUP.
        pid: u8,
        /// Exact payload, including all eight SETUP bytes; status ZLPs are empty.
        data: Vec<u8, 64>,
    },
    HidReport {
        endpoint: u8,
        data: Vec<u8, 64>,
    },
    Error {
        code: UsbError,
    },
    Lifecycle {
        state: HostState,
        session_id: u32,
    },
    /// Precedes its raw Transfer record. Queue loss can remove either record;
    /// consumers must not associate non-adjacent packets by endpoint alone.
    TransferContext {
        transfer_id: u32,
        session_id: u32,
        endpoint: u8,
        kind: TransferKind,
        stage: TransferStage,
        elapsed_us: u64,
        retries: u32,
    },
    /// Failure context retained before channel cleanup changes the registers.
    /// A zero transfer ID with empty SETUP denotes controller/idle context.
    Diagnostic {
        code: UsbError,
        setup: [u8; 8],
        session_id: u32,
        transfer_id: u32,
        stage: TransferStage,
        endpoint: u8,
        elapsed_us: u64,
        retries: u32,
        hcint: u32,
        hcchar: u32,
        hctsiz: u32,
        hprt: u32,
    },
    Status {
        enabled: bool,
        attached: bool,
        address: u8,
        transfers: u32,
        errors: u32,
        dropped: u32,
    },
    Storage {
        blocks: u32,
        block_size: u32,
        vendor: String<8>,
        product: String<16>,
        fat32: bool,
    },
    File {
        index: u16,
        name: String<13>,
        size: u32,
        directory: bool,
    },
    /// Live-only response to StorageRead; never replayed by processor snapshots.
    FileData {
        index: u16,
        offset: u32,
        total_size: u32,
        data: Vec<u8, 128>,
        eof: bool,
    },
    StorageError {
        message: String<96>,
    },
    Network {
        info: NetworkInfo,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapturedEvent {
    pub timestamp_us: u64,
    pub event: UsbEvent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub usb_version: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet_size: u8,
    pub configurations: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InterfaceInfo {
    pub number: u8,
    pub alternate: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EndpointInfo {
    pub interface: u8,
    pub alternate: u8,
    pub address: u8,
    pub attributes: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiEvent {
    Hello {
        protocol_version: u32,
    },
    Host {
        event: UsbEvent,
    },
    Device {
        descriptor: DeviceInfo,
    },
    Interface {
        descriptor: InterfaceInfo,
    },
    Endpoint {
        descriptor: EndpointInfo,
    },
    Keyboard {
        modifiers: u8,
        keys: Vec<u8, 6>,
        pressed: Vec<u8, 6>,
        released: Vec<u8, 6>,
    },
    Mouse {
        buttons: u8,
        dx: i8,
        dy: i8,
        wheel: i8,
    },
    Error {
        message: String<96>,
    },
    Dropped {
        count: u32,
    },
    Proxy {
        status: super::proxy::ProxyStatus,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UiRecord {
    pub timestamp_ms: u64,
    pub event: UiEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Event, Message};

    #[test]
    fn proxy_enable_requires_an_explicit_u32_generation() {
        for json in [
            &br#"{"command":"proxy_enable"}"#[..],
            &br#"{"command":{"proxy_enable":{}}}"#[..],
            &br#"{"command":{"proxy_enable":{"generation":-1}}}"#[..],
            &br#"{"command":{"proxy_enable":{"generation":4294967296}}}"#[..],
        ] {
            assert!(serde_json_core::from_slice::<CommandRequest>(json).is_err());
        }
        let json = br#"{"command":{"proxy_enable":{"generation":0}}}"#;
        let (request, used) = serde_json_core::from_slice::<CommandRequest>(json).unwrap();
        assert_eq!(used, json.len());
        assert_eq!(request.command, HostCommand::ProxyEnable { generation: 0 });
    }
    #[test]
    fn proxy_commands_and_snapshot_json_contract() {
        for (json, expected) in [
            (
                &br#"{"command":{"proxy_enable":{"generation":4294967295}}}"#[..],
                HostCommand::ProxyEnable {
                    generation: u32::MAX,
                },
            ),
            (
                &br#"{"command":"proxy_disable"}"#[..],
                HostCommand::ProxyDisable,
            ),
        ] {
            let (request, used) = serde_json_core::from_slice::<CommandRequest>(json).unwrap();
            assert_eq!(used, json.len());
            assert_eq!(request.command, expected);
            let message = Message::encode(Event::Command, &expected).unwrap();
            assert_eq!(message.decode::<HostCommand>().unwrap(), expected);
        }
        use crate::usb::proxy::{ProxyFault, ProxyRelease, ProxyStatus, ProxySupport};
        let record = UiRecord {
            timestamp_ms: 0,
            event: UiEvent::Proxy {
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
            },
        };
        let mut bytes = [0; crate::protocol::RECORD_BYTES];
        let n = serde_json_core::to_slice(&record, &mut bytes).unwrap();
        assert_eq!(
            core::str::from_utf8(&bytes[..n]).unwrap(),
            r#"{"timestamp_ms":0,"event":{"type":"proxy","status":{"enabled":false,"output_connected":false,"ready":false,"release":"pending","session":0,"generation":0,"keyboard":"absent","mouse":"absent","fault":"none","faults":0,"rejected_reports":0,"led_failures":0}}}"#
        );
    }

    #[test]
    fn storage_commands_and_maximum_records_fit_transport() {
        for (json, expected) in [
            (
                &br#"{"command":"storage_list"}"#[..],
                HostCommand::StorageList,
            ),
            (
                &br#"{"command":{"storage_read":{"file":31,"offset":65536}}}"#[..],
                HostCommand::StorageRead {
                    file: 31,
                    offset: 65536,
                },
            ),
        ] {
            let (request, used) = serde_json_core::from_slice::<CommandRequest>(json).unwrap();
            assert_eq!(used, json.len());
            assert_eq!(request.command, expected);
            let message = Message::encode(Event::Command, &expected).unwrap();
            assert_eq!(message.decode::<HostCommand>().unwrap(), expected);
        }
        let events = [
            UsbEvent::Storage {
                blocks: u32::MAX,
                block_size: u32::MAX,
                vendor: String::try_from("12345678").unwrap(),
                product: String::try_from("1234567890123456").unwrap(),
                fat32: true,
            },
            UsbEvent::File {
                index: u16::MAX,
                name: String::try_from("12345678.123").unwrap(),
                size: u32::MAX,
                directory: true,
            },
            UsbEvent::FileData {
                index: u16::MAX,
                offset: u32::MAX,
                total_size: u32::MAX,
                data: Vec::from_slice(&[255; 128]).unwrap(),
                eof: true,
            },
            UsbEvent::StorageError {
                message: {
                    let mut text = String::new();
                    for _ in 0..96 {
                        text.push('\0').unwrap();
                    }
                    text
                },
            },
        ];
        for event in events {
            let capture = CapturedEvent {
                timestamp_us: u64::MAX,
                event: event.clone(),
            };
            let message = Message::encode(Event::Usb, &capture).unwrap();
            assert_eq!(message.decode::<CapturedEvent>().unwrap(), capture);
            assert!(message.value.as_ref().unwrap().len() <= 256);
            let mut frame = [0; crate::protocol::MAX_FRAME_SIZE];
            assert!(postcard::to_slice(&message, &mut frame).is_ok());
            let record = UiRecord {
                timestamp_ms: u64::MAX,
                event: UiEvent::Host { event },
            };
            let mut json = [0; 1024];
            assert!(serde_json_core::to_slice(&record, &mut json).is_ok());
        }
    }

    #[test]
    fn host_context_events_fit_transport() {
        let events = [
            UsbEvent::Lifecycle {
                state: HostState::Enumerating,
                session_id: u32::MAX,
            },
            UsbEvent::TransferContext {
                transfer_id: u32::MAX,
                session_id: u32::MAX,
                endpoint: 0x81,
                kind: TransferKind::Bulk,
                stage: TransferStage::Data,
                elapsed_us: u64::MAX,
                retries: u32::MAX,
            },
            UsbEvent::Diagnostic {
                code: UsbError::Timeout,
                setup: [0; 8],
                session_id: u32::MAX,
                transfer_id: u32::MAX,
                stage: TransferStage::Status,
                endpoint: 0x81,
                elapsed_us: u64::MAX,
                retries: u32::MAX,
                hcint: u32::MAX,
                hcchar: u32::MAX,
                hctsiz: u32::MAX,
                hprt: u32::MAX,
            },
        ];
        for event in events {
            let capture = CapturedEvent {
                timestamp_us: u64::MAX,
                event,
            };
            let message = Message::encode(Event::Usb, &capture).unwrap();
            assert_eq!(message.decode::<CapturedEvent>().unwrap(), capture);
            let mut frame = [0; crate::protocol::MAX_FRAME_SIZE];
            assert!(postcard::to_slice(&message, &mut frame).is_ok());
            let record = UiRecord {
                timestamp_ms: u64::MAX,
                event: UiEvent::Host {
                    event: capture.event,
                },
            };
            let mut json = [0; crate::protocol::RECORD_BYTES];
            assert!(serde_json_core::to_slice(&record, &mut json).is_ok());
        }
    }

    #[test]
    fn network_diagnostics_fit_ipc_without_credentials() {
        let info = NetworkInfo {
            mode: String::try_from("connected").unwrap(),
            mac: [2, 0, 0, 0, 0x47, 1],
            ip: Some(String::try_from("192.168.29.100").unwrap()),
            error: Some(String::try_from("DHCP timed out").unwrap()),
        };
        let capture = CapturedEvent {
            timestamp_us: u64::MAX,
            event: UsbEvent::Network { info },
        };
        let message = Message::encode(Event::Usb, &capture).unwrap();
        assert_eq!(message.decode::<CapturedEvent>().unwrap(), capture);
        let mut frame = [0; crate::protocol::MAX_FRAME_SIZE];
        assert!(postcard::to_slice(&message, &mut frame).is_ok());
        let record = UiRecord {
            timestamp_ms: 1,
            event: UiEvent::Host {
                event: capture.event,
            },
        };
        let mut json = [0; crate::protocol::RECORD_BYTES];
        let length = serde_json_core::to_slice(&record, &mut json).unwrap();
        let json = core::str::from_utf8(&json[..length]).unwrap();
        assert!(json.contains("192.168.29.100"));
        assert!(!json.contains("password"));
        assert!(!json.contains("ssid"));
    }

    #[test]
    fn raw_packets_roundtrip_and_fit_transport_bounds() {
        assert_eq!(crate::protocol::PROTOCOL_VERSION, 10);
        for (endpoint, direction, pid, payload) in [
            (0, Direction::Out, 3, &[0x80, 6, 0, 1, 0, 0, 18, 0][..]),
            (0x80, Direction::In, 0, &[255; 64][..]),
            (0x81, Direction::In, 2, &[0, 0, 4, 0, 0, 0, 0, 0][..]),
            (0, Direction::Out, 2, &[][..]),
            (0x80, Direction::In, 2, &[][..]),
        ] {
            let event = UsbEvent::Transfer {
                endpoint,
                direction,
                bytes: payload.len() as u16,
                pid,
                data: Vec::from_slice(payload).unwrap(),
            };
            let capture = CapturedEvent {
                timestamp_us: u64::MAX,
                event: event.clone(),
            };
            let message = Message::encode(Event::Usb, &capture).unwrap();
            assert_eq!(message.decode::<CapturedEvent>().unwrap(), capture);
            let mut frame = [0; crate::protocol::MAX_FRAME_SIZE];
            let encoded = postcard::to_slice(&message, &mut frame).unwrap();
            if payload.len() == 64 {
                assert_eq!(message.value.as_ref().unwrap().len(), 80);
                assert_eq!(encoded.len(), 83);
            }
            let record = UiRecord {
                timestamp_ms: u64::MAX,
                event: UiEvent::Host { event },
            };
            let mut json = [0; 1024];
            let length = serde_json_core::to_slice(&record, &mut json).unwrap();
            assert!(length <= crate::protocol::RECORD_BYTES);
        }
    }

    #[test]
    fn raw_setup_json_golden() {
        let record = UiRecord {
            timestamp_ms: 123,
            event: UiEvent::Host {
                event: UsbEvent::Transfer {
                    endpoint: 0,
                    direction: Direction::Out,
                    bytes: 8,
                    pid: 3,
                    data: Vec::from_slice(&[128, 6, 0, 1, 0, 0, 18, 0]).unwrap(),
                },
            },
        };
        let mut json = [0; 1024];
        let length = serde_json_core::to_slice(&record, &mut json).unwrap();
        assert_eq!(&json[..length], br#"{"timestamp_ms":123,"event":{"type":"host","event":{"transfer":{"endpoint":0,"direction":"out","bytes":8,"pid":3,"data":[128,6,0,1,0,0,18,0]}}}}"#);
    }

    #[test]
    fn full_descriptor_chunk_fits_ipc_and_roundtrips() {
        let capture = CapturedEvent {
            timestamp_us: u64::MAX,
            event: UsbEvent::Descriptor {
                descriptor_type: 2,
                index: 0,
                offset: 896,
                total_length: MAX_DESCRIPTOR_SIZE as u16,
                data: Vec::from_slice(&[255; DESCRIPTOR_CHUNK_SIZE]).unwrap(),
            },
        };
        let message = Message::encode(Event::Usb, &capture).unwrap();
        assert_eq!(message.decode::<CapturedEvent>().unwrap(), capture);
        let mut frame = [0; crate::protocol::MAX_FRAME_SIZE];
        assert!(postcard::to_slice(&message, &mut frame).is_ok());
    }

    #[test]
    fn largest_ui_payloads_fit_network_record() {
        let records = [
            UiRecord {
                timestamp_ms: u64::MAX,
                event: UiEvent::Host {
                    event: UsbEvent::Descriptor {
                        descriptor_type: u8::MAX,
                        index: u8::MAX,
                        offset: u16::MAX,
                        total_length: u16::MAX,
                        data: Vec::from_slice(&[u8::MAX; DESCRIPTOR_CHUNK_SIZE]).unwrap(),
                    },
                },
            },
            UiRecord {
                timestamp_ms: u64::MAX,
                event: UiEvent::Error {
                    message: {
                        let mut message = String::new();
                        for _ in 0..96 {
                            message.push(char::from(0)).unwrap();
                        }
                        message
                    },
                },
            },
        ];
        let mut buffer = [0; crate::protocol::RECORD_BYTES];
        for record in records {
            assert!(serde_json_core::to_slice(&record, &mut buffer).is_ok());
        }
    }

    #[test]
    fn browser_json_matches_wire_contract() {
        let record = UiRecord {
            timestamp_ms: 123,
            event: UiEvent::Host {
                event: UsbEvent::Attached {
                    speed: UsbSpeed::Full,
                },
            },
        };
        let mut buffer = [0; 256];
        let len = serde_json_core::to_slice(&record, &mut buffer).unwrap();
        assert_eq!(&buffer[..len], br#"{"timestamp_ms":123,"event":{"type":"host","event":{"attached":{"speed":"full"}}}}"#);
        let (command, used) =
            serde_json_core::from_slice::<CommandRequest>(br#"{"command":"reset"}"#).unwrap();
        assert_eq!(used, 19);
        assert_eq!(command.command, HostCommand::Reset);
        let message = Message::encode(Event::Command, &command.command).unwrap();
        assert_eq!(message.decode::<HostCommand>().unwrap(), HostCommand::Reset);
    }
}
