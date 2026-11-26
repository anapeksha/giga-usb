//! Pure policy shared by the opt-in hardware callbacks and native tests.
#![cfg_attr(not(all(target_arch = "arm", target_os = "none")), allow(dead_code))]

pub const MAX_WRITE: u32 = 4096;
pub const MAX_ERASE: u32 = 4096;
pub const WAIT_CYCLES: u32 = 64_000_000;
pub const WAIT_POLLS: u32 = 4096;

pub fn permitted(address: u32, size: u32) -> Option<u32> {
    let a = super::offset(address, size)?;
    let end = a.checked_add(size)?;
    [
        (0x200000, 0x203000),
        (0x220000, 0x221000),
        (0xf00000, 0x1000000),
    ]
    .into_iter()
    .any(|(lo, hi)| a >= lo && end <= hi)
    .then_some(a)
}

pub fn write_range(address: u32, size: u32) -> Option<u32> {
    (size <= MAX_WRITE).then_some(())?;
    permitted(address, size)
}

// Cube 2.24 measured single-sector calls use last == first (last sector base).
// Also accept the reviewed byte-inclusive end, never a multisector envelope.
pub fn erase_range(first: u32, last: u32) -> Option<u32> {
    let inclusive_end = first.checked_add(MAX_ERASE - 1)?;
    (first.is_multiple_of(MAX_ERASE) && (last == first || last == inclusive_end)).then_some(())?;
    permitted(first, MAX_ERASE)
}

pub fn erase_stack(code_end: u32, msp: u32) -> bool {
    code_end >= 0x24000004
        && code_end.checked_add(0x100).is_some_and(|min| msp >= min)
        && msp & 7 == 0
        && msp <= 0x2406ffe0
}

pub fn page_chunk(offset: u32, remaining: u32) -> u32 {
    remaining.min(256 - (offset & 255))
}

pub fn one_to_zero(old: u8, new: u8) -> bool {
    old & new == new
}

pub fn unprotected(s: [u8; 3], wel: bool) -> bool {
    // Rev Q tables 3–5: SR1 BUSY/WEL/BP4..0; SR2 SUS1/CMP/SUS2.
    // SRP0/SRP1 protect status writes (which we never issue); LB protects OTP.
    // QE (SR2 bit 1) is permitted: ordinary 1-1-1 commands remain valid.
    s[0] & 0x7f == if wel { 2 } else { 0 } && s[1] & 0xc4 == 0
}

pub fn within_wait(start: u32, now: u32, polls: u32) -> bool {
    now.wrapping_sub(start) < WAIT_CYCLES && polls < WAIT_POLLS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BASE;

    #[test]
    fn every_sector_is_exactly_scoped() {
        for a in (0..crate::SIZE).step_by(4096) {
            let allowed = (0x200000..0x203000).contains(&a) || a == 0x220000 || a >= 0xf00000;
            assert_eq!(erase_range(BASE + a, BASE + a), allowed.then_some(a));
            assert_eq!(erase_range(BASE + a, BASE + a + 4095), allowed.then_some(a));
            assert_eq!(erase_range(BASE + a + 1, BASE + a + 1), None);
            assert_eq!(write_range(BASE + a, 4096), allowed.then_some(a));
            assert_eq!(erase_range(BASE + a + 1, BASE + a + 4095), None);
            assert_eq!(erase_range(BASE + a, BASE + a + 4096), None);
        }
    }

    #[test]
    fn bounds_and_overflow() {
        for (a, n) in [
            (0, 1),
            (BASE, 0),
            (BASE + 0x203000, 1),
            (BASE + 0x202fff, 2),
            (BASE + 0x221000, 1),
            (BASE + 0x220fff, 2),
            (BASE + crate::SIZE - 1, 2),
            (u32::MAX, 2),
            (BASE + 0xf00000, 4097),
        ] {
            assert_eq!(write_range(a, n), None);
        }
        for (a, b) in [
            (0, u32::MAX),
            (BASE, BASE - 1),
            (u32::MAX, u32::MAX),
            (BASE + 0x200000, BASE + 0x201fff),
            (BASE + 0xf00000, BASE + 0xf00001),
            (BASE + 0xf00000, BASE + 0xf00ffe),
            (BASE + crate::SIZE, BASE + crate::SIZE),
            (0xfffff000, 0xfffff000),
        ] {
            assert_eq!(erase_range(a, b), None);
        }
        assert_eq!(
            write_range(BASE + crate::SIZE - 1, 1),
            Some(crate::SIZE - 1)
        );
        assert_eq!(write_range(BASE + 0x201fff, 2), Some(0x201fff));
    }

    #[test]
    fn measured_cube_single_sector_envelope() {
        // program-sector.log: Cube 2.24 supplies the same sector base in R0/R1.
        assert_eq!(erase_range(0x90200000, 0x90200000), Some(0x200000));
        assert_eq!(erase_range(0x90200000, 0x90200fff), Some(0x200000));
        for last in 0x90200001..0x90200fff {
            assert_eq!(erase_range(0x90200000, last), None);
        }
        assert_eq!(erase_range(0x90200000, 0x90201000), None);
        assert_eq!(erase_range(0x90200000, 0x90201fff), None);
        assert_eq!(erase_range(0x90fff000, 0x90fff000), Some(0xfff000));
        assert_eq!(erase_range(0x90fff000, 0x90ffffff), Some(0xfff000));
    }

    #[test]
    fn erase_stack_arena() {
        let end = 0x24001cac;
        assert!(erase_stack(end, 0x24002000));
        assert!(erase_stack(end, 0x2406ffe0));
        for (code, sp) in [
            (0, 0x24002000),
            (u32::MAX, 0x24002000),
            (end, end + 0xfc),
            (end, 0x24002001),
            (end, 0x2406ffe8),
            (end, 0),
            (end, u32::MAX),
        ] {
            assert!(!erase_stack(code, sp));
        }
    }

    #[test]
    fn chunks_never_wrap_pages() {
        for start in 0..256 {
            for size in [1, 255, 256, 257, 4096] {
                let mut done = 0;
                while done < size {
                    let n = page_chunk(start + done, size - done);
                    assert!((1..=256).contains(&n));
                    assert_eq!((start + done) / 256, (start + done + n - 1) / 256);
                    done += n;
                }
                assert_eq!(done, size);
            }
        }
    }

    #[test]
    fn nor_transitions_exhaustive() {
        for old in 0..=255u8 {
            for new in 0..=255u8 {
                assert_eq!(one_to_zero(old, new), new & !old == 0);
            }
        }
    }

    #[test]
    fn status_and_deadline_guards() {
        assert!(unprotected([0, 2, 0], false));
        assert!(unprotected([2, 2, 0], true));
        for bit in 0..7 {
            assert!(!unprotected([1 << bit, 2, 0], false));
        }
        for bit in [2, 6, 7] {
            assert!(!unprotected([0, 1 << bit, 0], false));
        }
        assert!(!unprotected([0, 0, 0], true));
        let start = u32::MAX - 100;
        assert!(within_wait(start, 50, 1));
        assert!(!within_wait(start, start.wrapping_add(WAIT_CYCLES), 1));
        assert!(!within_wait(start, start, WAIT_POLLS));
    }
}
