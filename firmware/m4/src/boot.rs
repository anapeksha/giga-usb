#![forbid(unsafe_code)]

use embassy_stm32::Peripherals;
use shared::{
    hal_state,
    ipc::{self, Endpoint},
};

/// Adopt M7's initialized clocks and claim M4's sole IPC endpoint after release.
pub fn init_m4() -> (Peripherals, Endpoint) {
    let peripherals = embassy_stm32::init_secondary(hal_state());
    let ipc = ipc::initialize_secondary();
    (peripherals, ipc)
}
