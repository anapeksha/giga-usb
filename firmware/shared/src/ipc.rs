//! Project-specific postcard framing over the BSP's atomic SPSC queues.
//!
//! Both images must use matching layouts at the same 32-byte-aligned NOLOAD
//! address. Configure shared D3 SRAM as normal, non-cacheable, non-bufferable
//! memory before access. M7 initializes both queues before releasing M4;
//! independent core restarts are unsupported.

use core::{cell::Cell, marker::PhantomData};
#[cfg(any(target_os = "none", test))]
use core::{
    mem::size_of,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
};

pub use giga_r1::ipc::QueueError;
use giga_r1::ipc::{Consumer, Producer, SharedQueue};

#[cfg(any(target_os = "none", test))]
use crate::protocol::PROTOCOL_VERSION;
use crate::protocol::{MAX_FRAME_SIZE, Message};

const FRAME_WORDS: usize = MAX_FRAME_SIZE.div_ceil(4);
type Queue = SharedQueue<FRAME_WORDS, 1>;
const _: () = assert!(Queue::BLOCK_CAPACITY == MAX_FRAME_SIZE);

#[cfg(any(target_os = "none", test))]
#[repr(C, align(32))]
struct Mailbox {
    version: AtomicU32,
    primary_to_secondary: Queue,
    secondary_to_primary: Queue,
}

#[cfg(any(target_os = "none", test))]
impl Mailbox {
    const fn new() -> Self {
        Self {
            version: AtomicU32::new(0),
            primary_to_secondary: Queue::new(),
            secondary_to_primary: Queue::new(),
        }
    }

    fn initialize(&self) {
        self.version.store(0, Ordering::Release);
        // Reset claims and frames, including stale contents retained across
        // reset. Publish the shared protocol version only after both are ready.
        self.primary_to_secondary
            .initialize_primary()
            .expect("invalid primary IPC queue");
        self.secondary_to_primary
            .initialize_primary()
            .expect("invalid secondary IPC queue");
        self.version.store(PROTOCOL_VERSION, Ordering::Release);
    }

    fn endpoint(&'static self, primary: bool) -> Endpoint {
        assert_eq!(
            self.version.load(Ordering::Acquire),
            PROTOCOL_VERSION,
            "IPC protocol version mismatch or mailbox not initialized"
        );
        let (tx, rx) = if primary {
            (&self.primary_to_secondary, &self.secondary_to_primary)
        } else {
            (&self.secondary_to_primary, &self.primary_to_secondary)
        };
        // Claims validate initialization and capture the queue generation.
        Endpoint {
            tx: tx.producer().expect("IPC producer unavailable"),
            rx: rx.consumer().expect("IPC consumer unavailable"),
            _not_sync: PhantomData,
        }
    }
}

// SAFETY: the embedded linker reserves this identical, aligned region in both
// images, outside runtime-cleared RAM. M7 resets queue metadata while M4 is
// held; payloads are published before reads. The only exception here is placement.
#[cfg(any(target_os = "none", test))]
#[allow(unsafe_code)]
#[used]
#[cfg_attr(target_os = "none", unsafe(link_section = ".ipc_mailbox"))]
static STORAGE: Mailbox = Mailbox::new();

#[cfg(any(target_os = "none", test))]
const _: () = assert!(size_of::<Mailbox>() <= 1024);

#[derive(Debug, Eq, PartialEq)]
pub enum SendError {
    Full,
    Serialize(postcard::Error),
    Transport(QueueError),
}

#[derive(Debug, Eq, PartialEq)]
pub enum ReceiveError {
    Deserialize(postcard::Error),
    TrailingBytes,
    Transport(QueueError),
}

/// Unique transmit/receive claims. Movable between tasks (`Send`), but neither
/// `Clone` nor `Sync`. Dropping it releases its generation's endpoint claims.
pub struct Endpoint {
    tx: Producer<'static, FRAME_WORDS, 1>,
    rx: Consumer<'static, FRAME_WORDS, 1>,
    _not_sync: PhantomData<Cell<()>>,
}

impl Endpoint {
    pub fn try_send(&mut self, message: &Message) -> Result<(), SendError> {
        let mut bytes = [0; MAX_FRAME_SIZE];
        let encoded = postcard::to_slice(message, &mut bytes).map_err(SendError::Serialize)?;
        self.tx.try_push(encoded).map_err(|error| match error {
            QueueError::Full => SendError::Full,
            error => SendError::Transport(error),
        })
    }

    pub fn try_receive(&mut self) -> Result<Option<Message>, ReceiveError> {
        let mut bytes = [0; MAX_FRAME_SIZE];
        let len = match self.rx.try_pop(&mut bytes) {
            Ok(len) => len,
            Err(QueueError::Empty) => return Ok(None),
            Err(error) => return Err(ReceiveError::Transport(error)),
        };
        // Pop has already released the slot: malformed postcard frames cannot
        // wedge the queue, and decoding accesses only this local copy.
        match postcard::take_from_bytes::<Message>(&bytes[..len]) {
            Ok((message, [])) => Ok(Some(message)),
            Ok(_) => Err(ReceiveError::TrailingBytes),
            Err(error) => Err(ReceiveError::Deserialize(error)),
        }
    }
}

/// M7 only: invoke once while M4 is held, before releasing it. Configure shared
/// SRAM as non-cacheable/non-bufferable memory first. Both queues are
/// initialized before publishing our version; do not rely on NOLOAD being zero.
#[cfg(any(target_os = "none", test))]
pub fn initialize_primary() -> Endpoint {
    static TAKEN: AtomicBool = AtomicBool::new(false);
    assert!(
        !TAKEN.swap(true, Ordering::Relaxed),
        "primary IPC already initialized"
    );
    STORAGE.initialize();
    STORAGE.endpoint(true)
}

/// M4 only: invoke once after boot's secondary HAL initialization, following
/// M7's mailbox initialization and M4 release. Version validation is not a
/// replacement for this boot ordering or the shared-memory attributes.
#[cfg(any(target_os = "none", test))]
pub fn initialize_secondary() -> Endpoint {
    static TAKEN: AtomicBool = AtomicBool::new(false);
    assert!(
        !TAKEN.swap(true, Ordering::Relaxed),
        "secondary IPC already initialized"
    );
    STORAGE.endpoint(false)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use self::std::boxed::Box;
    use super::*;
    use crate::protocol::Event;

    fn pair() -> (Endpoint, Endpoint) {
        let mailbox = Box::leak(Box::new(Mailbox::new()));
        mailbox.initialize();
        (mailbox.endpoint(true), mailbox.endpoint(false))
    }

    #[test]
    fn global_initializers() {
        // Only this test uses STORAGE; other tests have independent mailboxes.
        let mut primary = initialize_primary();
        let mut secondary = initialize_secondary();
        primary.try_send(&Message::ping()).unwrap();
        assert_eq!(secondary.try_receive(), Ok(Some(Message::ping())));
        secondary.try_send(&Message::pong()).unwrap();
        assert_eq!(primary.try_receive(), Ok(Some(Message::pong())));
        assert!(std::panic::catch_unwind(initialize_primary).is_err());
        assert!(std::panic::catch_unwind(initialize_secondary).is_err());
    }

    #[test]
    fn concurrent_sequence_transfer() {
        use self::std::{
            thread,
            time::{Duration, Instant},
        };
        let (mut producer, mut consumer) = pair();
        let deadline = Instant::now() + Duration::from_secs(10);
        let sender = thread::spawn(move || {
            for sequence in 0_u32..1000 {
                let message = Message::data(&sequence.to_le_bytes()).unwrap();
                loop {
                    match producer.try_send(&message) {
                        Ok(()) => break,
                        Err(SendError::Full) => {
                            assert!(Instant::now() < deadline, "producer timed out");
                            thread::yield_now();
                        }
                        Err(error) => panic!("send failed: {error:?}"),
                    }
                }
            }
        });
        let receiver = thread::spawn(move || {
            for sequence in 0_u32..1000 {
                let expected = Message::data(&sequence.to_le_bytes()).unwrap();
                loop {
                    match consumer.try_receive().unwrap() {
                        Some(message) => {
                            assert_eq!(message, expected);
                            break;
                        }
                        None => {
                            assert!(Instant::now() < deadline, "consumer timed out");
                            thread::yield_now();
                        }
                    }
                }
            }
            assert_eq!(consumer.try_receive(), Ok(None));
        });
        sender.join().unwrap();
        receiver.join().unwrap();
    }

    #[test]
    fn bidirectional_and_backpressure() {
        fn assert_send<T: Send>() {}
        assert_send::<Endpoint>();
        let (mut primary, mut secondary) = pair();
        assert_eq!(primary.try_receive(), Ok(None));
        let data = Message::data(&[0, 255, 1]).unwrap();
        primary.try_send(&data).unwrap();
        assert_eq!(primary.try_send(&Message::ping()), Err(SendError::Full));
        secondary.try_send(&Message::pong()).unwrap();
        assert_eq!(secondary.try_receive(), Ok(Some(data)));
        assert_eq!(primary.try_receive(), Ok(Some(Message::pong())));
        primary.try_send(&Message::ping()).unwrap();
        assert_eq!(secondary.try_receive(), Ok(Some(Message::ping())));
        assert_eq!(secondary.try_receive(), Ok(None));
    }

    #[test]
    fn optional_and_binary_payloads() {
        let (mut primary, mut secondary) = pair();
        for message in [
            Message::new(Event::Data),
            Message::data(&[]).unwrap(),
            Message::data(&[0, 255, 128, 0]).unwrap(),
            Message::data(&[255; 256]).unwrap(),
        ] {
            primary.try_send(&message).unwrap();
            assert_eq!(secondary.try_receive(), Ok(Some(message)));
        }
    }

    #[test]
    fn malformed_frames_are_consumed() {
        let (mut primary, mut secondary) = pair();
        for bytes in [&[][..], &[255][..], &[0, 0, 42][..]] {
            primary.tx.try_push(bytes).unwrap();
            let error = secondary.try_receive().unwrap_err();
            if bytes == [0, 0, 42] {
                assert_eq!(error, ReceiveError::TrailingBytes);
            } else {
                assert!(matches!(error, ReceiveError::Deserialize(_)));
            }
            assert_eq!(secondary.try_receive(), Ok(None));
            primary.try_send(&Message::data(&[]).unwrap()).unwrap();
            assert_eq!(
                secondary.try_receive(),
                Ok(Some(Message::data(&[]).unwrap()))
            );
        }
        assert_eq!(
            primary.tx.try_push(&[0; MAX_FRAME_SIZE + 1]),
            Err(QueueError::PayloadTooLarge)
        );
        assert_eq!(secondary.try_receive(), Ok(None));
    }

    #[test]
    fn transport_errors_are_preserved() {
        let mailbox = Box::leak(Box::new(Mailbox::new()));
        mailbox.initialize();
        let mut primary = mailbox.endpoint(true);
        // Quiescent reinitialization invalidates old claims by generation.
        // The peer is quiescent and no queued data needs to be retained here.
        mailbox.initialize();
        assert_eq!(
            primary.try_send(&Message::ping()),
            Err(SendError::Transport(QueueError::StaleEndpoint))
        );
        assert_eq!(
            primary.try_receive(),
            Err(ReceiveError::Transport(QueueError::StaleEndpoint))
        );
    }
}
