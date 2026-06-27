//! x86_64 segment descriptors — adapted from i386 `segments.h`
//!
//! **x86_64 differences from i386:**
//! - GDT descriptor format is the same 8 bytes, but for long mode:
//!   - Code segments: L=1, D/B=0 (required by Intel SDM)
//!   - Data segments: granularity/limit bits ignored in 64-bit mode
//! - IDT gate descriptors: **16 bytes** on x86_64 (8 bytes on i386).
//!   The 64-bit handler offset is split across low bits 16-31 and
//!   high bits 32-63 in a second qword.
//! - Region descriptor for LGDT/LIDT: 16-bit limit + **64-bit base**
//!   (i386: 16-bit limit + 32-bit base)
//! - GDT selector layout: same (index << 3 | RPL)
//! - No VM86 mode support needed

// ── Selector manipulation ───────────────────────────────────────────────

pub const SEL_KPL: u16 = 0;
pub const SEL_UPL: u16 = 3;
pub const SEL_RPL: u16 = 3;
pub const SEL_LDT: u16 = 4;

pub const fn ispl(s: u16) -> u16 {
    s & SEL_RPL
}
pub const fn isldt(s: u16) -> bool {
    (s & SEL_LDT) != 0
}
pub const fn idxsel(s: u16) -> u16 {
    (s >> 3) & 0x1fff
}
pub const fn gsel(s: u16, r: u16) -> u16 {
    (s << 3) | r
}
pub const fn gsyssel(s: u16, r: u16) -> u16 {
    gsel(s, r)
}

pub const fn usermode(cs: u16) -> bool {
    ispl(cs) == SEL_UPL
}
pub const fn kernelmode(cs: u16) -> bool {
    ispl(cs) == SEL_KPL
}

// ── GDT entry structure (8 bytes, same as i386) ─────────────────────────

/// 8-byte GDT segment descriptor.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SegmentDescriptor {
    pub sd_lolimit: u16,
    pub sd_lobase: u16,
    pub sd_midbase: u8,
    pub sd_type: u8,  // type(4) | S(1) | DPL(2) | P(1)
    pub sd_flags: u8, // limit_hi(4) | flags(4): G, D/B, L, AVL
    pub sd_hibase: u8,
}

/// Create a code segment descriptor for x86_64 long mode.
pub const fn code64_descriptor(dpl: u8, present: bool) -> SegmentDescriptor {
    let p = if present { 0x80u8 } else { 0x00u8 };
    let type_byte = 0x1A | (dpl << 5) | p;
    let flags_byte = 0xA0; // G=1, D/B=0, L=1
    SegmentDescriptor {
        sd_lolimit: 0,
        sd_lobase: 0,
        sd_midbase: 0,
        sd_type: type_byte,
        sd_flags: flags_byte,
        sd_hibase: 0,
    }
}

/// Create a data segment descriptor for x86_64.
pub const fn data64_descriptor(dpl: u8, present: bool) -> SegmentDescriptor {
    let p = if present { 0x80u8 } else { 0x00u8 };
    let type_byte = 0x12 | (dpl << 5) | p;
    let flags_byte = 0xC0; // G=1, D/B=1, L=0
    SegmentDescriptor {
        sd_lolimit: 0,
        sd_lobase: 0,
        sd_midbase: 0,
        sd_type: type_byte,
        sd_flags: flags_byte,
        sd_hibase: 0,
    }
}

// ── 16-byte IDT gate descriptor (x86_64 only) ───────────────────────────

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct GateDescriptor64 {
    pub offset_low: u16,
    pub selector: u16,
    pub ist: u8,
    pub type_dpl_p: u8,
    pub offset_mid: u16,
    pub offset_high: u32,
    pub reserved: u32,
}

/// Build a 64-bit IDT gate descriptor.
pub const fn make_idt_gate(
    selector: u16,
    offset: u64,
    dpl: u8,
    ist: u8,
    typ: u8,
) -> GateDescriptor64 {
    let p = 0x80u8;
    let type_dpl_p = typ | (dpl << 5) | p;
    GateDescriptor64 {
        offset_low: (offset & 0xFFFF) as u16,
        selector,
        ist: ist & 0x07,
        type_dpl_p,
        offset_mid: ((offset >> 16) & 0xFFFF) as u16,
        offset_high: ((offset >> 32) & 0xFFFFFFFF) as u32,
        reserved: 0,
    }
}

/// Extract the handler address from a 64-bit IDT gate.
pub fn gate_offset(g: &GateDescriptor64) -> u64 {
    let off_low = g.offset_low as u64;
    let off_mid = g.offset_mid as u64;
    let off_high = g.offset_high as u64;
    off_low | (off_mid << 16) | (off_high << 32)
}

// ── Region descriptor (for LGDT/LIDT) — x86_64 has 64-bit base ──────────

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct RegionDescriptor64 {
    pub limit: u16,
    pub base: u64,
}

// ── GDT selector indices ───────────────────────────────────────────────

pub const GNULL_SEL: u16 = 0;
pub const GCODE_SEL: u16 = 1;
pub const GDATA_SEL: u16 = 2;
pub const GUCODE_SEL: u16 = 3;
pub const GUDATA_SEL: u16 = 4;
pub const GLDT_SEL: u16 = 5;
pub const GCPU_SEL: u16 = 6;
pub const GTSS_SEL: u16 = 7;
pub const GUSERFS_SEL: u16 = 8;
pub const GUSERGS_SEL: u16 = 9;

/// On x86_64, a TSS descriptor takes 2 GDT entries (16 bytes).
pub const NGDT: u16 = 16;

// ── IDT constants ─────────────────────────────────────────────────────

pub const NIDT: u16 = 256;
pub const NRSVIDT: u16 = 32;

// ── Gate types ──────────────────────────────────────────────────────────

pub const SDT_SYS386TSS: u8 = 9;
pub const SDT_SYS386BSY: u8 = 11;
pub const SDT_SYS386IGT: u8 = 14;
pub const SDT_SYS386TGT: u8 = 15;
pub const SDT_SYSNULL: u8 = 0;
pub const SDT_SYSLDT: u8 = 2;

pub const SDT_MEMRO: u8 = 16;
pub const SDT_MEMRW: u8 = 18;
pub const SDT_MEME: u8 = 24;
pub const SDT_MEMER: u8 = 26;

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_gdt_selector() {
        assert_eq!(gsel(GCODE_SEL, SEL_KPL), 0x08);
        assert_eq!(gsel(GUCODE_SEL, SEL_UPL), 0x1B);
        assert_eq!(gsel(GUDATA_SEL, SEL_UPL), 0x23);
    }

    #[test]
    fn test_kernel_code_descriptor() {
        let d = code64_descriptor(0, true);
        assert_eq!(d.sd_type, 0x9A);
        assert_eq!(d.sd_flags, 0xA0);
    }

    #[test]
    fn test_user_code_descriptor() {
        let d = code64_descriptor(3, true);
        assert_eq!(d.sd_type, 0xFA);
    }

    #[test]
    fn test_data_descriptor() {
        let d = data64_descriptor(0, true);
        assert_eq!(d.sd_type, 0x92);
    }

    #[test]
    fn test_idt_gate_size() {
        assert_eq!(size_of::<GateDescriptor64>(), 16);
    }

    #[test]
    fn test_make_idt_gate() {
        let handler: u64 = 0xFFFF800010002000;
        let g = make_idt_gate(0x08, handler, 0, 0, SDT_SYS386IGT);
        let sel = g.selector;
        assert_eq!(sel, 0x08);
        assert_eq!(gate_offset(&g), handler);
    }

    #[test]
    fn test_region_descriptor_size() {
        assert!(size_of::<RegionDescriptor64>() >= 10);
    }

    #[test]
    fn test_segment_descriptor_size() {
        assert_eq!(size_of::<SegmentDescriptor>(), 8);
    }

    #[test]
    fn test_gdt_indices() {
        assert_eq!(gsel(GNULL_SEL, SEL_KPL), 0x00);
        assert_eq!(gsel(GCODE_SEL, SEL_KPL), 0x08);
        assert_eq!(gsel(GDATA_SEL, SEL_KPL), 0x10);
    }
}
