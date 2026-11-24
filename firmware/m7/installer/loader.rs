#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]

const INPUT_MAGIC: u32 = 0x31494c47;

const BASE: u32 = 0xf00000;
const END: u32 = 0x1000000;

// Field offsets/encodings: stm32-metapac 21.0.0 stm32h747xi-cm7,
// rcc_h7::{Cr,Cfgr,D1cfgr,Gcr}; FLASH BCM4 matches m7/src/boot.rs.
// Require canonical reset divider encodings, not reserved DIV1 aliases.
fn valid_environment(v: &[u32; 8]) -> bool {
    // ST LL_RCC_HSI_IsDividerReady defines HSIDIVF=1 as applied and ready,
    // not an error flag: require readiness as well as the DIV1 selection.
    v[0] & 0x3d == 0x25 // HSION, HSIRDY, HSIDIVF; HSIDIV=0
        && v[1] & 0x3f == 0 // SW and SWS both HSI
        && v[2] & 0xfff == 0 // HPRE, D1PPRE, D1CPRE canonical DIV1
        && v[3] & (1 << 3) == 0 // BOOT_C2
        && v[4] & (1 << 22) == 0 // current BCM4 option
        && v[5] & (3 << 16) == 0 // SCB CCR DC and IC
        && v[6] == 0 // MPU CTRL entirely reset/disabled
}

fn valid_request(v: &[u32; 8]) -> bool {
    if v[0] != INPUT_MAGIC || v[1] != 1 || v[6] != 0 || v[7] != 0 {
        return false;
    }
    let (op, offset, len) = (v[2], v[3], v[4]);
    match op {
        1 => len > 0 && len <= 262144 && offset < END && len <= END - offset,
        2 => (BASE..END).contains(&offset) && offset % 4096 == 0 && len == 4096,
        3 => offset == BASE && len == 32,
        _ => false,
    }
}
fn crc(bytes: impl Iterator<Item = u8>) -> u32 {
    let mut c = !0u32;
    for b in bytes {
        c ^= b as u32;
        for _ in 0..8 {
            c = (c >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}
fn valid_header(h: &[u8; 32]) -> bool {
    let len = u32::from_le_bytes([h[8], h[9], h[10], h[11]]);
    &h[..8] == b"GIGAUI01" && (18..=1048544).contains(&len) && h[16..].iter().all(|b| *b == 0)
}

#[cfg(not(test))]
mod hardware {
    use super::*;
    const RESULT_MAGIC: u32 = 0x31524c47;
    const MAILBOX: usize = 0x24010000;
    const DATA: usize = 0x24012000;
    const SCRATCH: usize = MAILBOX + 0x1000;
    use core::{
        arch::{asm, global_asm},
        ptr::{read_volatile, write_volatile},
    };

    global_asm!(
        ".section .entry,\"ax\"",
        ".word 0x20020000",
        ".word _start",
        ".global _start",
        ".thumb_func",
        "_start:",
        "cpsid i",
        "ldr sp, =0x20020000",
        "bl main",
        "2: b 2b"
    );

    // Only this module accesses fixed MMIO and reserved RAM. The host must halt both
    // cores, reset to the documented 64-MHz state, disable caches/MPU and load the
    // complete RAM invocation before entry. No Rust references alias mailbox RAM.
    fn rd(a: usize) -> u32 {
        unsafe { read_volatile(a as *const u32) }
    }
    fn wr(a: usize, v: u32) {
        unsafe { write_volatile(a as *mut u32, v) }
    }
    fn get(a: usize) -> u8 {
        unsafe { read_volatile(a as *const u8) }
    }
    fn put(a: usize, v: u8) {
        unsafe { write_volatile(a as *mut u8, v) }
    }
    fn delay() {
        for _ in 0..32 {
            unsafe { asm!("nop", options(nomem, nostack)) }
        }
    }
    fn idle() -> ! {
        loop {
            unsafe { asm!("nop", options(nomem, nostack)) }
        }
    }
    const D: usize = 0x58020c00;
    const E: usize = 0x58021000;
    const F: usize = 0x58021400;
    const G: usize = 0x58021800;
    fn pin(p: usize, n: u32, high: bool) {
        wr(p + 0x18, 1 << (n + if high { 0 } else { 16 }));
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
    fn select() {
        pin(G, 6, false);
    }
    fn deselect() {
        pin(G, 6, true);
        delay();
    }
    fn command(c: u8) {
        select();
        byte(c);
        deselect();
    }
    fn address(a: u32) {
        byte((a >> 16) as u8);
        byte((a >> 8) as u8);
        byte(a as u8);
    }
    fn status(c: u8) -> u8 {
        select();
        byte(c);
        let s = byte(0);
        deselect();
        s
    }
    fn read(c: u8, a: u32, out: usize, len: usize) {
        select();
        byte(c);
        address(a);
        if c == 0x5a {
            byte(0);
        }
        for i in 0..len {
            put(out + i, byte(0));
        }
        deselect();
    }
    fn result(offset: usize, value: u32) {
        wr(MAILBOX + 32 + offset, value);
    }
    fn finish(error: u32) -> ! {
        deselect();
        // WRDI is harmless for reads, and is attempted even on a busy/error exit.
        command(4);
        result(24, status(5) as u32);
        result(28, status(0x35) as u32);
        result(32, status(0x15) as u32);
        publish(error)
    }
    // Pre-GPIO failures must not even deselect or send WRDI/status commands.
    fn publish(error: u32) -> ! {
        result(8, error);
        unsafe {
            asm!("dsb", options(nostack));
        }
        result(4, if error == 0 { 2 } else { 0xffff });
        idle()
    }
    fn environment_gate() {
        // PAC bases: RCC=0x58024400, FLASH=0x52002000. Core addresses
        // and cache masks follow cortex-m 0.7.7 peripheral register layouts.
        let addresses = [
            0x58024400, 0x58024410, 0x58024418, 0x580244a0, 0x5200201c, 0xe000ed14, 0xe000ed94,
        ];
        let mut snapshot = [0; 8];
        for (i, a) in addresses.iter().enumerate() {
            snapshot[i] = rd(*a);
            wr(MAILBOX + 0x200 + i * 4, snapshot[i]);
        }
        wr(MAILBOX + 0x21c, 0);
        if !valid_environment(&snapshot) {
            publish(15);
        }
        // Debuggers may already have enabled CYCCNT; preserve its value/state.
        wr(0xe000edfc, rd(0xe000edfc) | (1 << 24));
        wr(0xe0001000, rd(0xe0001000) | 1);
        unsafe {
            asm!("dsb", "isb", options(nostack));
        }
        let start = rd(0xe0001004);
        for _ in 0..1024 {
            let advance = rd(0xe0001004).wrapping_sub(start);
            if advance != 0 {
                wr(MAILBOX + 0x21c, advance);
                return;
            }
        }
        publish(15)
    }
    fn ready() {
        let start = rd(0xe0001004);
        for _ in 0..2_000_000 {
            if status(5) & 1 == 0 {
                return;
            }
            if rd(0xe0001004).wrapping_sub(start) >= 64_000_000 {
                finish(3);
            }
        }
        finish(4)
    }
    fn enable() {
        ready();
        if status(5) & 0x7f != 0 || status(0x35) & 0xc4 != 0 {
            finish(2);
        }
        command(6);
        if status(5) & 0x7f != 2 {
            finish(5);
        }
    }
    fn program(a: u32, source: usize, len: usize) {
        enable();
        select();
        byte(2);
        address(a);
        for i in 0..len {
            byte(get(source + i));
        }
        deselect();
        ready();
        if status(5) & 3 != 0 {
            finish(6);
        }
    }
    #[unsafe(no_mangle)]
    extern "C" fn main() -> ! {
        for i in 0..16 {
            result(i * 4, 0);
        }
        result(0, RESULT_MAGIC);
        result(4, 1);
        let mut input = [0; 8];
        for (i, v) in input.iter_mut().enumerate() {
            *v = rd(MAILBOX + i * 4);
        }
        environment_gate();
        // Configure only the proven GPIO SPI route, never QUADSPI.
        wr(0x580244e0, rd(0x580244e0) | 0x78);
        let _ = rd(0x580244e0);
        pin(G, 6, true);
        pin(F, 10, false);
        pin(D, 11, false);
        pin(E, 2, true);
        pin(F, 6, true);
        mode(G, 6, 1);
        mode(F, 10, 1);
        mode(D, 11, 1);
        mode(D, 12, 0);
        mode(E, 2, 1);
        mode(F, 6, 1);

        if !valid_request(&input) {
            finish(7);
        }
        select();
        byte(0x9f);
        let id = byte(0) as u32 | ((byte(0) as u32) << 8) | ((byte(0) as u32) << 16);
        deselect();
        result(20, id);
        read(0x5a, 0, MAILBOX + 256, 256);
        result(24, status(5) as u32);
        result(28, status(0x35) as u32);
        result(32, status(0x15) as u32);
        if id != 0x01891f
            || rd(MAILBOX + 256) != 0x50444653
            || rd(MAILBOX + 256 + 0x34) != 0x07ffffff
            || rd(MAILBOX + 256 + 0x4c) != 0x520f200c
        {
            finish(1);
        }
        if status(5) & 0x7f != 0 || status(0x35) & 0xc4 != 0 {
            finish(2);
        }
        let (op, offset, len) = (input[2], input[3], input[4] as usize);
        if op != 1 && crc((0..len).map(|i| get(DATA + i))) != input[5] {
            finish(8);
        }
        if op == 2 && offset == BASE && (0..32).any(|i| get(DATA + i) != 255) {
            finish(9);
        }
        if op == 3 {
            let mut h = [0; 32];
            for (i, b) in h.iter_mut().enumerate() {
                *b = get(DATA + i);
            }
            if !valid_header(&h) {
                finish(10);
            }
            read(3, BASE, SCRATCH, 34);
            if (0..32).any(|i| get(SCRATCH + i) != 255)
                || get(SCRATCH + 32) != 0x1f
                || get(SCRATCH + 33) != 0x8b
            {
                finish(11);
            }
        }
        if op == 2 {
            result(12, 1);
            enable();
            select();
            byte(0x20);
            address(offset);
            deselect();
            ready();
            if status(5) & 3 != 0 {
                finish(6);
            }
            read(3, offset, SCRATCH, 4096);
            if (0..4096).any(|i| get(SCRATCH + i) != 255) {
                finish(12);
            }
            result(12, 2);
            for i in (0..4096).step_by(256) {
                if offset != BASE || i != 0 {
                    program(offset + i as u32, DATA + i, 256);
                } else {
                    program(offset + 32, DATA + 32, 224);
                }
                result(16, (i + 256) as u32);
            }
        } else if op == 3 {
            result(12, 2);
            program(BASE, DATA, 32);
        }
        result(12, 3);
        if op != 1 {
            read(3, offset, SCRATCH, len);
            if (0..len).any(|i| get(SCRATCH + i) != get(DATA + i)) {
                finish(13);
            }
        }
        read(3, offset, DATA, len);
        result(36, crc((0..len).map(|i| get(DATA + i))));
        result(40, len as u32);
        result(16, len as u32);
        result(12, 4);
        finish(0)
    }
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        publish(14)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(op: u32, offset: u32, len: u32) -> [u32; 8] {
        [INPUT_MAGIC, 1, op, offset, len, 0, 0, 0]
    }
    #[test]
    fn environment_reset_state() {
        let reset = [0x25, 0, 0, 0, 0, 0x200, 0, 0];
        assert!(valid_environment(&reset));
        for (word, bits) in [
            (0, 1),
            (0, 4),
            (0, 8),
            (0, 16),
            (0, 32),
            (1, 1),
            (1, 8),
            (2, 1),
            (2, 0x10),
            (2, 0x100),
            (3, 8),
            (4, 1 << 22),
            (5, 1 << 16),
            (5, 1 << 17),
            (6, 1),
            (6, 2),
            (6, 4),
        ] {
            let mut bad = reset;
            bad[word] ^= bits;
            assert!(!valid_environment(&bad), "word {word}, bits {bits:#x}");
        }
        let mut unrelated = reset;
        unrelated[0] |= 1 << 7;
        unrelated[3] |= 1 << 2;
        unrelated[4] |= 1 << 21;
        unrelated[7] = 123;
        assert!(valid_environment(&unrelated));
    }
    #[test]
    fn environment_attempt_three_snapshot() {
        // Reported 3-failure-environment.bin: DWT not reached by the old gate.
        let actual = [0xc025, 0, 0, 0, 0x0b86aaf0, 0x40200, 0, 0];
        assert!(valid_environment(&actual));
        for div in 1..=3 {
            let mut bad = actual;
            bad[0] |= div << 3;
            assert!(!valid_environment(&bad));
        }
        for source in 1..=7 {
            for shift in [0, 3] {
                let mut bad = actual;
                bad[1] |= source << shift;
                assert!(!valid_environment(&bad));
            }
        }
        for (word, bits) in [
            (0, 1),
            (0, 4),
            (0, 32),
            (2, 8),
            (2, 0x800),
            (3, 8),
            (4, 1 << 22),
            (5, 1 << 16),
            (5, 1 << 17),
            (6, 1),
        ] {
            let mut bad = actual;
            bad[word] ^= bits;
            assert!(!valid_environment(&bad));
        }
    }
    #[test]
    fn read_bounds() {
        assert!(valid_request(&request(1, END - 262144, 262144)));
        for (o, n) in [(0, 0), (0, 262145), (END, 1), (END - 1, 2), (u32::MAX, 2)] {
            assert!(!valid_request(&request(1, o, n)));
        }
    }
    #[test]
    fn sector_bounds() {
        assert!(valid_request(&request(2, BASE, 4096)));
        assert!(valid_request(&request(2, END - 4096, 4096)));
        for (o, n) in [
            (BASE - 4096, 4096),
            (BASE + 1, 4096),
            (END, 4096),
            (BASE, 8192),
        ] {
            assert!(!valid_request(&request(2, o, n)));
        }
    }
    #[test]
    fn envelope() {
        for i in [0, 1, 6, 7] {
            let mut v = request(1, 0, 1);
            v[i] ^= 1;
            assert!(!valid_request(&v));
        }
        assert!(!valid_request(&request(4, BASE, 32)));
        assert!(valid_request(&request(3, BASE, 32)));
        assert!(!valid_request(&request(3, BASE + 32, 32)));
    }
    #[test]
    fn crc_vector() {
        assert_eq!(crc(b"123456789".iter().copied()), 0xcbf43926);
    }
    #[test]
    fn header_bounds() {
        let mut h = [0; 32];
        h[..8].copy_from_slice(b"GIGAUI01");
        for n in [18u32, 1048544] {
            h[8..12].copy_from_slice(&n.to_le_bytes());
            assert!(valid_header(&h));
        }
        for n in [0u32, 17, 1048545, u32::MAX] {
            h[8..12].copy_from_slice(&n.to_le_bytes());
            assert!(!valid_header(&h));
        }
        h[8..12].copy_from_slice(&18u32.to_le_bytes());
        h[31] = 1;
        assert!(!valid_header(&h));
    }
}
