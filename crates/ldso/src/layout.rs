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

/// The union of the `p_flags` of every `PT_LOAD` whose page range covers the page
/// segment `i` starts on, or `None` when index `i` is not a loadable segment.
///
/// Each segment is mapped over its *page-rounded* extent, and ELF lets adjacent
/// segments share the page where one ends and the next begins — a `.text` whose end
/// is not page-aligned runs into the following `.rodata`. A page carries one
/// protection and VM gives it that of the region that owns it, which is the later
/// segment's, so the later one has to be mapped with the union or the earlier one's
/// permissions on that page are lost with it. Where the execute bit is enforced
/// that is not cosmetic: the shared page holds the tail of `.text` — the `.plt` —
/// and a fetch from it would fault on every retry. (VM's region carve is the other
/// half: it keeps an earlier region's own pages rather than dropping it.)
///
/// VM's `do_vfs_mmap` path computes the same union for an `exec`ed image
/// (`crates/servers/src/vfs/exec.rs`), which is also where the pre-fault that its
/// data regions need is asked for; a DSO's pages are demand-paged here.
pub fn shared_page_flags(elf: &Elf<'_>, i: usize) -> Option<u32> {
    let p = elf.phdr(i)?;
    if p.p_type != PT_LOAD || p.p_memsz == 0 {
        return None;
    }
    let page = page_down(p.p_vaddr);
    let mut flags = 0;
    for j in 0..elf.e_phnum() as usize {
        let q = elf.phdr(j)?;
        if q.p_type != PT_LOAD || q.p_memsz == 0 {
            continue;
        }
        if page >= page_down(q.p_vaddr) && page < page_up(q.p_vaddr + q.p_memsz) {
            flags |= q.p_flags;
        }
    }
    Some(flags)
}

/// The size of thread-local storage block a `PT_TLS` of `memsz` bytes needs.
///
/// x86_64 lays TLS out backwards from the thread pointer: the pointer sits past
/// the block and a module's storage is at `tp - tls_block_size(memsz)`. The C
/// runtime's `tls_block_alloc` (`crates/minix-libc/src/lib.rs`) rounds the same
/// way, and the two must agree: a block a `pthread` allocates has to be reachable
/// through the same `__tls_get_addr` the loader wrote relocations for.
///
/// aarch64 and riscv64 use the other variant — the pointer *is* the block's
/// first byte — so the rounding only matters for the x86_64 placement.
pub const fn tls_block_size(memsz: u64) -> u64 {
    (memsz + 15) & !15
}

/// How many thread-local modules a process can hold: the ones a program starts with, and a
/// surplus for the ones it loads later.
///
/// The surplus is what makes a module loaded after startup possible at all. Every thread's
/// block is allocated once, big enough for all of these, so a later module takes a reserved
/// slice and **nothing moves** — no other thread's offsets change, and there is no per-thread
/// table to build or grow. A module past this count is refused rather than placed over its
/// neighbour's storage.
pub const TLS_SLOTS: usize = 8;

/// The room a surplus slot gets, in bytes, before rounding.
///
/// A module whose `PT_TLS` needs more than this cannot be loaded at run time — it can still be
/// a *startup* dependency, which is placed at its real size — and the loader refuses it rather
/// than placing it over its neighbour.
pub const TLS_SURPLUS_SLOT: u64 = 2048;

/// Where each module's storage starts, relative to the thread pointer.
///
/// `tp + disp[i]` is module `i`'s first byte, so a lookup is `tp + disp[module] + offset`.
/// The offsets an object's own code carries need no adjustment for this: they are relative to
/// the module's own storage already, which is what the DTV pointer in a system loader is
/// pointing at.
pub struct TlsLayout {
    /// One displacement per slot, in module order.
    pub disp: [i64; TLS_SLOTS],
    /// Where a thread's generation table starts: one word per slot, holding the serial of the
    /// module whose copy is in that slot. It is part of the same block, beyond the modules, and
    /// it is what lets a thread that existed *before* a module was loaded initialise that
    /// module's slice when it first touches it (`rtld.rs::__tls_get_addr`).
    pub table: i64,
    /// Total bytes a thread's block needs, surplus and generation table included.
    pub block: u64,
}

/// Lay `sizes` out as thread-local modules, [`TLS_SLOTS`] slots in all.
///
/// `sizes[i]` is module `i`'s `p_memsz`; every slot past the live modules gets
/// [`TLS_SURPLUS_SLOT`] instead, which is what reserves room for a `dlopen`'d module.
///
/// The two thread-pointer conventions put the modules on opposite sides of the pointer.
/// x86_64's pointer is past the whole block, so module 0's storage ends at it and the rest sit
/// below (`disp` negative, and `disp[0] == -tls_block_size(sizes[0])` — the placement the
/// single-module loader has always used). aarch64 and riscv64 point at the block's first byte,
/// so module 0 starts there (`disp[0] == 0`) and the rest follow it. Either way a module's
/// displacement depends only on the sizes ahead of it and never on the thread, which is the
/// whole reason a lookup can be a displacement rather than a per-thread table.
pub const fn tls_layout(sizes: &[u64]) -> TlsLayout {
    let mut disp = [0i64; TLS_SLOTS];
    let mut used = 0u64;
    let mut i = 0;
    while i < TLS_SLOTS {
        let size = tls_block_size(if i < sizes.len() {
            sizes[i]
        } else {
            TLS_SURPLUS_SLOT
        });
        used += size;
        // Past the pointer on x86_64 (so the module ends where the pointer is), at the start
        // of the block elsewhere.
        disp[i] = if TP_IS_PAST_THE_BLOCK {
            -(used as i64)
        } else {
            (used - size) as i64
        };
        i += 1;
    }
    // The generation words go beyond the last module, on the same side of the pointer as they
    // are: below every module on x86_64, above them elsewhere.
    let words = tls_block_size((TLS_SLOTS * 8) as u64);
    let table = if TP_IS_PAST_THE_BLOCK {
        -(used as i64) - words as i64
    } else {
        used as i64
    };
    TlsLayout {
        disp,
        table,
        block: used + words,
    }
}

/// Whether the thread pointer sits past the block (`x86_64`) or at its start
/// (`aarch64`, `riscv64`).
///
/// This is the one part of the TLS ABI the loader has to get right per target,
/// and the port already has both halves: `minix_libc`'s `tls_block_alloc` picks
/// the pointer the same way, and its comment says why — "x86_64 uses the
/// negative-offset TLS layout (TP past the end of the image, 16-aligned,
/// self-pointer at [TP]); aarch64/riscv64 point at the block start". The loader
/// and the runtime must agree, or a thread's storage is reached through one
/// convention and allocated for the other.
#[cfg(target_arch = "x86_64")]
pub const TP_IS_PAST_THE_BLOCK: bool = true;
#[cfg(not(target_arch = "x86_64"))]
pub const TP_IS_PAST_THE_BLOCK: bool = false;

/// What a `tls_index`'s offset word is short by, on the target that is short by
/// anything.
///
/// The offset word is the variable's distance from its *module's* storage, and on
/// x86_64 and aarch64 it is exactly that. RISC-V's psABI states its `DTPREL` as
/// `TPREL - DTP_OFFSET`, so the same word there is `0x800` less than the distance —
/// which is why LLD's riscv64 target has `const uint64_t dtpOffset = 0x800` and
/// writes `val - dtpOffset` for `R_RISCV_TLS_DTPREL64`
/// (`lld/ELF/Arch/RISCV.cpp`). Nothing else in that object compensates: the code
/// around the call is `auipc`/`addi` to the `tls_index`, `jal __tls_get_addr`,
/// then a load at offset zero of what came back, so the whole of the bias has to
/// be added back by whoever reads the word.
///
/// The loader is that reader, and it is also what writes the storage the word is
/// about — its own init copy puts the module's image at `tp + disp`, unbiased. So
/// this constant is the difference between the two halves agreeing, and leaving it
/// out is not a wrong pointer but a silent one: on riscv64 a thread would read the
/// 2048 bytes below its module's storage, which is where the same bug made `errno`
/// look correct — its writer and its reader were both offset the same way, so the
/// value round-tripped, while a `PT_TLS` image copied by the loader did not.
#[cfg(target_arch = "riscv64")]
pub const DTV_OFFSET: i64 = 0x800;
#[cfg(not(target_arch = "riscv64"))]
pub const DTV_OFFSET: i64 = 0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::elf::{
        EHDR_SIZE, ELF_MAGIC, ELFCLASS64, ELFDATA2LSB, EM_X86_64, ET_DYN, PF_R, PF_X, PHDR_SIZE,
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

    /// A module goes beside the ones before it, and the first one stays where the single
    /// module has always been — which is what keeps the loader's own placement of `libc.so`
    /// unchanged by there being room for more.
    #[test]
    fn a_module_is_placed_beside_the_ones_before_it() {
        let one = tls_layout(&[32]);
        let two = tls_layout(&[32, 64]);
        if TP_IS_PAST_THE_BLOCK {
            assert_eq!(one.disp[0], -32);
            assert_eq!(two.disp[0], -32);
            assert_eq!(two.disp[1], -96);
        } else {
            assert_eq!(one.disp[0], 0);
            assert_eq!(two.disp[0], 0);
            assert_eq!(two.disp[1], 32);
        }
        // The block is [`TLS_SLOTS`] slots either way, and a module that is present replaces
        // the surplus room one would have reserved — so the block follows the modules a
        // program actually starts with, and does not grow when one of them is `dlopen`'d.
        let gens = tls_block_size((TLS_SLOTS * 8) as u64);
        assert_eq!(
            one.block,
            32 + (TLS_SLOTS - 1) as u64 * TLS_SURPLUS_SLOT + gens
        );
        assert_eq!(
            two.block,
            32 + 64 + (TLS_SLOTS - 2) as u64 * TLS_SURPLUS_SLOT + gens
        );
    }

    /// Every slot — live module or reserved surplus — lies inside the one block a thread
    /// allocates, and the slots tile it without overlapping. That is the property that lets a
    /// `dlopen` fill a surplus slot when it arrives: the room was already bought, so no other
    /// thread's storage moves.
    #[test]
    fn every_slot_lies_inside_the_block() {
        let l = tls_layout(&[32, 64]);
        for i in 0..TLS_SLOTS {
            let size = if i == 0 {
                32
            } else if i == 1 {
                64
            } else {
                TLS_SURPLUS_SLOT
            };
            // Where the slot begins, measured from the block's first byte.
            let off = if TP_IS_PAST_THE_BLOCK {
                (l.block as i64 + l.disp[i]) as u64
            } else {
                l.disp[i] as u64
            };
            assert!(off + size <= l.block, "slot {i} runs past the block");
        }
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

    /// The page a segment starts on can be an earlier segment's too, and the union
    /// of the two is what it must be mapped with: the loader passes this as the
    /// segment's protection, so the page keeps every bit either segment asked for.
    #[test]
    fn a_shared_page_carries_the_union_of_both_segments_flags() {
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
        // `.text` ending mid-page, then `.rodata` beginning on that same page —
        // the shape a linker actually emits, and the one the union is for.
        let segs = [(0x1000u64, 0x700u64, PF_R | PF_X), (0x1700, 0x400, PF_R)];
        for (i, (vaddr, memsz, flags)) in segs.iter().enumerate() {
            let p = EHDR_SIZE + i * PHDR_SIZE;
            wr32(&mut b, p, PT_LOAD);
            wr32(&mut b, p + 4, *flags);
            wr64(&mut b, p + 16, *vaddr);
            wr64(&mut b, p + 40, *memsz);
        }
        let e = Elf::new(&b).unwrap();
        assert_eq!(shared_page_flags(&e, 0), Some(PF_R | PF_X));
        assert_eq!(
            shared_page_flags(&e, 1),
            Some(PF_R | PF_X),
            "the page holds the tail of the first segment, so it needs its X"
        );
        // A segment the flags are not a property of is not a loadable one.
        assert_eq!(shared_page_flags(&e, 2), None);
    }
}
