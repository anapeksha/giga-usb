#![forbid(unsafe_code)]
//! Bounded report-protocol keyboard/mouse subset, not an arbitrary HID interpreter.
use heapless::Vec;

#[derive(Clone, Copy, Default)]
struct Global {
    page: u32,
    size: u32,
    count: u32,
    id: u8,
    signed: bool,
    logical_min: i64,
    logical_max: i64,
}
#[derive(Clone, Copy)]
struct Field {
    report_id: u8,
    offset: u32,
    size: u32,
    page: u32,
    usage: u32,
    variable: bool,
    relative: bool,
    signed: bool,
}
struct Report {
    id: u8,
    bits: u32,
}
/// Parsed input layout with 64 fields total across at most eight reports.
/// Supports a bounded keyboard/button/relative-axis subset, not arbitrary HID.
pub struct Layout {
    reports: Vec<Report, 8>,
    fields: Vec<Field, 64>,
    numbered: bool,
}
pub struct Decoded {
    pub keys: Vec<u8, 6>,
    pub modifiers: u8,
    pub keyboard: bool,
    pub mouse: bool,
    pub buttons: u8,
    pub axes: [i8; 3],
    pub id: u8,
}
impl Layout {
    /// Parse up to 1024 descriptor bytes, rejecting malformed or unsupported items
    /// and layouts exceeding fixed report, field, or nesting budgets.
    pub fn parse(data: &[u8]) -> Result<Self, &'static str> {
        if data.is_empty() || data.len() > 1024 {
            return Err("Invalid HID descriptor length");
        }
        let mut layout = Self {
            reports: Vec::new(),
            fields: Vec::new(),
            numbered: false,
        };
        let mut g = Global::default();
        let mut stack = Vec::<Global, 4>::new();
        let mut usages = Vec::<u32, 64>::new();
        let mut minimum = None;
        let mut maximum = None;
        let mut depth = 0u8;
        let mut pos = 0;
        while pos < data.len() {
            let prefix = data[pos];
            pos += 1;
            if prefix == 0xfe {
                return Err("Unsupported HID long item");
            }
            let size = match prefix & 3 {
                3 => 4,
                n => usize::from(n),
            };
            let bytes = data.get(pos..pos + size).ok_or("Truncated HID item")?;
            pos += size;
            let mut value = 0u32;
            for (i, byte) in bytes.iter().enumerate() {
                value |= u32::from(*byte) << (i * 8);
            }
            let tag = prefix >> 4;
            match (prefix >> 2 & 3, tag) {
                (1, 0) => g.page = value,
                (1, 1) => {
                    g.signed = size != 0 && bytes[size - 1] & 0x80 != 0;
                    g.logical_min = if g.signed {
                        i64::from((value << (32 - size * 8)) as i32 >> (32 - size * 8))
                    } else {
                        i64::from(value)
                    };
                }
                (1, 2) => {
                    g.logical_max = if g.signed && size != 0 {
                        i64::from((value << (32 - size * 8)) as i32 >> (32 - size * 8))
                    } else {
                        i64::from(value)
                    };
                }
                (1, 7) => {
                    if value > 32 {
                        return Err("HID field wider than 32 bits");
                    }
                    g.size = value;
                }
                (1, 8) => {
                    g.id = u8::try_from(value).map_err(|_| "Invalid HID report ID")?;
                    if g.id == 0 {
                        return Err("Zero HID report ID");
                    }
                    layout.numbered = true;
                }
                (1, 9) => g.count = value,
                (1, 10) => {
                    if size != 0 {
                        return Err("Malformed HID push");
                    }
                    stack.push(g).map_err(|_| "HID global stack full")?;
                }
                (1, 11) => {
                    if size != 0 {
                        return Err("Malformed HID pop");
                    }
                    g = stack.pop().ok_or("HID global stack underflow")?;
                }
                (2, 0) => {
                    usages
                        .push(if size == 4 {
                            value
                        } else {
                            (g.page << 16) | value
                        })
                        .map_err(|_| "Too many HID usages")?;
                }
                (2, 1) => {
                    minimum = Some(if size == 4 {
                        value
                    } else {
                        (g.page << 16) | value
                    })
                }
                (2, 2) => {
                    maximum = Some(if size == 4 {
                        value
                    } else {
                        (g.page << 16) | value
                    })
                }
                (2, _) => return Err("Unsupported HID local item"),
                (0, _) => {
                    if minimum.is_some() != maximum.is_some()
                        || minimum
                            .zip(maximum)
                            .is_some_and(|(a, b)| a > b || a >> 16 != b >> 16)
                    {
                        return Err("Invalid HID usage range");
                    }
                    match tag {
                        8 => {
                            if g.size == 0 || g.count == 0 {
                                return Err("Empty HID input");
                            }
                            let bits = g
                                .size
                                .checked_mul(g.count)
                                .ok_or("HID bit count overflow")?;
                            let index =
                                if let Some(i) = layout.reports.iter().position(|r| r.id == g.id) {
                                    i
                                } else {
                                    layout
                                        .reports
                                        .push(Report { id: g.id, bits: 0 })
                                        .map_err(|_| "Too many HID reports")?;
                                    layout.reports.len() - 1
                                };
                            let report = &mut layout.reports[index];
                            let end = report.bits.checked_add(bits).ok_or("HID report overflow")?;
                            if end > 512 {
                                return Err("HID report exceeds packet limit");
                            }
                            if value & 1 == 0 {
                                if value & 2 == 0 && g.page == 7 {
                                    // Array values are selectors, not necessarily usage IDs.
                                    // Only identity ranges are supported by this subset.
                                    if !usages.is_empty()
                                        || !minimum.zip(maximum).is_some_and(|(a, b)| {
                                            a >> 16 == 7
                                                && b >> 16 == 7
                                                && i64::from(a & 0xffff) == g.logical_min
                                                && i64::from(b & 0xffff) == g.logical_max
                                        })
                                    {
                                        return Err("Nonidentity HID keyboard array unsupported");
                                    }
                                }
                                for i in 0..g.count {
                                    let usage = usages
                                        .get(i as usize)
                                        .copied()
                                        .or_else(|| {
                                            minimum
                                                .zip(maximum)
                                                .map(|(a, b)| a.saturating_add(i).min(b))
                                        })
                                        .or_else(|| usages.last().copied())
                                        .unwrap_or(g.page << 16);
                                    layout
                                        .fields
                                        .push(Field {
                                            report_id: g.id,
                                            offset: report.bits + i * g.size,
                                            size: g.size,
                                            page: usage >> 16,
                                            usage: usage & 0xffff,
                                            variable: value & 2 != 0,
                                            relative: value & 4 != 0,
                                            signed: g.signed,
                                        })
                                        .map_err(|_| "Too many HID fields")?;
                                }
                            }
                            report.bits = end;
                        }
                        10 => depth = depth.checked_add(1).ok_or("HID collection overflow")?,
                        12 => {
                            if size != 0 {
                                return Err("Malformed HID collection end");
                            }
                            depth = depth.checked_sub(1).ok_or("HID collection underflow")?;
                        }
                        9 | 11 => {}
                        _ => return Err("Unsupported HID main item"),
                    }
                    usages.clear();
                    minimum = None;
                    maximum = None;
                }
                (1, 3..=6) => {}
                _ => return Err("Unsupported HID item"),
            }
        }
        if depth != 0
            || !stack.is_empty()
            || !usages.is_empty()
            || minimum.is_some()
            || maximum.is_some()
            || layout.reports.is_empty()
            || (layout.numbered && layout.reports.iter().any(|r| r.id == 0))
        {
            return Err("Incomplete HID descriptor");
        }
        Ok(layout)
    }
    /// Decode one exact-length input report, including its ID byte when numbered.
    /// Reject unknown IDs, rollover, more than six keys, absolute axes, and axis
    /// values outside `i8`; unsupported semantics are not a generic HID fallback.
    pub fn decode(&self, packet: &[u8]) -> Result<Decoded, &'static str> {
        let (id, data) = if self.numbered {
            (
                *packet.first().ok_or("Missing HID report ID")?,
                &packet[1..],
            )
        } else {
            (0, packet)
        };
        let report = self
            .reports
            .iter()
            .find(|r| r.id == id)
            .ok_or("Unknown HID report ID")?;
        if data.len() != report.bits.div_ceil(8) as usize {
            return Err("Invalid HID report length");
        }
        let mut out = Decoded {
            keys: Vec::new(),
            modifiers: 0,
            keyboard: false,
            mouse: false,
            buttons: 0,
            axes: [0; 3],
            id,
        };
        for f in self.fields.iter().filter(|f| f.report_id == id) {
            let mut raw = 0u32;
            for bit in 0..f.size {
                let at = f.offset + bit;
                raw |= u32::from((data[at as usize / 8] >> (at % 8)) & 1) << bit;
            }
            if f.page == 7 {
                out.keyboard = true;
                let key = if f.variable {
                    if raw == 0 {
                        continue;
                    }
                    f.usage
                } else {
                    raw
                };
                if (1..=3).contains(&key) {
                    return Err("HID keyboard rollover");
                }
                if (224..=231).contains(&key) {
                    out.modifiers |= 1 << (key - 224);
                } else if key != 0 {
                    let key = u8::try_from(key).map_err(|_| "Unsupported HID key")?;
                    if !out.keys.contains(&key) {
                        out.keys.push(key).map_err(|_| "More than six HID keys")?;
                    }
                }
            } else if f.page == 9 && f.variable && (1..=8).contains(&f.usage) {
                out.mouse = true;
                if raw != 0 {
                    out.buttons |= 1 << (f.usage - 1);
                }
            } else if f.page == 1 && f.variable && matches!(f.usage, 0x30 | 0x31 | 0x38) {
                if !f.relative {
                    return Err("Absolute HID axes unsupported");
                }
                let signed = if f.signed {
                    ((raw << (32 - f.size)) as i32) >> (32 - f.size)
                } else {
                    i32::try_from(raw).map_err(|_| "HID axis overflow")?
                };
                let axis = match f.usage {
                    0x30 => 0,
                    0x31 => 1,
                    _ => 2,
                };
                out.axes[axis] = i8::try_from(signed).map_err(|_| "HID axis exceeds UI range")?;
                out.mouse = true;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MOUSE: &[u8] = &[
        0x05, 1, 0x09, 2, 0xa1, 1, 0x09, 1, 0xa1, 0, 0x05, 9, 0x19, 1, 0x29, 3, 0x15, 0, 0x25, 1,
        0x95, 3, 0x75, 1, 0x81, 2, 0x95, 1, 0x75, 5, 0x81, 1, 0x05, 1, 0x09, 0x30, 0x09, 0x31,
        0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 8, 0x95, 3, 0x81, 6, 0xc0, 0xc0,
    ];
    const KEYBOARD: &[u8] = &[
        0x05, 1, 0x09, 6, 0xa1, 1, 0x85, 7, 0x05, 7, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0, 0x25, 1,
        0x75, 1, 0x95, 8, 0x81, 2, 0x75, 8, 0x95, 1, 0x81, 1, 0x19, 0, 0x29, 0x65, 0x15, 0, 0x25,
        0x65, 0x75, 8, 0x95, 6, 0x81, 0, 0xc0,
    ];
    #[test]
    fn layout_memory_and_shared_field_budget() {
        extern crate std;
        std::println!(
            "Layout: {} bytes; Report: {} bytes; Field: {} bytes",
            core::mem::size_of::<Layout>(),
            core::mem::size_of::<Report>(),
            core::mem::size_of::<Field>()
        );
        assert!(core::mem::size_of::<Layout>() <= 1536);
        assert!(core::mem::size_of::<Report>() <= 8);
        let mut descriptor = Vec::<u8, 128>::new();
        descriptor
            .extend_from_slice(&[0x05, 7, 0x09, 4, 0x75, 1, 0x95, 32])
            .unwrap();
        for id in 1..=3 {
            descriptor.extend_from_slice(&[0x85, id, 0x81, 2]).unwrap();
        }
        assert!(matches!(
            Layout::parse(&descriptor),
            Err("Too many HID fields")
        ));
    }
    #[test]
    fn nonidentity_keyboard_array_is_rejected() {
        let descriptor = [
            0x05, 7, 0x15, 1, 0x25, 2, 0x19, 4, 0x29, 5, 0x75, 8, 0x95, 1, 0x81, 0,
        ];
        assert!(matches!(
            Layout::parse(&descriptor),
            Err("Nonidentity HID keyboard array unsupported")
        ));
    }
    #[test]
    fn standard_mouse_signed_axes_and_lengths() {
        let layout = Layout::parse(MOUSE).unwrap();
        let report = layout.decode(&[5, 255, 128, 127]).unwrap();
        assert!(report.mouse);
        assert_eq!(report.buttons, 5);
        assert_eq!(report.axes, [-1, -128, 127]);
        assert!(layout.decode(&[0; 3]).is_err());
        assert!(layout.decode(&[0; 5]).is_err());
    }
    #[test]
    fn report_id_keyboard_modifiers_arrays_and_rollover() {
        let layout = Layout::parse(KEYBOARD).unwrap();
        let report = layout.decode(&[7, 2, 0, 4, 5, 0, 0, 0, 0]).unwrap();
        assert!(report.keyboard);
        assert_eq!(report.modifiers, 2);
        assert_eq!(report.keys.as_slice(), &[4, 5]);
        assert!(layout.decode(&[8, 0, 0, 0, 0, 0, 0, 0, 0]).is_err());
        assert!(layout.decode(&[7, 0, 0, 1, 0, 0, 0, 0, 0]).is_err());
    }
    #[test]
    fn malformed_truncated_and_bounded() {
        for data in [
            &[0xb4][..],
            &[0xa4, 0xa4, 0xa4, 0xa4, 0xa4],
            &[0x75, 33],
            &[0x77, 1],
            &[0xc0],
            &[0x85, 0],
            &[0x75, 32, 0x97, 255, 255, 255, 255, 0x81, 2],
        ] {
            assert!(Layout::parse(data).is_err());
        }
        for end in 0..MOUSE.len() {
            assert!(Layout::parse(&MOUSE[..end]).is_err());
        }
    }
}
