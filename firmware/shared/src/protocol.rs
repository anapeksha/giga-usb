#![forbid(unsafe_code)]

//! Allocation-free wire contract shared by both cores.
//! Variant order and payload encoding are wire ABI; bump PROTOCOL_VERSION when
//! changing them, and rebuild/flash both core images together.

use heapless::Vec;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// Maximum assembled JSON UI record, shared by the producer and bridge.
pub const RECORD_BYTES: usize = 1024;

pub const MAX_PAYLOAD_SIZE: usize = 256;
/// Includes postcard's event, option and length tags (260 bytes at maximum).
pub const MAX_FRAME_SIZE: usize = 288;

pub const PROTOCOL_VERSION: u32 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Event {
    Ping,
    Pong,
    Data,
    Usb,
    Command,
    Ui,
    Proxy,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub event: Event,
    pub value: Option<Vec<u8, MAX_PAYLOAD_SIZE>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PayloadTooLarge;

impl Message {
    pub fn encode<T: Serialize>(event: Event, payload: &T) -> Result<Self, postcard::Error> {
        let mut buffer = [0; MAX_PAYLOAD_SIZE];
        let bytes = postcard::to_slice(payload, &mut buffer)?;
        Self::with_payload(event, bytes).map_err(|_| postcard::Error::SerializeBufferFull)
    }

    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, postcard::Error> {
        let bytes = self
            .value
            .as_ref()
            .ok_or(postcard::Error::DeserializeUnexpectedEnd)?;
        match postcard::take_from_bytes(bytes.as_slice())? {
            (value, []) => Ok(value),
            _ => Err(postcard::Error::DeserializeBadEncoding),
        }
    }

    pub const fn new(event: Event) -> Self {
        Self { event, value: None }
    }

    pub const fn ping() -> Self {
        Self::new(Event::Ping)
    }

    pub const fn pong() -> Self {
        Self::new(Event::Pong)
    }

    /// An empty slice produces `Some(empty)`, not `None`.
    pub fn with_payload(event: Event, bytes: &[u8]) -> Result<Self, PayloadTooLarge> {
        Ok(Self {
            event,
            value: Some(Vec::from_slice(bytes).map_err(|_| PayloadTooLarge)?),
        })
    }

    pub fn data(bytes: &[u8]) -> Result<Self, PayloadTooLarge> {
        Self::with_payload(Event::Data, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_preserve_optional_payload() {
        let messages = [
            Message::ping(),
            Message::pong(),
            Message::new(Event::Data),
            Message::data(&[]).unwrap(),
            Message::data(&[0, 255, 128, 0]).unwrap(),
            Message::data(&[255; MAX_PAYLOAD_SIZE]).unwrap(),
        ];
        for message in messages {
            let mut frame = [0; MAX_FRAME_SIZE];
            let encoded = postcard::to_slice(&message, &mut frame).unwrap();
            assert_eq!(postcard::from_bytes::<Message>(encoded).unwrap(), message);
        }
        assert_ne!(Message::new(Event::Data), Message::data(&[]).unwrap());
    }

    #[test]
    fn payload_limit() {
        assert!(Message::data(&[0; MAX_PAYLOAD_SIZE]).is_ok());
        assert_eq!(
            Message::data(&[0; MAX_PAYLOAD_SIZE + 1]),
            Err(PayloadTooLarge)
        );
    }
}
