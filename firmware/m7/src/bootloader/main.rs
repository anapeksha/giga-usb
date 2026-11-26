#![no_std]
#![no_main]
#![allow(unsafe_code)]

//! Reset-only persistent boot target. All Rust code/constants and fault vectors
//! are copied to AXI SRAM before FLASH can be made busy. No HAL initialization,
//! interrupts, DMA, allocator, option writes, or release of M4 occurs here.
use core::{
    arch::{asm, global_asm},
    cell::RefCell,
    ptr::{read_volatile, write_volatile},
};
use embassy_boot::{BootLoader, BootLoaderConfig};
use embassy_stm32::pac;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
use m7::{
    ota_image, ota_manifest as layout,
    ota_nor::{self, Nor, Spi},
    ota_state::JournalState,
};

global_asm!(
    ".section .boot_flash_exceptions,\"a\"",
    ".rept 254",
    ".word _boot_flash_fault",
    ".endr",
    ".section .boot_reset,\"ax\"",
    ".global _boot_flash_fault",
    ".thumb_func",
    "_boot_flash_fault:",
    "b .",
    ".global _boot_reset",
    ".thumb_func",
    "_boot_reset:",
    "cpsid i",
    "ldr r0, =__boot_ram_load",
    "ldr r1, =__boot_ram_start",
    "ldr r2, =__boot_ram_end",
    "1:",
    "cmp r1, r2",
    "bhs 2f",
    "ldr r3, [r0], #4",
    "str r3, [r1], #4",
    "b 1b",
    "2:",
    "ldr r1, =__boot_bss_start",
    "ldr r2, =__boot_bss_end",
    "movs r3, #0",
    "3:",
    "cmp r1, r2",
    "bhs 4f",
    "str r3, [r1], #4",
    "b 3b",
    "4:",
    "ldr r0, =_boot_ram_vectors",
    "ldr r1, =0xe000ed08",
    "str r0, [r1]",
    "dsb",
    "isb",
    "bl boot_main",
    "b .",
    ".section .boot_ram_vectors,\"a\"",
    ".balign 1024",
    ".global _boot_ram_vectors",
    "_boot_ram_vectors:",
    ".word 0x24080000",
    ".word _boot_reset",
    ".rept 254",
    ".word boot_fault",
    ".endr",
);

fn rd(address: usize) -> u32 {
    unsafe { read_volatile(address as *const u32) }
}
fn wr(address: usize, value: u32) {
    unsafe { write_volatile(address as *mut u32, value) }
}
fn cycles() -> u32 {
    rd(0xe0001004)
}
fn feed() {
    pac::IWDG1
        .kr()
        .write(|w| w.set_key(pac::iwdg::vals::Key::RESET));
}
fn stop() -> ! {
    loop {
        cortex_m::asm::nop();
    }
}
#[unsafe(no_mangle)]
extern "C" fn boot_fault() -> ! {
    stop()
}
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    stop()
}

fn environment() -> Result<(), ota_nor::Error> {
    // Reset clock/readiness encodings must match the bounded DWT deadlines.
    if pac::RCC.cr().read().0 & 0x3d != 0x25
        || pac::RCC.cfgr().read().0 & 0x3f != 0
        || pac::RCC.d1cfgr().read().0 & 0xfff != 0
        || pac::RCC.gcr().read().boot_c2()
        || pac::FLASH.optsr_cur().read().0 & ((1 << 22) | (1 << 31)) != 0
        || rd(0xe000ed14) & (3 << 16) != 0
        || rd(0xe000ed94) != 0
    {
        return Err(ota_nor::Error::Protected);
    }
    wr(0xe000edfc, rd(0xe000edfc) | (1 << 24));
    wr(0xe0001000, rd(0xe0001000) | 1);
    let start = cycles();
    for _ in 0..1024 {
        if cycles().wrapping_sub(start) != 0 {
            return Ok(());
        }
    }
    Err(ota_nor::Error::Timeout)
}

fn watchdog() -> Result<(), ota_nor::Error> {
    pac::IWDG1
        .kr()
        .write(|w| w.set_key(pac::iwdg::vals::Key::START));
    pac::IWDG1
        .kr()
        .write(|w| w.set_key(pac::iwdg::vals::Key::ENABLE));
    pac::IWDG1
        .pr()
        .write(|w| w.set_pr(pac::iwdg::vals::Pr::DIVIDE_BY256));
    pac::IWDG1.rlr().write(|w| w.set_rl(4095));
    let start = cycles();
    for _ in 0..1_000_000 {
        if pac::IWDG1.sr().read().0 == 0 {
            feed();
            return Ok(());
        }
        if cycles().wrapping_sub(start) >= 6_400_000 {
            break;
        }
    }
    Err(ota_nor::Error::Timeout)
}

struct GpioSpi;
fn pin(port: pac::gpio::Gpio, n: usize, high: bool) {
    port.bsrr().write(|w| {
        if high {
            w.set_bs(n, true);
        } else {
            w.set_br(n, true);
        }
    });
}
impl GpioSpi {
    fn new() -> Self {
        pac::RCC.ahb4enr().modify(|w| {
            w.set_gpioden(true);
            w.set_gpioeen(true);
            w.set_gpiofen(true);
            w.set_gpiogen(true);
        });
        let _ = pac::RCC.ahb4enr().read();
        for (port, n, output, high) in [
            (pac::GPIOG, 6, true, true),
            (pac::GPIOF, 10, true, false),
            (pac::GPIOD, 11, true, false),
            (pac::GPIOD, 12, false, false),
            (pac::GPIOE, 2, true, true),
            (pac::GPIOF, 6, true, true),
        ] {
            pin(port, n, high);
            port.moder()
                .modify(|w| w.0 = (w.0 & !(3 << (n * 2))) | (u32::from(output) << (n * 2)));
            port.otyper().modify(|w| w.0 &= !(1 << n));
            port.pupdr().modify(|w| w.0 &= !(3 << (n * 2)));
        }
        Self
    }
}
impl Spi for GpioSpi {
    fn select(&mut self, selected: bool) {
        pin(pac::GPIOG, 6, !selected);
        cortex_m::asm::delay(32);
    }
    fn byte(&mut self, byte: u8) -> u8 {
        let mut result = 0;
        for bit in (0..8).rev() {
            pin(pac::GPIOD, 11, byte & (1 << bit) != 0);
            cortex_m::asm::delay(32);
            pin(pac::GPIOF, 10, true);
            cortex_m::asm::delay(32);
            result = (result << 1) | ((pac::GPIOD.idr().read().0 >> 12) & 1) as u8;
            pin(pac::GPIOF, 10, false);
        }
        result
    }
}

type External = RefCell<Nor<GpioSpi>>;
struct Partition<'a> {
    flash: &'a External,
    offset: u32,
    length: u32,
}
impl ErrorType for Partition<'_> {
    type Error = ota_nor::Error;
}
impl ReadNorFlash for Partition<'_> {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        self.length as usize
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check(offset, bytes.len(), self.length)?;
        self.flash.borrow_mut().read(self.offset + offset, bytes)?;
        feed();
        Ok(())
    }
}
fn check(offset: u32, length: usize, capacity: u32) -> Result<(), ota_nor::Error> {
    if offset > capacity || length > (capacity - offset) as usize {
        Err(ota_nor::Error::Bounds)
    } else {
        Ok(())
    }
}
fn external_ready(nor: &mut Nor<GpioSpi>) -> Result<(), ota_nor::Error> {
    let start = cycles();
    for _ in 0..2_000_000 {
        if nor.poll_ready()? {
            feed();
            return Ok(());
        }
        if cycles().wrapping_sub(start) >= 64_000_000 {
            return Err(nor.timeout());
        }
    }
    Err(nor.timeout())
}
impl NorFlash for Partition<'_> {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 4096;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check(offset, bytes.len(), self.length)?;
        let mut nor = self.flash.borrow_mut();
        let mut done = 0;
        while done < bytes.len() {
            let address = self.offset + offset + done as u32;
            let size = (256 - address as usize % 256).min(bytes.len() - done);
            nor.start_program(address, &bytes[done..done + size])?;
            external_ready(&mut nor)?;
            let mut verify = [0; 256];
            nor.read(address, &mut verify[..size])?;
            if verify[..size] != bytes[done..done + size] {
                return Err(ota_nor::Error::Verify);
            }
            done += size;
        }
        Ok(())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        if from > to || !from.is_multiple_of(4096) || !to.is_multiple_of(4096) {
            return Err(ota_nor::Error::Bounds);
        }
        check(from, (to - from) as usize, self.length)?;
        let mut nor = self.flash.borrow_mut();
        for offset in (from..to).step_by(4096) {
            nor.start_erase(self.offset + offset)?;
            external_ready(&mut nor)?;
            let mut verify = [0; 256];
            for index in (0..4096).step_by(256) {
                nor.read(self.offset + offset + index, &mut verify)?;
                if verify.iter().any(|b| *b != 0xff) {
                    return Err(ota_nor::Error::Verify);
                }
            }
        }
        Ok(())
    }
}

struct Internal;
impl ErrorType for Internal {
    type Error = ota_nor::Error;
}
impl ReadNorFlash for Internal {
    const READ_SIZE: usize = 1;
    fn capacity(&self) -> usize {
        layout::ACTIVE_LENGTH as usize
    }
    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        check(offset, bytes.len(), layout::ACTIVE_LENGTH)?;
        for (i, byte) in bytes.iter_mut().enumerate() {
            // Exclusive boot context, M4 held, and interrupts disabled. Only the
            // allocated ACTIVE range is read; all code and vectors live in RAM.
            *byte = unsafe {
                read_volatile((layout::ACTIVE_ORIGIN as usize + offset as usize + i) as *const u8)
            };
        }
        feed();
        Ok(())
    }
}
fn internal_ready(bank: pac::flash::Bank) -> Result<(), ota_nor::Error> {
    let start = cycles();
    for _ in 0..100_000_000 {
        let sr = bank.sr().read();
        if !sr.bsy() && !sr.qw() && !sr.wbne() {
            return if sr.0 & 0x07ee0000 != 0 {
                Err(ota_nor::Error::Verify)
            } else {
                Ok(())
            };
        }
        if cycles().wrapping_sub(start) >= 192_000_000 {
            break;
        }
    }
    // Returning into the application with a busy bank is never permitted.
    Err(ota_nor::Error::Timeout)
}
fn unlock(bank: pac::flash::Bank) -> Result<(), ota_nor::Error> {
    internal_ready(bank)?;
    if bank.cr().read().lock() {
        bank.keyr().write_value(0x45670123);
        bank.keyr().write_value(0xcdef89ab);
    }
    if bank.cr().read().lock() {
        return Err(ota_nor::Error::Protected);
    }
    bank.ccr().write(|w| w.0 = 0x07ef0000);
    Ok(())
}
impl NorFlash for Internal {
    const WRITE_SIZE: usize = 32;
    const ERASE_SIZE: usize = 0x20000;
    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        check(offset, bytes.len(), layout::ACTIVE_LENGTH)?;
        if !offset.is_multiple_of(32) || !bytes.len().is_multiple_of(32) {
            return Err(ota_nor::Error::Bounds);
        }
        for (index, chunk) in bytes.as_chunks::<32>().0.iter().enumerate() {
            let address = layout::ACTIVE_ORIGIN + offset + (index * 32) as u32;
            let bank = pac::FLASH.bank(usize::from(address >= layout::M4_ORIGIN));
            unlock(bank)?;
            bank.cr().modify(|w| {
                w.set_psize(3);
                w.set_pg(true);
            });
            for (word, data) in chunk.as_chunks::<4>().0.iter().enumerate() {
                unsafe {
                    write_volatile(
                        (address as usize + word * 4) as *mut u32,
                        u32::from_le_bytes(*data),
                    );
                }
            }
            cortex_m::asm::dsb();
            let ready = internal_ready(bank);
            bank.cr().modify(|w| {
                w.set_pg(false);
                w.set_lock(true);
            });
            ready?;
            let mut verify = [0; 32];
            self.read(address - layout::ACTIVE_ORIGIN, &mut verify)?;
            if verify != *chunk {
                return Err(ota_nor::Error::Verify);
            }
        }
        Ok(())
    }
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        if from > to || !from.is_multiple_of(0x20000) || !to.is_multiple_of(0x20000) {
            return Err(ota_nor::Error::Bounds);
        }
        check(from, (to - from) as usize, layout::ACTIVE_LENGTH)?;
        for offset in (from..to).step_by(0x20000) {
            let address = layout::ACTIVE_ORIGIN + offset;
            let bank = pac::FLASH.bank(usize::from(address >= layout::M4_ORIGIN));
            unlock(bank)?;
            bank.cr().modify(|w| {
                w.set_psize(3);
                w.set_snb(((address & 0xfffff) / 0x20000) as u8);
                w.set_ser(true);
                w.set_start(true);
            });
            let ready = internal_ready(bank);
            bank.cr().modify(|w| {
                w.set_ser(false);
                w.set_lock(true);
            });
            ready?;
            feed();
        }
        Ok(())
    }
}

fn boot() -> Result<(), ota_nor::Error> {
    environment()?;
    watchdog()?;
    // Keep the physical host unpowered throughout swap/authentication, before
    // the application's typed HAL pin owner is constructed.
    pac::RCC.ahb4enr().modify(|w| w.set_gpioaen(true));
    let _ = pac::RCC.ahb4enr().read();
    pin(pac::GPIOA, 15, false);
    pac::GPIOA.otyper().modify(|w| w.0 &= !(1 << 15));
    pac::GPIOA.pupdr().modify(|w| w.0 &= !(3 << 30));
    pac::GPIOA
        .moder()
        .modify(|w| w.0 = (w.0 & !(3 << 30)) | (1 << 30));
    let flash = RefCell::new(Nor::open(GpioSpi::new())?);
    let partition = |offset, length| Partition {
        flash: &flash,
        offset,
        length,
    };
    let state = JournalState::open(partition(
        layout::STATE_ORIGIN,
        layout::STATE_JOURNAL_LENGTH,
    ))
    .map_err(|_| ota_nor::Error::Verify)?;
    let mut engine = BootLoader::new(BootLoaderConfig {
        active: Internal,
        dfu: partition(layout::DFU_ORIGIN, layout::DFU_LENGTH),
        state,
    });
    #[repr(align(32))]
    struct Buffer([u8; 4096]);
    let mut buffer = Buffer([0; 4096]);
    let state = engine
        .prepare_boot(&mut buffer.0)
        .map_err(|_| ota_nor::Error::Verify)?;
    let mut external = partition(0, ota_nor::CAPACITY);
    if ota_image::select(&mut Internal, &mut external).is_err() {
        // A bad trial is never entered. A second preparation reverts the complete
        // pair; a bad confirmed image fails closed instead of inventing recovery.
        if state != embassy_boot::State::Swap {
            return Err(ota_nor::Error::Verify);
        }
        engine
            .prepare_boot(&mut buffer.0)
            .map_err(|_| ota_nor::Error::Verify)?;
        ota_image::select(&mut Internal, &mut external).map_err(|_| ota_nor::Error::Verify)?;
    }
    feed();
    // The validated application sets up its own HAL/IPC and is the sole M4
    // release authority. Reset clock configuration and watchdog remain intact.
    wr(0xe000ed08, layout::ACTIVE_ORIGIN);
    let msp = rd(layout::ACTIVE_ORIGIN as usize);
    let reset = rd(layout::ACTIVE_ORIGIN as usize + 4);
    unsafe {
        asm!("dsb", "isb", "msr msp, {msp}", "cpsie i", "bx {reset}", msp = in(reg) msp, reset = in(reg) reset, options(noreturn));
    }
}

#[unsafe(no_mangle)]
extern "C" fn boot_main() -> ! {
    let _ = boot();
    stop()
}
