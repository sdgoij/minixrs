//! x86_64 Interrupt Descriptor Table (IDT).
//!
//! The IDT has 256 entries, each 16 bytes on x86_64 (8 bytes on i386).
//! Each entry is a gate descriptor that specifies the handler address,
//! code segment selector, interrupt stack table (IST) index, and
//! attributes (gate type, DPL, present bit).
//!
//! This module provides the IDT structure, a global static IDT, and
//! initialization logic to load it via `lidt`.

use core::cell::UnsafeCell;

use crate::segments::{GateDescriptor64, NIDT, RegionDescriptor64, SDT_SYS386IGT, make_idt_gate};
use core::mem::size_of;

/// The Interrupt Descriptor Table: 256 gate descriptors, 16 bytes each.
///
/// Total size: 256 × 16 = 4096 bytes.
#[repr(C, align(8))]
pub struct Idt {
    /// The 256 IDT entries.
    pub entries: [GateDescriptor64; NIDT as usize],
}

impl Idt {
    /// Create a new IDT with all entries set to missing (zeroed).
    pub const fn new() -> Self {
        // Workaround: const array initialization with a non-Copy struct.
        // We use a helper const to produce an empty gate.
        const EMPTY: GateDescriptor64 = GateDescriptor64 {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_dpl_p: 0,
            offset_mid: 0,
            offset_high: 0,
            reserved: 0,
        };
        Idt {
            entries: [EMPTY; NIDT as usize],
        }
    }

    /// Set the handler for a specific interrupt vector.
    ///
    /// # Panics
    ///
    /// Panics if `vector >= NIDT`.
    pub fn set_handler(&mut self, vector: usize, handler: u64, ist: u8, dpl: u8) {
        assert!(
            vector < NIDT as usize,
            "vector {} out of range (max {})",
            vector,
            NIDT - 1
        );
        self.entries[vector] = make_idt_gate(0x0008, handler, dpl, ist, SDT_SYS386IGT);
    }
}

impl Default for Idt {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapper for `Idt` — the IDT.
pub struct IdtCell(UnsafeCell<Idt>);
unsafe impl Sync for IdtCell {}
impl IdtCell {
    pub const fn new(val: Idt) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut Idt {
        self.0.get()
    }
}

/// The global IDT.
///
/// Initialized with all-zeros (missing) entries. Loaded during `init_idt()`.
///
/// # Safety
///
/// Mutable access must be serialized (single CPU during boot, or with
/// proper synchronization on SMP).
pub static IDT: IdtCell = IdtCell::new(Idt::new());

/// Initialize the IDT with default interrupt gates and load it via `lidt`.
///
/// Sets all 256 entries to a default 64-bit interrupt gate with:
/// - Selector: `0x0008` (kernel code segment)
/// - DPL: 0 (kernel privilege)
/// - IST: 0 (no interrupt stack)
/// - Gate type: 64-bit interrupt gate
///
/// After populating the table, executes `lidt` with the IDT pointer.
///
/// # Safety
///
/// Must be called exactly once during boot, on the BSP, in ring 0.
/// The IDT must not be in use by any other CPU when modified.
pub unsafe fn init_idt() {
    // Set a default interrupt gate for all 256 vectors.
    // The actual handlers are filled in later by the kernel's interrupt
    // management code (e.g., set_handler, register_handler).
    // SAFETY: Single-threaded during boot; no other CPU accesses IDT.
    unsafe {
        for i in 0..(NIDT as usize) {
            (*IDT.get()).entries[i] = make_idt_gate(0x0008, 0, 0, 0, SDT_SYS386IGT);
        }
    }

    // Build the 10-byte pseudo-descriptor for LIDT.
    let idtr = RegionDescriptor64 {
        limit: (size_of::<Idt>() - 1) as u16,
        base: IDT.get() as *const Idt as u64,
    };

    // Cast the RegionDescriptor64 to the raw byte slice that lidt expects.
    // The types have identical layout: u16 limit + u64 base = 10 bytes.
    // SAFETY: idtr is a valid local; lidt copies the descriptor.
    unsafe {
        crate::asm::lidt(&*(core::ptr::addr_of!(idtr) as *const [u8; 10]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of;

    // GateDescriptor64 is repr(C, packed), so we cannot take references
    // to its fields. These helpers read fields by byte offset using
    // unaligned reads.

    /// Read a u16 field from a packed GateDescriptor64 at `offset` bytes.
    ///
    /// # Safety
    ///
    /// `g` must point to a valid descriptor; `offset` must be within bounds.
    unsafe fn read_u16(g: &GateDescriptor64, offset: usize) -> u16 {
        // SAFETY: caller guarantees validity; unaligned read is fine for packed structs.
        unsafe {
            let ptr = (g as *const GateDescriptor64 as *const u8).add(offset);
            core::ptr::read_unaligned(ptr as *const u16)
        }
    }

    /// Read a u32 field at `offset` bytes.
    ///
    /// # Safety
    ///
    /// `g` must point to a valid descriptor; `offset` must be within bounds.
    unsafe fn read_u32(g: &GateDescriptor64, offset: usize) -> u32 {
        unsafe {
            let ptr = (g as *const GateDescriptor64 as *const u8).add(offset);
            core::ptr::read_unaligned(ptr as *const u32)
        }
    }

    /// Read a u8 field at `offset` bytes.
    ///
    /// # Safety
    ///
    /// `g` must point to a valid descriptor; `offset` must be within bounds.
    unsafe fn read_u8(g: &GateDescriptor64, offset: usize) -> u8 {
        unsafe {
            let ptr = (g as *const GateDescriptor64 as *const u8).add(offset);
            core::ptr::read_unaligned(ptr)
        }
    }

    // GateDescriptor64 layout (16 bytes, packed):
    //   +0: offset_low  (u16)
    //   +2: selector    (u16)
    //   +4: ist         (u8)
    //   +5: type_dpl_p  (u8)
    //   +6: offset_mid  (u16)
    //   +8: offset_high (u32)
    //  +12: reserved    (u32)

    #[test]
    fn test_idt_size() {
        // 256 entries × 16 bytes each = 4096 bytes.
        assert_eq!(size_of::<Idt>(), 4096);
    }

    #[test]
    fn test_idt_new_all_zero() {
        let idt = Idt::new();
        for i in 0..(NIDT as usize) {
            let g = &idt.entries[i];
            unsafe {
                assert_eq!(read_u16(g, 0), 0, "offset_low[{}]", i);
                assert_eq!(read_u16(g, 2), 0, "selector[{}]", i);
                assert_eq!(read_u16(g, 6), 0, "offset_mid[{}]", i);
                assert_eq!(read_u32(g, 8), 0, "offset_high[{}]", i);
            }
        }
    }

    #[test]
    fn test_idt_set_handler() {
        let mut idt = Idt::new();
        let handler: u64 = 0xFFFF800010002000;
        idt.set_handler(0x20, handler, 0, 0);

        let g = &idt.entries[0x20];
        unsafe {
            let off_low = read_u16(g, 0) as u64;
            let off_mid = read_u16(g, 6) as u64;
            let off_high = read_u32(g, 8) as u64;
            let reconstructed = off_low | (off_mid << 16) | (off_high << 32);
            assert_eq!(reconstructed, handler);

            let selector = read_u16(g, 2);
            assert_eq!(selector, 0x0008);

            let type_dpl_p = read_u8(g, 5);
            // 0x8E = present | DPL=0 | 64-bit interrupt gate(type 14).
            assert_eq!(type_dpl_p, 0x8E);
        }
    }

    #[test]
    fn test_idt_set_handler_dpl3() {
        let mut idt = Idt::new();
        // User-accessible interrupt gate: DPL=3.
        idt.set_handler(0x80, 0x1234, 0, 3);
        let g = &idt.entries[0x80];
        unsafe {
            let type_dpl_p = read_u8(g, 5);
            // 0xEE = present | DPL=3 | 64-bit interrupt gate(type 14).
            assert_eq!(type_dpl_p, 0xEE);
        }
    }

    #[test]
    fn test_idt_set_handler_with_ist() {
        let mut idt = Idt::new();
        idt.set_handler(0x0E, 0x5678, 1, 0);
        let g = &idt.entries[0x0E];
        unsafe {
            let ist = read_u8(g, 4);
            assert_eq!(ist, 1);
        }
    }

    #[test]
    fn test_idt_entry_size() {
        // Each IDT entry is 16 bytes on x86_64.
        assert_eq!(size_of::<GateDescriptor64>(), 16);
    }

    #[test]
    fn test_idt_align() {
        // The IDT must be 8-byte aligned for LIDT.
        let idt = Idt::new();
        let addr = &idt as *const Idt as usize;
        assert_eq!(addr % 8, 0, "IDT must be 8-byte aligned");
    }

    #[test]
    #[should_panic]
    fn test_set_handler_out_of_range() {
        let mut idt = Idt::new();
        idt.set_handler(256, 0, 0, 0);
    }

    #[test]
    fn test_idtr_layout() {
        // RegionDescriptor64: u16 limit + u64 base = 10 bytes.
        let _idtr = RegionDescriptor64 {
            limit: 4095,
            base: 0xFFFF800010000000,
        };
        assert_eq!(size_of::<RegionDescriptor64>(), 10);
    }

    #[test]
    fn test_default_gate_type() {
        let gate = make_idt_gate(0x0008, 0, 0, 0, SDT_SYS386IGT);
        unsafe {
            let type_dpl_p = read_u8(&gate, 5);
            assert_eq!(type_dpl_p, 0x8E);
        }
    }
}
