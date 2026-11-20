#![no_std]
#![forbid(unsafe_code)]

#[cfg(target_os = "none")]
pub mod boot;
pub mod usb;
