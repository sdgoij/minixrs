//! x86_64 Task State Segment — adapted from i386 `tss.h`
//!
//! **x86_64 differences from i386:**
//! - TSS is still 104 bytes (same format as Intel SDM Vol 3 for x86_64)
//! - But fields are 64-bit: RSP0/1/2, IST1-7 (vs 32-bit on i386)
//! - I/O map base address at offset 102 is still present (set >= 104 for no I/O map)
//! - TSS descriptor in GDT takes 2 entries (16 bytes) on x86_64

use core::fmt;

/// x86_64 Task State Segment (104 bytes).
///
/// Layout per Intel SDM Vol 3, Section 8.7:
/// +0x00: Reserved (u32)
/// +0x04: RSP0 (lower 32 bits)
/// +0x08: RSP0 (upper 32 bits)
/// +0x0C: RSP1 (lower 32 bits)
/// +0x10: RSP1 (upper 32 bits)
/// +0x14: RSP2 (lower 32 bits)
/// +0x18: RSP2 (upper 32 bits)
/// +0x1C: Reserved (u64)
/// +0x24: IST1 (lower 32 bits)
/// +0x28: IST1 (upper 32 bits)
/// +0x2C..0x54: IST2-7 (each as 64-bit split into lo/hi)
/// +0x64: Reserved (u64)
/// +0x6C: I/O map base (u16)
/// +0x6E: Reserved (u16)
/// Total: 104 bytes
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct Tss64 {
    pub _reserved1: u32,
    pub rsp0_lo: u32,
    pub rsp0_hi: u32,
    pub rsp1_lo: u32,
    pub rsp1_hi: u32,
    pub rsp2_lo: u32,
    pub rsp2_hi: u32,
    pub _reserved2: u64,
    pub ist1_lo: u32,
    pub ist1_hi: u32,
    pub ist2_lo: u32,
    pub ist2_hi: u32,
    pub ist3_lo: u32,
    pub ist3_hi: u32,
    pub ist4_lo: u32,
    pub ist4_hi: u32,
    pub ist5_lo: u32,
    pub ist5_hi: u32,
    pub ist6_lo: u32,
    pub ist6_hi: u32,
    pub ist7_lo: u32,
    pub ist7_hi: u32,
    pub _reserved3: u64,
    pub _reserved4: u16,
    pub io_map_base: u16,
}

impl fmt::Debug for Tss64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tss64")
            .field("rsp0", &self.rsp0())
            .field("ist1", &self.ist(1))
            .finish()
    }
}

impl Tss64 {
    /// Create a zeroed-out TSS (const-compatible for static initializers).
    #[must_use]
    pub const fn new_zeroed() -> Self {
        Self {
            _reserved1: 0,
            rsp0_lo: 0,
            rsp0_hi: 0,
            rsp1_lo: 0,
            rsp1_hi: 0,
            rsp2_lo: 0,
            rsp2_hi: 0,
            _reserved2: 0,
            ist1_lo: 0,
            ist1_hi: 0,
            ist2_lo: 0,
            ist2_hi: 0,
            ist3_lo: 0,
            ist3_hi: 0,
            ist4_lo: 0,
            ist4_hi: 0,
            ist5_lo: 0,
            ist5_hi: 0,
            ist6_lo: 0,
            ist6_hi: 0,
            ist7_lo: 0,
            ist7_hi: 0,
            _reserved3: 0,
            _reserved4: 0,
            io_map_base: 104,
        }
    }
}

impl Default for Tss64 {
    fn default() -> Self {
        Self {
            _reserved1: 0,
            rsp0_lo: 0,
            rsp0_hi: 0,
            rsp1_lo: 0,
            rsp1_hi: 0,
            rsp2_lo: 0,
            rsp2_hi: 0,
            _reserved2: 0,
            ist1_lo: 0,
            ist1_hi: 0,
            ist2_lo: 0,
            ist2_hi: 0,
            ist3_lo: 0,
            ist3_hi: 0,
            ist4_lo: 0,
            ist4_hi: 0,
            ist5_lo: 0,
            ist5_hi: 0,
            ist6_lo: 0,
            ist6_hi: 0,
            ist7_lo: 0,
            ist7_hi: 0,
            _reserved3: 0,
            _reserved4: 0,
            io_map_base: 104, // >= sizeof(Tss64) means no I/O map
        }
    }
}

impl Tss64 {
    /// Get RSP0 (kernel stack pointer on ring 0 entry).
    pub fn rsp0(&self) -> u64 {
        (self.rsp0_lo as u64) | ((self.rsp0_hi as u64) << 32)
    }

    /// Set RSP0.
    pub fn set_rsp0(&mut self, val: u64) {
        self.rsp0_lo = val as u32;
        self.rsp0_hi = (val >> 32) as u32;
    }

    /// Get an IST entry (1-indexed).
    pub fn ist(&self, n: u32) -> u64 {
        let (lo, hi) = match n {
            1 => (self.ist1_lo, self.ist1_hi),
            2 => (self.ist2_lo, self.ist2_hi),
            3 => (self.ist3_lo, self.ist3_hi),
            4 => (self.ist4_lo, self.ist4_hi),
            5 => (self.ist5_lo, self.ist5_hi),
            6 => (self.ist6_lo, self.ist6_hi),
            7 => (self.ist7_lo, self.ist7_hi),
            _ => (0, 0),
        };
        (lo as u64) | ((hi as u64) << 32)
    }

    /// Set an IST entry (1-indexed).
    pub fn set_ist(&mut self, n: u32, val: u64) {
        let lo = val as u32;
        let hi = (val >> 32) as u32;
        match n {
            1 => {
                self.ist1_lo = lo;
                self.ist1_hi = hi;
            }
            2 => {
                self.ist2_lo = lo;
                self.ist2_hi = hi;
            }
            3 => {
                self.ist3_lo = lo;
                self.ist3_hi = hi;
            }
            4 => {
                self.ist4_lo = lo;
                self.ist4_hi = hi;
            }
            5 => {
                self.ist5_lo = lo;
                self.ist5_hi = hi;
            }
            6 => {
                self.ist6_lo = lo;
                self.ist6_hi = hi;
            }
            7 => {
                self.ist7_lo = lo;
                self.ist7_hi = hi;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    #[test]
    fn test_tss_size() {
        // x86_64 TSS must be exactly 104 bytes
        assert_eq!(size_of::<Tss64>(), 104);
    }

    #[test]
    fn test_tss_rsp0() {
        let mut tss = Tss64::default();
        tss.set_rsp0(0xFFFF800010002000);
        assert_eq!(tss.rsp0(), 0xFFFF800010002000);
    }

    #[test]
    fn test_tss_ist() {
        let mut tss = Tss64::default();
        tss.set_ist(1, 0xFFFF800010003000);
        assert_eq!(tss.ist(1), 0xFFFF800010003000);
    }

    #[test]
    fn test_tss_default_io_map() {
        let tss = Tss64::default();
        // io_map_base should be >= TSS size to disable I/O map
        assert!(tss.io_map_base >= 104);
    }
}
