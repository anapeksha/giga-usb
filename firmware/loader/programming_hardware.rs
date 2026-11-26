use super::*;
use crate::programming::*;

// Reuse the fixed diagnostic ABI: stage 10 range, 11 DWT, 12 protection,
// 13 WREN, 14 busy deadline, 15 verify/transition, 16 success, 17 WRDI.
// Words 17..20 retain address, length, input pointer (erase: inclusive end), MSP.
// Word 15 packs SR1 | SR2<<8 | SR3<<16 after programming status samples.
fn statuses() -> [u8; 3] {
    let mut s = [0; 3];
    for (out, command) in s.iter_mut().zip([5, 0x35, 0x15]) {
        pin(G, 6, false);
        byte(command);
        *out = byte(0);
        end();
    }
    LoaderDiagnostics[15].store(
        u32::from(s[0]) | (u32::from(s[1]) << 8) | (u32::from(s[2]) << 16),
        Ordering::SeqCst,
    );
    s
}
fn command(c: u8) {
    pin(G, 6, false);
    byte(c);
    end();
}
fn addressed(c: u8, a: u32) {
    pin(G, 6, false);
    byte(c);
    byte((a >> 16) as u8);
    byte((a >> 8) as u8);
    byte(a as u8);
}
fn clock() -> bool {
    // gate() has already established canonical 64-MHz HSI with DIV1.
    // Preserve debugger CYCCNT; never reset it. Reject an absent/locked counter.
    wr(0xe000edfc, rd(0xe000edfc) | (1 << 24));
    wr(0xe0001000, rd(0xe0001000) | 1);
    unsafe {
        asm!("dsb", "isb", options(nostack));
    }
    let start = rd(0xe0001004);
    for _ in 0..1024 {
        if rd(0xe0001004).wrapping_sub(start) != 0 {
            return true;
        }
    }
    stage(11);
    false
}
fn ready() -> bool {
    let start = rd(0xe0001004);
    for poll in 0..WAIT_POLLS {
        // Deadline wins even if the first ready sample arrives too late.
        if !within_wait(start, rd(0xe0001004), poll) {
            break;
        }
        let s = statuses();
        if s[0] & 1 == 0 && within_wait(start, rd(0xe0001004), poll) {
            if unprotected(s, false) {
                return true;
            }
            stage(12);
            return false;
        }
    }
    stage(14);
    false
}
fn enable() -> bool {
    if !unprotected(statuses(), false) {
        stage(12);
        return false;
    }
    command(6);
    if !unprotected(statuses(), true) {
        stage(13);
        return false;
    }
    true
}
fn finish(ok: bool) -> i32 {
    // Only reached after successful GPIO/identity initialization. Even timeout
    // exits attempt WRDI, but cannot promise an in-progress operation is cancelled.
    end();
    command(4);
    let s = statuses();
    if ok && !unprotected(s, false) {
        stage(17);
        return 0;
    }
    if ok {
        stage(16);
    }
    i32::from(ok)
}
fn sample(address: u32, size: u32, pointer: u32) -> u32 {
    let msp: u32;
    unsafe {
        asm!("mrs {0}, MSP", out(reg) msp, options(nomem, nostack));
    }
    for (slot, value) in LoaderDiagnostics[17..]
        .iter()
        .zip([address, size, pointer, msp])
    {
        slot.store(value, Ordering::SeqCst);
    }
    msp
}
fn source(pointer: u32, i: u32) -> u8 {
    // Entire source arena was checked before GPIO access; interrupts are masked
    // and M4 is held. The debugger must not mutate the buffer during callbacks.
    unsafe { read_volatile((pointer + i) as *const u8) }
}
fn compare(a: u32, pointer: u32, size: u32, transitions: bool) -> bool {
    let mut done = 0;
    let mut ok = true;
    while done < size {
        let n = (size - done).min(256);
        addressed(3, a + done);
        for i in 0..n {
            let old = byte(0);
            let new = source(pointer, done + i);
            ok &= if transitions {
                one_to_zero(old, new)
            } else {
                old == new
            };
        }
        end();
        done += n;
    }
    if !ok {
        stage(15);
    }
    ok
}

#[unsafe(no_mangle)]
pub extern "C" fn Write(address: u32, size: u32, input: *const u8) -> i32 {
    let Some(_interrupts) = InterruptGuard::enter() else {
        return 0;
    };
    let msp = sample(address, size, input as u32);
    let Some(a) = write_range(address, size) else {
        stage(10);
        return 0;
    };
    if !buffer(
        input as u32,
        size,
        core::ptr::addr_of!(__loader_code_end) as u32,
        msp,
    ) {
        stage(9);
        return 0;
    }
    if !init() {
        return 0;
    }
    if !clock() {
        return finish(false);
    }
    // Preflight the complete callback before making any byte destructive.
    if !unprotected(statuses(), false) {
        stage(12);
        return finish(false);
    }
    if !compare(a, input as u32, size, true) {
        return finish(false);
    }
    let mut done = 0;
    while done < size {
        let n = page_chunk(a + done, size - done);
        if !enable() {
            return finish(false);
        }
        addressed(2, a + done);
        for i in 0..n {
            byte(source(input as u32, done + i));
        }
        end();
        if !ready() || !compare(a + done, input as u32 + done, n, false) {
            return finish(false);
        }
        done += n;
    }
    finish(true)
}

#[unsafe(no_mangle)]
pub extern "C" fn SectorErase(first: u32, last: u32) -> i32 {
    let Some(_interrupts) = InterruptGuard::enter() else {
        return 0;
    };
    let msp = sample(first, MAX_ERASE, last);
    let Some(a) = erase_range(first, last) else {
        stage(10);
        return 0;
    };
    let code_end = core::ptr::addr_of!(__loader_code_end) as u32;
    // Erase has no buffer. Apply the same stack/code separation and ceiling.
    if !erase_stack(code_end, msp) {
        stage(9);
        return 0;
    }
    if !init() {
        return 0;
    }
    if !clock() || !enable() {
        return finish(false);
    }
    addressed(0x20, a);
    end();
    if !ready() {
        return finish(false);
    }
    let mut ok = true;
    for done in (0..MAX_ERASE).step_by(256) {
        addressed(3, a + done);
        for _ in 0..256 {
            ok &= byte(0) == 255;
        }
        end();
    }
    if !ok {
        stage(15);
    }
    finish(ok)
}
