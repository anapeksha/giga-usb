#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod ota_boot_faults;
#[cfg(test)]
mod ota_swap_tests;

pub mod ap_driver;
#[cfg(target_os = "none")]
pub mod boot;
pub mod dashboard;
pub mod event_stream;
pub mod ipc_delivery;
pub mod network_bus;
pub mod ota_control;
pub mod ota_image;
pub mod ota_journal_plan;
pub mod ota_manifest;
pub mod ota_nor;
pub mod ota_progress;
pub mod ota_state;
pub mod ota_upload;
pub mod proxy;
pub mod status;
pub mod trace_query;
pub mod usb;
