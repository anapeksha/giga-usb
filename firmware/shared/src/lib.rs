#![no_std]
#![deny(unsafe_code)]

//! Shared wire contracts, serialized IPC, and HAL coordination metadata.
//! Application crates contain no unsafe code. Shared-memory access is confined
//! to the transport, with explicit initialization and acquire/release ownership.

#[cfg(target_os = "none")]
use core::mem::{MaybeUninit, size_of};
#[cfg(target_os = "none")]
use embassy_stm32::SharedData;

pub mod ipc;
pub mod logging;

pub mod protocol;
pub mod usb;
pub mod wifi;

pub const M4_FLASH_ORIGIN: u32 = 0x0810_0000;
pub const M4_FLASH_END: u32 = 0x081c_0000;
pub const M4_RAM_ORIGIN: u32 = 0x3000_0000;
pub const M4_RAM_END: u32 = 0x3002_0000;

// SAFETY: both memory.x files place this NOLOAD section at SRAM4 base, outside
// private RAM. M7 initializes it before releasing M4; neither runtime clears it.
#[cfg(target_os = "none")]
#[allow(unsafe_code)]
#[used]
#[unsafe(link_section = ".shared_hal")]
static HAL_STATE: MaybeUninit<SharedData> = MaybeUninit::uninit();

#[cfg(target_os = "none")]
const _: () = assert!(size_of::<SharedData>() <= 1024);

/// Shared HAL metadata placed identically by both linker scripts. Primary must
/// initialize it before releasing secondary; neither core may reinitialize it.
#[cfg(target_os = "none")]
pub fn hal_state() -> &'static MaybeUninit<SharedData> {
    &HAL_STATE
}
