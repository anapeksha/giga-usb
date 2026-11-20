#![forbid(unsafe_code)]

//! Safe, core-specific entry points for the board's boot sequence.
//!
//! Requires the supplied linker scripts, caches left disabled, and a full system
//! reset with BCM7=1 / BCM4=0. Build each core separately with matching shared
//! layouts. This is platform code, not an inter-core peripheral ownership lock.

use embassy_stm32::Peripherals;

use shared::{
    hal_state,
    ipc::{self, Endpoint},
};

use embassy_stm32::{
    Config, Peri,
    flash::{FLASH_BASE, Flash},
    pac,
    peripherals::FLASH,
    rcc::{
        AHBPrescaler, APBPrescaler, Pll, PllDiv, PllMul, PllPreDiv, PllSource, SupplyConfig,
        Sysclk, VoltageScale,
    },
};
use shared::{M4_FLASH_END, M4_FLASH_ORIGIN, M4_RAM_END};

/// Whether M7 released M4 after primary initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum M4Boot {
    Started,
    /// Missing/invalid vectors: M4 stays held, but M7 can continue running.
    InvalidVectors,
}

/// Boot settings that would make shared initialization unsafe across cores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BootError {
    /// Disable BCM4 in option bytes and perform a full system reset.
    M4NotHeld,
}

/// Initialize M7's HAL with the board's LDO supply and HSI-derived PLL1 clocks, then
/// release a valid M4 image. Invoke once from the M7 application entry point.
///
/// Returns an error without initializing shared state if M4 can already run.
/// Repeated initialization panics through the HAL's singleton check.
pub fn init_m7() -> Result<(Peripherals, Endpoint, M4Boot), BootError> {
    if pac::FLASH.optsr_cur().read().0 & (1 << 22) != 0 || pac::RCC.gcr().read().boot_c2() {
        return Err(BootError::M4NotHeld);
    }

    // Delegate board memory attributes to the BSP's ownership-based API.
    // SRAM4 must be non-cacheable/non-bufferable before HAL or IPC state is used.
    let mut core = cortex_m::Peripherals::take().expect("M7 core peripherals already taken");
    core.SCB.disable_dcache(&mut core.CPUID);
    giga_r1::bridge::configure_m7_shared_sram(core.MPU);

    let mut config = Config::default();
    config.rcc.supply_config = SupplyConfig::LDO;
    // SDMMC1 uses PLL1_Q. AXI SRAM (M7's RAM) is IDMA-accessible;
    // D-cache remains disabled above for SDIO buffer coherency.

    config.rcc.pll1 = Some(Pll {
        source: PllSource::HSI,
        prediv: PllPreDiv::DIV4,
        mul: PllMul::MUL50,
        divp: Some(PllDiv::DIV2),
        divq: Some(PllDiv::DIV4),
        divr: None,
    });
    config.rcc.sys = Sysclk::PLL1_P;
    config.rcc.ahb_pre = AHBPrescaler::DIV2;
    config.rcc.apb1_pre = APBPrescaler::DIV2;
    config.rcc.apb2_pre = APBPrescaler::DIV2;
    config.rcc.apb3_pre = APBPrescaler::DIV2;
    config.rcc.apb4_pre = APBPrescaler::DIV2;
    config.rcc.voltage_scale = VoltageScale::Scale1;
    // Both USB controllers share this 48 MHz source; configure it before M4 starts.
    config.rcc.mux.usbsel = pac::rcc::vals::Usbsel::HSI48;
    let mut peripherals = embassy_stm32::init_primary(config, hal_state());
    // Initialize both directional slots before M4 can observe the mailbox.
    let ipc = ipc::initialize_primary();
    let m4 = start_m4(peripherals.FLASH.reborrow());
    Ok((peripherals, ipc, m4))
}

fn start_m4(flash: Peri<'_, FLASH>) -> M4Boot {
    // Borrow the flash singleton only for the checked read; ownership returns to
    // the application when this driver is dropped. No raw pointers are needed.
    let mut flash = Flash::new_blocking(flash);
    let mut vectors = [0; 8];
    let offset = M4_FLASH_ORIGIN - FLASH_BASE as u32;
    if flash.blocking_read(offset, &mut vectors).is_err() {
        return M4Boot::InvalidVectors;
    }
    let msp = u32::from_le_bytes([vectors[0], vectors[1], vectors[2], vectors[3]]);
    let reset = u32::from_le_bytes([vectors[4], vectors[5], vectors[6], vectors[7]]);
    // The fixed M4 linker layout puts its initial, 8-byte-aligned MSP here.
    if msp != M4_RAM_END
        || !(M4_FLASH_ORIGIN..M4_FLASH_END).contains(&(reset & !1))
        || reset & 1 == 0
    {
        return M4Boot::InvalidVectors;
    }

    // M4 needs SRAM1 before its runtime touches stack, data or BSS.
    pac::RCC.ahb2enr().modify(|w| w.set_sram1en(true));
    let _ = pac::RCC.ahb2enr().read();
    pac::RCC.apb4enr().modify(|w| w.set_syscfgen(true));
    let _ = pac::RCC.apb4enr().read();

    // UR3[31:16] is BCM4_ADD0 on H747; the PAC calls it boot_add1.
    pac::SYSCFG
        .ur3()
        .modify(|w| w.set_boot_add1((M4_FLASH_ORIGIN >> 16) as u16));
    cortex_m::asm::dsb();
    pac::RCC.gcr().modify(|w| w.set_boot_c2(true));
    cortex_m::asm::dsb();
    M4Boot::Started
}
