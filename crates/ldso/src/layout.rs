//! Where an object goes, and how much of the address space it covers.
//!
//! Pure address policy, kept out of the loader's memory access so it is
//! unit-tested on the host. [`BaseAlloc`] is the deterministic placement Phase 1
//! adds — the port has no ASLR, so the same image loads at the same addresses
//! every run, which is what makes a loader failure reproducible. [`image_extent`]
//! is the span an object occupies: it sizes that placement, and it is also what
//! bounds the addresses a relocation belonging to the object may write.

use crate::elf::{Elf, PT_LOAD};

/// Where the first `ET_DYN` object is mapped. Fixed (no ASLR), and clear of the
/// main program (`0x0100_0000`), the loader itself (`0x0400_0000`), the stack
/// (`0x0FE0_0000`) and the heap (`0x3FE0_0000`).
pub const DSO_BASE: u64 = 0x0200_0000;

/// No object is placed at or above this: the loader is at `0x0400_0000`, and an
/// object must not be mapped over it.
pub const DSO_LIMIT: u64 = 0x0400_0000;

/// Gap left after each object, so an access past one object's end faults instead
/// of landing in whatever is mapped next.
pub const DSO_GAP: u64 = 0x1_0000;

pub const fn page_down(x: u64) -> u64 {
    x & !0xFFF
}

pub const fn page_up(x: u64) -> u64 {
    (x + 0xFFF) & !0xFFF
}

/// Assigns each `DT_NEEDED` object a base: the first lands at [`DSO_BASE`] and
/// each later one above the previous object's highest mapped page, plus a gap.
pub struct BaseAlloc {
    next: u64,
}

impl BaseAlloc {
    pub const fn new() -> Self {
        Self { next: DSO_BASE }
    }

    /// Reserve `span` bytes and return the base to map an object at. `None` when
    /// the next one would reach the loader.
    ///
    /// `span` is the object's *highest* address measured from its own vaddr
    /// origin (`image_extent`'s `hi`), not its extent: the loader maps at
    /// `base + page_down(p_vaddr)`, so an object linked at a non-zero base reaches
    /// that much further past the base it was given.
    pub fn reserve(&mut self, span: u64) -> Option<u64> {
        let base = self.next;
        let end = base.checked_add(page_up(span))?.checked_add(DSO_GAP)?;
        if end > DSO_LIMIT {
            return None;
        }
        self.next = end;
        Some(base)
    }
}

impl Default for BaseAlloc {
    fn default() -> Self {
        Self::new()
    }
}

/// An object's own address extent: its lowest `PT_LOAD` page and one past its
/// highest. `None` when the image has no loadable segment.
pub fn image_extent(elf: &Elf<'_>) -> Option<(u64, u64)> {
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for i in 0..elf.e_phnum() as usize {
        let p = elf.phdr(i)?;
        if p.p_type != PT_LOAD || p.p_memsz == 0 {
            continue;
        }
        lo = lo.min(page_down(p.p_vaddr));
        hi = hi.max(page_up(p.p_vaddr + p.p_memsz));
    }
    if lo == u64::MAX { None } else { Some((lo, hi)) }
}

/// The size of thread-local storage block a `PT_TLS` of `memsz` bytes needs.
///
/// x86_64 lays TLS out backwards from the thread pointer: the pointer sits past
/// the block and a module's storage is at `tp - tls_block_size(memsz)`. The C
/// runtime's `tls_block_alloc` (`crates/minix-libc/src/lib.rs`) rounds the same
/// way, and the two must agree: a block a `pthread` allocates has to be reachable
/// through the same `__tls_get_addr` the loader wrote relocations for.
pub const fn tls_block_size(memsz: u64) -> u64 {
    (memsz + 15) & !15
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::{
        EHDR_SIZE, ELF_MAGIC, ELFCLASS64, ELFDATA2LSB, EM_X86_64, ET_DYN, PF_R, PHDR_SIZE,
    };

    fn wr16(b: &mut [u8], o: usize, v: u16) {
        b[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn wr32(b: &mut [u8], o: usize, v: u32) {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn wr64(b: &mut [u8], o: usize, v: u64) {
        b[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// The loader's block size is the runtime's, including the case where a
    /// segment's size is already a multiple of the alignment.
    #[test]
    fn tls_block_size_rounds_to_the_thread_pointers_alignment() {
        assert_eq!(tls_block_size(0), 0);
        assert_eq!(tls_block_size(1), 16);
        assert_eq!(tls_block_size(12), 16);
        assert_eq!(tls_block_size(16), 16);
        assert_eq!(tls_block_size(17), 32);
    }

    /// An `ET_DYN` header with two `PT_LOAD`s: `0x1000..0x1800` and
    /// `0x2000..0x2900`, so the extent is `0x1000..0x3000`.
    fn two_segment_image() -> Vec<u8> {
        let mut b = vec![0u8; 0x200];
        b[0..4].copy_from_slice(&ELF_MAGIC);
        b[4] = ELFCLASS64;
        b[5] = ELFDATA2LSB;
        b[6] = 1;
        wr16(&mut b, 16, ET_DYN);
        wr16(&mut b, 18, EM_X86_64);
        wr64(&mut b, 32, EHDR_SIZE as u64);
        wr16(&mut b, 54, PHDR_SIZE as u16);
        wr16(&mut b, 56, 2);
        for (i, (vaddr, memsz)) in [(0x1000u64, 0x800u64), (0x2000, 0x900)].iter().enumerate() {
            let p = EHDR_SIZE + i * PHDR_SIZE;
            wr32(&mut b, p, PT_LOAD);
            wr32(&mut b, p + 4, PF_R);
            wr64(&mut b, p + 16, *vaddr);
            wr64(&mut b, p + 40, *memsz);
        }
        b
    }

    #[test]
    fn the_first_object_lands_at_the_base_and_the_next_above_it() {
        let mut a = BaseAlloc::new();
        assert_eq!(a.reserve(0x3000), Some(DSO_BASE));
        let second = a.reserve(0x2000).expect("room for a second object");
        assert_eq!(second, DSO_BASE + page_up(0x3000) + DSO_GAP);
        // Separated by the gap, so a relocation belonging to one object cannot
        // reach into the other's span.
        assert!(second >= DSO_BASE + 0x3000 + DSO_GAP);
    }

    #[test]
    fn placement_is_deterministic() {
        let run = || {
            let mut a = BaseAlloc::new();
            [a.reserve(0x1000), a.reserve(0x1000), a.reserve(0x1000)]
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn an_object_that_would_reach_the_loader_is_refused() {
        let mut a = BaseAlloc::new();
        assert_eq!(a.reserve(DSO_LIMIT - DSO_BASE), None);
        // A refusal does not consume the space it was asked for.
        assert_eq!(a.reserve(0x1000), Some(DSO_BASE));
    }

    #[test]
    fn the_space_between_the_objects_and_the_loader_runs_out() {
        let mut a = BaseAlloc::new();
        let mut n = 0;
        let mut last = 0;
        while let Some(b) = a.reserve(0x1000) {
            last = b;
            n += 1;
        }
        assert!(
            n > 100,
            "got {n} small objects; expected the space to hold many"
        );
        assert!(last < DSO_LIMIT);
    }

    #[test]
    fn page_helpers_round_the_right_way() {
        assert_eq!(page_down(0x1234), 0x1000);
        assert_eq!(page_up(0x1234), 0x2000);
        assert_eq!(page_up(0x1000), 0x1000);
        assert_eq!(page_up(0), 0);
    }

    #[test]
    fn image_extent_spans_the_load_segments() {
        let b = two_segment_image();
        let e = Elf::new(&b).unwrap();
        assert_eq!(image_extent(&e), Some((0x1000, 0x3000)));
    }

    #[test]
    fn an_image_with_no_load_segment_has_no_extent() {
        let mut b = two_segment_image();
        wr64(&mut b, EHDR_SIZE + 40, 0);
        wr64(&mut b, EHDR_SIZE + PHDR_SIZE + 40, 0);
        let e = Elf::new(&b).unwrap();
        assert_eq!(image_extent(&e), None);
    }
}
