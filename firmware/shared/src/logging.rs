//! Shared logging facade, independent of the application's logging backend.

use core::fmt::Arguments;

#[cfg(not(feature = "defmt"))]
#[inline]
pub fn noop(_: Arguments<'_>) {}

pub fn info(args: Arguments<'_>) {
    #[cfg(feature = "defmt")]
    defmt::info!("{}", defmt::Display2Format(&args));
    #[cfg(not(feature = "defmt"))]
    noop(args);
}

pub fn warn(args: Arguments<'_>) {
    #[cfg(feature = "defmt")]
    defmt::warn!("{}", defmt::Display2Format(&args));
    #[cfg(not(feature = "defmt"))]
    noop(args);
}

pub fn error(args: Arguments<'_>) {
    #[cfg(feature = "defmt")]
    defmt::error!("{}", defmt::Display2Format(&args));
    #[cfg(not(feature = "defmt"))]
    noop(args);
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::logging::info(core::format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::logging::warn(core::format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => { $crate::logging::error(core::format_args!($($arg)*)) };
}

pub use crate::{log_error as error, log_info as info, log_warn as warn};
