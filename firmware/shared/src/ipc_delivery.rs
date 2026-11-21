//! Core-local IPC delivery utilities for cooperative tasks on one executor.
//!
//! `NoopRawMutex` provides no inter-core or interrupt synchronization. Keep these
//! queues in the owning core's private RAM: never place them in SRAM4 or share
//! them across cores or interrupt handlers. Cross-core transport remains in
//! [`crate::ipc`].
#![forbid(unsafe_code)]
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};

/// One owned value outside the queue: `Full` retains it unchanged. A terminal
/// transport error discards it once; callers classify the loss using `event`.
#[derive(Default)]
pub struct Pending(Option<crate::protocol::Message>);
impl Pending {
    pub const fn new() -> Self {
        Self(None)
    }
    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }
    pub fn event(&self) -> Option<crate::protocol::Event> {
        self.0.as_ref().map(|message| message.event)
    }
    /// Set the next frame only after the previous frame has been delivered or
    /// discarded by a terminal error. `None` leaves an empty pending slot empty.
    ///
    /// # Panics
    /// Panics if a frame is already pending, preventing silent replacement of a
    /// frame retained under backpressure.
    pub fn set(&mut self, message: Option<crate::protocol::Message>) {
        assert!(self.is_none());
        self.0 = message;
    }
    pub fn retry(
        &mut self,
        mut send: impl FnMut(&crate::protocol::Message) -> Result<(), crate::ipc::SendError>,
    ) -> Result<(), crate::ipc::SendError> {
        if let Some(message) = self.0.as_ref() {
            match send(message) {
                Ok(()) => self.0 = None,
                Err(crate::ipc::SendError::Full) => return Ok(()),
                Err(error) => {
                    self.0 = None;
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

pub struct Queue<T, const N: usize>(Channel<NoopRawMutex, T, N>);
impl<T, const N: usize> Default for Queue<T, N> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T, const N: usize> Queue<T, N> {
    pub const fn new() -> Self {
        Self(Channel::new())
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn is_full(&self) -> bool {
        self.0.is_full()
    }
    pub fn push_back(&self, value: T) -> Result<(), T> {
        self.0
            .try_send(value)
            .map_err(|embassy_sync::channel::TrySendError::Full(value)| value)
    }
    pub fn pop_front(&self) -> Option<T> {
        self.0.try_receive().ok()
    }
    pub fn clear(&self) {
        self.0.clear();
    }
    pub async fn receive(&self) -> T {
        self.0.receive().await
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use self::std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        task::Wake,
        vec::Vec,
    };
    use super::*;
    use crate::{ipc::SendError, protocol::Message};
    use core::{
        future::Future,
        pin::pin,
        task::{Context, Poll},
    };

    #[derive(Default)]
    struct Wakes(AtomicUsize);
    impl Wake for Wakes {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn channel_receive_future_wakes_when_capacity_peer_recovers() {
        let queue = Queue::<u32, 1>::new();
        let wakes = Arc::new(Wakes::default());
        let waker = wakes.clone().into();
        let mut cx = Context::from_waker(&waker);
        let mut receive = pin!(queue.receive());
        assert_eq!(receive.as_mut().poll(&mut cx), Poll::Pending);
        queue.push_back(42).unwrap();
        assert!(wakes.0.load(Ordering::Relaxed) > 0);
        assert_eq!(receive.as_mut().poll(&mut cx), Poll::Ready(42));
        assert!(queue.is_empty());
    }

    #[test]
    fn channel_send_future_wakes_when_consumer_frees_capacity() {
        let queue = Queue::<u32, 1>::new();
        queue.push_back(1).unwrap();
        let wakes = Arc::new(Wakes::default());
        let waker = wakes.clone().into();
        let mut cx = Context::from_waker(&waker);
        let mut send = pin!(queue.0.send(2));
        assert_eq!(send.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop_front(), Some(1));
        assert!(wakes.0.load(Ordering::Relaxed) > 0);
        assert_eq!(send.as_mut().poll(&mut cx), Poll::Ready(()));
        assert_eq!(queue.pop_front(), Some(2));
        assert!(queue.is_empty());
    }

    #[test]
    fn ipc_full_retries_exact_frame_then_delivers_burst_over_old_32() {
        let queue = Queue::<Message, 64>::new();
        for sequence in 0u32..64 {
            queue
                .push_back(Message::data(&sequence.to_le_bytes()).unwrap())
                .unwrap();
        }
        assert!(queue.push_back(Message::ping()).is_err());
        let mut pending = Pending::new();
        pending.set(queue.pop_front());
        for _ in 0..1000 {
            pending
                .retry(|message| {
                    assert_eq!(message, &Message::data(&0u32.to_le_bytes()).unwrap());
                    Err(SendError::Full)
                })
                .unwrap();
            assert!(!pending.is_none());
            assert_eq!(queue.len(), 63);
        }
        let mut delivered = Vec::new();
        loop {
            pending
                .retry(|message| {
                    delivered.push(message.clone());
                    Ok(())
                })
                .unwrap();
            if queue.is_empty() {
                break;
            }
            pending.set(queue.pop_front());
        }
        assert_eq!(delivered.len(), 64);
        for (sequence, message) in delivered.into_iter().enumerate() {
            assert_eq!(
                message,
                Message::data(&(sequence as u32).to_le_bytes()).unwrap()
            );
        }
        assert!(pending.is_none());
    }

    #[test]
    fn stalled_output_does_not_block_incoming_stop_or_independent_network() {
        let serial = Queue::<Message, 1>::new();
        let commands = Queue::<Message, 4>::new();
        let network = Queue::<Message, 64>::new();
        serial.push_back(Message::ping()).unwrap();
        let mut pending = Pending::new();
        pending.set(serial.pop_front());
        for _ in 0..4 {
            commands.push_back(Message::pong()).unwrap();
        }
        assert!(commands.push_back(Message::ping()).is_err());
        for tick in 0u32..1000 {
            pending.retry(|_| Err(SendError::Full)).unwrap();
            // Model a bounded service turn, not an await on the stalled output.
            if tick < 4 {
                assert_eq!(commands.pop_front(), Some(Message::pong()));
            }
            network
                .push_back(Message::data(&tick.to_le_bytes()).unwrap())
                .unwrap();
            assert_eq!(
                network.pop_front(),
                Some(Message::data(&tick.to_le_bytes()).unwrap())
            );
        }
        assert!(commands.is_empty());
        assert!(!pending.is_none());
    }

    #[test]
    fn queue_ram_is_bounded() {
        assert!(core::mem::size_of::<Queue<Message, 64>>() < 20 * 1024);
    }
}
