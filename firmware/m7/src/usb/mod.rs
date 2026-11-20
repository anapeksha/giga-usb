use shared::usb::types;

#[cfg(target_os = "none")]
pub mod host;
#[cfg(any(target_os = "none", test))]
mod host_logic;
#[cfg(target_os = "none")]
mod host_storage;
pub mod storage;
