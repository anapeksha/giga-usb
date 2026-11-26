#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_std)]
#![cfg_attr(all(target_arch = "arm", target_os = "none"), no_main)]

#[cfg(not(all(target_arch = "arm", target_os = "none")))]
fn main() {}

const BASE: u32 = 0x90000000;
const SIZE: u32 = 0x1000000;

#[cfg(any(test, all(target_arch = "arm", target_os = "none")))]
fn offset(address: u32, size: u32) -> Option<u32> {
    let a = address.checked_sub(BASE)?;
    (size != 0 && a < SIZE && size <= SIZE - a).then_some(a)
}
#[cfg(any(test, all(target_arch = "arm", target_os = "none")))]
fn buffer(address: u32, size: u32) -> bool {
    // Conservative buffer policy; CubeProgrammer's allocation is unverified.
    (0x24010000..0x24070000).contains(&address) && size != 0 && size <= 0x24070000 - address
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
    let text = b"GIGA AT25 1f8901 READ ONLY - UNQUALIFIED";
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

// Qualification edition: these callbacks never touch GPIO, regardless of inputs.
#[unsafe(no_mangle)]
pub extern "C" fn Write(_: u32, _: u32, _: *const u8) -> i32 {
    0
}
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
    };
    const D: usize = 0x58020c00;
    const E: usize = 0x58021000;
    const F: usize = 0x58021400;
    const G: usize = 0x58021800;
    // Unsafe boundary: exclusively debugger-invoked M7 callbacks, privileged
    // thread mode, interrupts masked by caller, fresh system-reset environment.
    // No references into host RAM; all buffer stores are validated byte MMIO.
    fn rd(a: usize) -> u32 {
        unsafe { read_volatile(a as *const u32) }
    }
    fn wr(a: usize, v: u32) {
        unsafe { write_volatile(a as *mut u32, v) }
    }
    fn gate() -> bool {
        let mut primask: u32;
        let mut control: u32;
        let mut ipsr: u32;
        unsafe {
            asm!("mrs {0}, PRIMASK", "mrs {1}, CONTROL", "mrs {2}, IPSR", out(reg) primask, out(reg) control, out(reg) ipsr, options(nomem, nostack));
        }
        primask == 1
            && control == 0
            && ipsr == 0
            && rd(0xe000ed00) & 0xfff0 == 0xc270
            && environment([
                rd(0x58024400),
                rd(0x58024410),
                rd(0x58024418),
                rd(0x580244a0),
                rd(0x5200201c),
                rd(0xe000ed14),
                rd(0xe000ed94),
                rd(0x580244d4),
            ])
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
        id == [0x1f, 0x89, 0x01] && status() & 3 == 0
    }
    fn init() -> bool {
        if !gate() {
            return false;
        }
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
        init() as i32
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn Read(address: u32, size: u32, out: *mut u8) -> i32 {
        let Some(a) = offset(address, size) else {
            return 0;
        };
        if !buffer(out as u32, size) || !init() {
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
        assert!(buffer(0x24010000, 0x60000));
        for (a, n) in [
            (0x2400ffff, 1),
            (0x24070000, 1),
            (0x2406ffff, 2),
            (u32::MAX, 2),
            (0x24010000, 0),
        ] {
            assert!(!buffer(a, n));
        }
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
