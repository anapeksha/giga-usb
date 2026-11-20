use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};
use std::vec::Vec;

use super::*;

const ADDRESS: u32 = 0x1234;
const FUNCTION: u32 = 2;
const MAX_LEN: usize = 2048;
const GUARD: u8 = 0xa5;

fn ready(future: impl Future<Output = ()>) {
    let mut future = pin!(future);
    let mut context = Context::from_waker(Waker::noop());
    std::assert!(matches!(future.as_mut().poll(&mut context), Poll::Ready(())));
}

fn data(offset: usize) -> u8 {
    (offset.wrapping_mul(37).wrapping_add(11) % 251) as u8
}

#[derive(Debug, PartialEq)]
struct Command {
    arg: u32,
    len: usize,
    blocks: bool,
}

#[derive(Default)]
struct Mock {
    commands: Vec<Command>,
    bytes: Vec<u8>,
}

impl Mock {
    fn record(&mut self, arg: u32, len: usize, blocks: bool) {
        std::assert_ne!(len, 0, "no empty bus operations");
        self.commands.push(Command { arg, len, blocks });
    }

    fn read(&mut self, buffer: &mut [u8]) {
        for byte in buffer {
            *byte = data(self.bytes.len());
            self.bytes.push(*byte);
        }
    }
}

impl SdioBusCyw43<BLOCK_SIZE> for Mock {
    type Error = core::convert::Infallible;

    fn set_bus_to_high_speed(&mut self, _: u32) -> Result<(), Self::Error> {
        std::panic!("unexpected speed change")
    }

    async fn cmd52(&mut self, _: u32) -> Result<u16, Self::Error> {
        std::panic!("unexpected CMD52")
    }

    async fn cmd53_block_read(
        &mut self,
        arg: u32,
        blocks: &mut [Aligned<A4, [u8; BLOCK_SIZE]>],
    ) -> Result<(), Self::Error> {
        self.record(arg, blocks.len() * BLOCK_SIZE, true);
        for block in blocks {
            self.read(&mut block[..]);
        }
        Ok(())
    }

    async fn cmd53_byte_read(&mut self, arg: u32, buffer: &mut Aligned<A4, [u8]>) -> Result<(), Self::Error> {
        self.record(arg, buffer.len(), false);
        self.read(&mut buffer[..]);
        Ok(())
    }

    async fn cmd53_block_write(
        &mut self,
        arg: u32,
        blocks: &[Aligned<A4, [u8; BLOCK_SIZE]>],
    ) -> Result<(), Self::Error> {
        self.record(arg, blocks.len() * BLOCK_SIZE, true);
        for block in blocks {
            self.bytes.extend_from_slice(&block[..]);
        }
        Ok(())
    }

    async fn cmd53_byte_write(&mut self, arg: u32, buffer: &Aligned<A4, [u8]>) -> Result<(), Self::Error> {
        self.record(arg, buffer.len(), false);
        self.bytes.extend_from_slice(&buffer[..]);
        Ok(())
    }
}

fn check_commands(mock: &Mock, len: usize, write: bool) {
    // Build the expected wire arguments independently of cmd53_arg.
    let prefix = (u32::from(write) << 31) | (FUNCTION << 28) | (1 << 26);
    let full = len / BLOCK_SIZE;
    let tail = len % BLOCK_SIZE;
    let mut expected = Vec::new();
    if full != 0 {
        expected.push(Command {
            arg: prefix | (1 << 27) | (ADDRESS << 9) | full as u32,
            len: full * BLOCK_SIZE,
            blocks: true,
        });
    }
    if tail != 0 {
        expected.push(Command {
            arg: prefix | ((ADDRESS + (full * BLOCK_SIZE) as u32) << 9) | tail as u32,
            len: tail,
            blocks: false,
        });
    }
    std::assert_eq!(mock.commands, expected, "length {len}, write {write}");
}

#[test]
fn cmd53_reads_every_length_with_guards() {
    for len in 0..=MAX_LEN {
        let mut storage = Aligned::<A4, _>([GUARD; MAX_LEN + 8]);
        let mut bus = SdioBus::new(Mock::default());
        // A four-byte prefix keeps the requested slice aligned while guarding both ends.
        // SAFETY: Aligned<A4> storage plus a four-byte offset guarantees alignment.
        let buffer = unsafe { &mut *(&mut storage[4..4 + len] as *mut [u8] as *mut Aligned<A4, [u8]>) };
        ready(bus.cmd53_read(FUNCTION, ADDRESS, buffer));
        check_commands(&bus.sdio, len, false);
        let expected: Vec<u8> = (0..len).map(data).collect();
        std::assert_eq!(&storage[4..4 + len], expected, "read length {len}");
        std::assert_eq!(bus.sdio.bytes, expected, "bus read length {len}");
        std::assert!(storage[..4].iter().all(|b| *b == GUARD));
        std::assert!(storage[4 + len..].iter().all(|b| *b == GUARD), "guard length {len}");
    }
}

#[test]
fn cmd53_writes_every_length_exactly() {
    for len in 0..=MAX_LEN {
        let mut storage = Aligned::<A4, _>([GUARD; MAX_LEN + 8]);
        for (offset, byte) in storage[4..4 + len].iter_mut().enumerate() {
            *byte = data(offset);
        }
        let original = *storage;
        let mut bus = SdioBus::new(Mock::default());
        // SAFETY: Aligned<A4> storage plus a four-byte offset guarantees alignment.
        let buffer = unsafe { &*(&storage[4..4 + len] as *const [u8] as *const Aligned<A4, [u8]>) };
        ready(bus.cmd53_write(FUNCTION, ADDRESS, buffer));
        check_commands(&bus.sdio, len, true);
        std::assert_eq!(bus.sdio.bytes, &original[4..4 + len], "write length {len}");
        std::assert_eq!(*storage, original, "write modified source at length {len}");
    }
}
