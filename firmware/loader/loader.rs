#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_std)]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_main)]

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn main() {}

const BASE: u32 = 0x90000000;
const SIZE: u32 = 0x1000000;

#[cfg(any(test, feature = "programming"))]
mod programming;

#[cfg(any(
    test,
    feature = "programming",
    all(target_arch = "arm", target_os = "none")
))]
fn offset(address: u32, size: u32) -> Option<u32> {
    let a = address.checked_sub(BASE)?;
    (size != 0 && a < SIZE && size <= SIZE - a).then_some(a)
}
#[cfg(any(test, all(target_arch = "arm", target_os = "none")))]
fn buffer(address: u32, size: u32, code_end: u32, msp: u32) -> bool {
    let Some(min_buffer) = code_end.checked_add(0x400) else {
        return false;
    };
    let Some(min_stack) = code_end.checked_add(0x100) else {
        return false;
    };
    let Some(stack_ceiling) = address.checked_sub(32) else {
        return false;
    };
    code_end >= 0x24000004
        && address >= min_buffer
        && address < 0x24070000
        && size != 0
        && size <= 0x24070000 - address
        && msp & 7 == 0
        && msp >= min_stack
        && msp <= stack_ceiling
}
#[cfg(any(test, all(target_arch = "arm", target_os = "none")))]
fn environment(v: [u32; 8]) -> bool {
    v[0] & 0x3d == 0x25
        && v[1] & 0x3f == 0
        && v[2] & 0xfff == 0
        && v[3] & 8 == 0
        && v[4] & (1 << 22) == 0
        && v[5] & (3 << 16) == 0
        && v[6] == 0
        && v[7] & (1 << 14) == 0 // QUADSPI clock must be gated.
}
#[repr(C)]
pub struct DeviceInfo {
    name: [u8; 100],
    kind: u16,
    start: u32,
    size: u32,
    page: u32,
    erased: u8,
    sectors: [[u32; 2]; 10],
}
const fn info() -> DeviceInfo {
    let mut name = [0; 100];
    let text: &[u8] = if cfg!(feature = "programming") {
        b"GIGA AT25 1f8901 SCOPED PROGRAM - UNQUALIFIED"
    } else {
        b"GIGA AT25 1f8901 READ ONLY - UNQUALIFIED"
    };
    let mut i = 0;
    while i < text.len() {
        name[i] = text[i];
        i += 1;
    }
    let mut sectors = [[0; 2]; 10];
    sectors[0] = [4096, 4096];
    DeviceInfo {
        name,
        kind: 7,
        start: BASE,
        size: SIZE,
        page: 256,
        erased: 255,
        sectors,
    }
}
#[used]
#[unsafe(no_mangle)]
#[cfg_attr(
    all(target_arch = "arm", target_os = "none"),
    unsafe(link_section = ".Dev_Info")
)]
pub static StorageInfo: DeviceInfo = info();

// Default and native editions never touch GPIO, regardless of inputs.
#[cfg(not(all(feature = "programming", target_arch = "arm", target_os = "none")))]
#[unsafe(no_mangle)]
pub extern "C" fn Write(_: u32, _: u32, _: *const u8) -> i32 {
    0
}
#[cfg(not(all(feature = "programming", target_arch = "arm", target_os = "none")))]
#[unsafe(no_mangle)]
pub extern "C" fn SectorErase(_: u32, _: u32) -> i32 {
    0
}
#[unsafe(no_mangle)]
pub extern "C" fn MassErase(_: u32) -> i32 {
    0
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
mod hardware {
    use super::*;
    use core::{
        arch::asm,
        ptr::{read_volatile, write_volatile},
        sync::atomic::{AtomicU32, Ordering},
    };

    // Word ABI: magic, stage, PRIMASK, CONTROL, IPSR, CPUID, eight
    // environment registers (gate order), JEDEC (0x1f8901), status, entry PRIMASK,
    // Read address, length, output pointer and sampled in-callback MSP.
    // Stages: 0 loaded, 1 sampling, 2 gate rejected, 3 GPIO setup,
    // 4 JEDEC rejected, 5 status sampling, 6 status rejected, 7 ready,
    // 8 Read address rejected, 9 Read buffer/stack rejected.
    // MAX marks an observation not yet made; Cube loads these bytes from ELF.
    #[used]
    #[unsafe(no_mangle)]
    #[unsafe(link_section = ".loader_diagnostics")]
    pub static LoaderDiagnostics: [AtomicU32; 21] = [
        AtomicU32::new(0x474c4431),
        AtomicU32::new(0),
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
        const { AtomicU32::new(u32::MAX) },
    ];

    unsafe extern "C" {
        static __loader_code_end: u8;
    }

    fn stage(value: u32) {
        LoaderDiagnostics[1].store(value, Ordering::SeqCst);
    }
    const D: usize = 0x58020c00;
    const E: usize = 0x58021000;
    const F: usize = 0x58021400;
    const G: usize = 0x58021800;
    // Unsafe boundary: exclusively debugger-invoked M7 callbacks, privileged
    // thread mode, scoped interrupt masking, fresh system-reset environment.
    // No references into host RAM; all buffer stores are validated byte MMIO.
    fn rd(a: usize) -> u32 {
        unsafe { read_volatile(a as *const u32) }
    }
    fn wr(a: usize, v: u32) {
        unsafe { write_volatile(a as *mut u32, v) }
    }
    struct InterruptGuard(u32);

    impl InterruptGuard {
        fn enter() -> Option<Self> {
            let primask: u32;
            let control: u32;
            let ipsr: u32;
            unsafe {
                asm!("mrs {0}, PRIMASK", "mrs {1}, CONTROL", "mrs {2}, IPSR", out(reg) primask, out(reg) control, out(reg) ipsr, options(nomem, nostack));
            }
            // Refuse before executing any privileged mask instruction.
            if control != 0 || ipsr != 0 {
                stage(1);
                for slot in &LoaderDiagnostics[2..] {
                    slot.store(u32::MAX, Ordering::SeqCst);
                }
                LoaderDiagnostics[2].store(primask, Ordering::SeqCst);
                LoaderDiagnostics[3].store(control, Ordering::SeqCst);
                LoaderDiagnostics[4].store(ipsr, Ordering::SeqCst);
                LoaderDiagnostics[16].store(primask, Ordering::SeqCst);
                stage(2);
                return None;
            }
            unsafe {
                asm!("cpsid i", options(nostack));
            }
            stage(1);
            for slot in &LoaderDiagnostics[2..] {
                slot.store(u32::MAX, Ordering::SeqCst);
            }
            LoaderDiagnostics[16].store(primask, Ordering::SeqCst);
            Some(Self(primask))
        }
    }

    impl Drop for InterruptGuard {
        fn drop(&mut self) {
            // Restore the caller's exact mask, including already-masked entry.
            unsafe {
                asm!("msr PRIMASK, {0}", in(reg) self.0, options(nostack));
            }
        }
    }

    fn gate() -> bool {
        let mut primask: u32;
        let mut control: u32;
        let mut ipsr: u32;
        unsafe {
            asm!("mrs {0}, PRIMASK", "mrs {1}, CONTROL", "mrs {2}, IPSR", out(reg) primask, out(reg) control, out(reg) ipsr, options(nomem, nostack));
        }
        let cpuid = rd(0xe000ed00);
        let env = [
            rd(0x58024400),
            rd(0x58024410),
            rd(0x58024418),
            rd(0x580244a0),
            rd(0x5200201c),
            rd(0xe000ed14),
            rd(0xe000ed94),
            rd(0x580244d4),
        ];
        for (slot, value) in LoaderDiagnostics[2..14]
            .iter()
            .zip([primask, control, ipsr, cpuid].into_iter().chain(env))
        {
            slot.store(value, Ordering::SeqCst);
        }
        primask == 1 && control == 0 && ipsr == 0 && cpuid & 0xfff0 == 0xc270 && environment(env)
    }
    fn pin(p: usize, n: u32, high: bool) {
        wr(p + 0x18, 1 << (n + if high { 0 } else { 16 }));
    }
    fn delay() {
        for _ in 0..32 {
            unsafe {
                asm!("nop", options(nomem, nostack));
            }
        }
    }
    fn mode(p: usize, n: u32, m: u32) {
        wr(p, (rd(p) & !(3 << (2 * n))) | (m << (2 * n)));
        wr(p + 4, rd(p + 4) & !(1 << n));
        wr(p + 0xc, rd(p + 0xc) & !(3 << (2 * n)));
    }
    fn byte(b: u8) -> u8 {
        let mut v = 0;
        for i in (0..8).rev() {
            pin(F, 10, false);
            pin(D, 11, b & (1 << i) != 0);
            delay();
            pin(F, 10, true);
            delay();
            v = (v << 1) | ((rd(D + 0x10) >> 12) & 1) as u8;
            pin(F, 10, false);
        }
        v
    }
    fn end() {
        pin(G, 6, true);
        delay();
    }
    fn status() -> u8 {
        pin(G, 6, false);
        byte(5);
        let s = byte(0);
        end();
        s
    }
    fn identify() -> bool {
        pin(G, 6, false);
        byte(0x9f);
        let id = [byte(0), byte(0), byte(0)];
        end();
        LoaderDiagnostics[14].store(
            (u32::from(id[0]) << 16) | (u32::from(id[1]) << 8) | u32::from(id[2]),
            Ordering::SeqCst,
        );
        if id != [0x1f, 0x89, 0x01] {
            stage(4);
            return false;
        }
        stage(5);
        let observed_status = status();
        LoaderDiagnostics[15].store(u32::from(observed_status), Ordering::SeqCst);
        let ready = observed_status & 3 == 0;
        stage(if ready { 7 } else { 6 });
        ready
    }
    fn init() -> bool {
        if !gate() {
            stage(2);
            return false;
        }
        stage(3);
        wr(0x580244e0, rd(0x580244e0) | 0x78);
        let _ = rd(0x580244e0);
        pin(G, 6, true);
        pin(F, 10, false);
        pin(D, 11, false);
        pin(E, 2, true);
        pin(F, 6, true);
        for (p, n, m) in [
            (G, 6, 1),
            (F, 10, 1),
            (D, 11, 1),
            (D, 12, 0),
            (E, 2, 1),
            (F, 6, 1),
        ] {
            mode(p, n, m);
        }
        identify()
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn Init() -> i32 {
        let Some(_interrupts) = InterruptGuard::enter() else {
            return 0;
        };
        init() as i32
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn Read(address: u32, size: u32, out: *mut u8) -> i32 {
        let Some(_interrupts) = InterruptGuard::enter() else {
            return 0;
        };
        let msp: u32;
        unsafe {
            asm!("mrs {0}, MSP", out(reg) msp, options(nomem, nostack));
        }
        for (slot, value) in LoaderDiagnostics[17..]
            .iter()
            .zip([address, size, out as u32, msp])
        {
            slot.store(value, Ordering::SeqCst);
        }
        let Some(a) = offset(address, size) else {
            stage(8);
            return 0;
        };
        let code_end = core::ptr::addr_of!(__loader_code_end) as u32;
        if !buffer(out as u32, size, code_end, msp) {
            stage(9);
            return 0;
        }
        if !init() {
            return 0;
        }
        // Fixed finite loops only; busy flash refuses immediately, never polls.
        // Short transactions bound CS assertion and permit debugger interruption.
        let mut done = 0;
        while done < size {
            let n = (size - done).min(256);
            let at = a + done;
            pin(G, 6, false);
            byte(3);
            byte((at >> 16) as u8);
            byte((at >> 8) as u8);
            byte(at as u8);
            for i in 0..n {
                unsafe {
                    write_volatile(out.wrapping_add((done + i) as usize), byte(0));
                }
            }
            end();
            done += n;
        }
        1
    }
    #[cfg(feature = "programming")]
    mod programming_hardware {
        include!("programming_hardware.rs");
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges() {
        assert_eq!(offset(BASE, SIZE), Some(0));
        assert_eq!(offset(BASE + SIZE - 1, 1), Some(SIZE - 1));
        for (a, n) in [
            (0, 1),
            (BASE, 0),
            (BASE + SIZE, 1),
            (BASE + SIZE - 1, 2),
            (u32::MAX, 2),
        ] {
            assert_eq!(offset(a, n), None);
        }
    }
    #[test]
    fn arena() {
        let end = 0x24000a54;
        assert!(buffer(0x24000e80, 256, end, 0x24000e50));
        assert!(buffer(end + 0x400, 1, end, end + 0x104));
        assert!(buffer(0x2406ffff, 1, end, 0x24000e50));
        for (a, n, code_end, msp) in [
            (end + 0x3ff, 1, end, end + 0x104),
            (0x24070000, 1, end, 0x24000e50),
            (0x2406ffff, 2, end, 0x24000e50),
            (u32::MAX, 2, end, 0x24000e50),
            (0x24000e80, 0, end, 0x24000e50),
            (0x24000e80, 1, end, end + 0xfc),
            (0x24000e80, 1, end, 0x24000e68),
            (0x24000e80, 1, end, 0x24000e51),
            (0x24000e80, 1, u32::MAX, 0x24000e50),
            (0x24000e80, 1, 0, 0x24000e50),
            (0, 1, end, 0x24000e50),
            (0x24000e80, 1, end + 0x100, 0x24000e50),
        ] {
            assert!(!buffer(a, n, code_end, msp));
        }
        assert!(buffer(0x24000e80, 256, end, 0x24000e60));
    }
    #[test]
    fn abi() {
        assert_eq!(core::mem::size_of::<DeviceInfo>(), 200);
        assert_eq!(core::mem::offset_of!(DeviceInfo, start), 104);
        assert_eq!(core::mem::offset_of!(DeviceInfo, sectors), 120);
        assert_eq!(StorageInfo.page, 256);
        assert_eq!(StorageInfo.sectors[0], [4096, 4096]);
        assert_eq!(StorageInfo.sectors[1], [0, 0]);
        let _: extern "C" fn(u32, u32, *const u8) -> i32 = Write;
        let _: extern "C" fn(u32, u32) -> i32 = SectorErase;
    }
    #[test]
    fn destructive_refusal() {
        for a in [0, BASE, BASE + SIZE - 1, u32::MAX] {
            assert_eq!(Write(a, u32::MAX, core::ptr::null()), 0);
            assert_eq!(SectorErase(a, u32::MAX), 0);
            assert_eq!(MassErase(a), 0);
        }
    }
    #[test]
    fn reset_gate() {
        let good = [0x25, 0, 0, 0, 0, 0x200, 0, 0];
        assert!(environment(good));
        for (i, b) in [
            (0, 1),
            (0, 4),
            (0, 8),
            (0, 32),
            (1, 1),
            (1, 8),
            (2, 1),
            (2, 0x100),
            (3, 8),
            (4, 1 << 22),
            (5, 1 << 16),
            (5, 1 << 17),
            (6, 1),
            (7, 1 << 14),
        ] {
            let mut bad = good;
            bad[i] ^= b;
            assert!(!environment(bad));
        }
    }
}
