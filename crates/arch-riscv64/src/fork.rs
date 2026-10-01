//! Release of a partially built child page table, for a fork that fails mid-walk.
//!
//! This lives outside `hal` (which is gated to `target_arch = "riscv64"`) so it compiles and is
//! unit-testable on the host -- a test written in `hal` would never run. `arch-aarch64`'s
//! `fork.rs` exists for the same reason.

/// The pointer the release walk uses for a frame: through the physmap on the target, and the
/// identity on the host.
///
/// Host builds have no physmap, and the walk's test builds its tree in an ordinary buffer, so
/// there the identity is what keeps the fixture working -- the same seam
/// `kernel::pagetable::table_ptr` and `arch-aarch64`'s `fork::phys_ptr` use.
#[inline]
fn walk_phys_ptr(pa: u64) -> *mut u64 {
    #[cfg(target_arch = "riscv64")]
    let ptr = crate::hal::phys_to_virt(pa) as *mut u64;
    #[cfg(not(target_arch = "riscv64"))]
    let ptr = pa as *mut u64; // physmap-ok: the host has no physmap; the arm above converts
    ptr
}

/// Where the release walk reports what it gave back, in the host suite.
///
/// That suite has no pool these frames could come from -- `alloc` self-gates to
/// `target_arch = "riscv64"`, so there is no allocator to return a page to -- so
/// `release_table_page` records here instead and the walk's test asserts on the record. Compiled
/// only under `cfg(test)`.
#[cfg(test)]
pub(crate) mod released {
    use core::cell::UnsafeCell;

    pub(crate) struct Log(UnsafeCell<([u64; 32], usize)>);
    // SAFETY: only the host suite reaches this, one test at a time.
    unsafe impl Sync for Log {}

    impl Log {
        pub(crate) const fn new() -> Self {
            Self(UnsafeCell::new(([0; 32], 0)))
        }

        pub(crate) fn record(&self, pa: u64) {
            // SAFETY: single-threaded, as above.
            let state = unsafe { &mut *self.0.get() };
            if state.1 < state.0.len() {
                state.0[state.1] = pa;
                state.1 += 1;
            }
        }

        /// Take the frames recorded so far, clearing the log.
        pub(crate) fn take(&self) -> ([u64; 32], usize) {
            // SAFETY: single-threaded, as above.
            let state = unsafe { &mut *self.0.get() };
            let out = (state.0, state.1);
            state.1 = 0;
            out
        }
    }

    pub(crate) static LOG: Log = Log::new();
}

/// Give one page-table page back to the pool.
///
/// The host arm is deliberately absent: `alloc` self-gates to `target_arch = "riscv64"`, so on the
/// host there is nothing to give a page back to, and the walk is only reachable from its test
/// there (which takes the `cfg(test)` arm above).
#[inline]
fn release_table_page(pa: u64) {
    #[cfg(test)]
    released::LOG.record(pa);
    #[cfg(all(not(test), target_arch = "riscv64"))]
    unsafe {
        crate::alloc::free_phys_page(pa)
    };
    // The host has no allocator to give the page back to (`alloc` self-gates to the target) and
    // nothing reaches this there but the walk's test, through the arm above.
    #[cfg(all(not(test), not(target_arch = "riscv64")))]
    let _ = pa;
}

/// Release every table page the fork walk created under `child_root`.
///
/// The child root starts as a copy of the parent's, and an entry is replaced only when the table
/// below it is deep-copied, so an entry is this walk's to release exactly when it differs from the
/// parent's at the same slot -- every replacement is a freshly allocated page, so it always
/// differs. That test is what keeps the walk off the parent's own tables: the split pass *shares*
/// an L1 by writing the same branch entry into both roots, so those entries compare equal and are
/// skipped. A leaf (V set with any of R/W/X) is a mapping, never a table, and is never followed.
///
/// `pub` because its only caller is `hal::vm_paging_fork` and `hal` is gated to
/// `target_arch = "riscv64"`: a host build would otherwise see this as an unused item.
///
/// `child_root` itself is the caller's root page and is not released here.
///
/// # Safety
///
/// Both roots must be valid, page-aligned, 512-entry SV39 root tables, and `parent_root` must be
/// the parent this child was built from: the walk compares the two roots slot by slot to tell the
/// fork's own tables from the parent's, so a mismatched pair would release live tables.
pub unsafe fn free_fork_tables(parent_root: *const u64, child_root: *mut u64) {
    const V: u64 = 0x001;
    const R: u64 = 0x002;
    const W: u64 = 0x004;
    const X: u64 = 0x008;
    const PPN_MASK: u64 = 0x003FFFFFFFFFFC00;

    unsafe {
        for l2 in 0..512 {
            let child_e = core::ptr::read(child_root.add(l2));
            let parent_e = core::ptr::read(parent_root.add(l2));
            if child_e == parent_e || child_e & V == 0 || child_e & (R | W | X) != 0 {
                continue;
            }
            let child_l1_pa = child_e & PPN_MASK;
            let child_l1 = walk_phys_ptr(child_l1_pa);
            let parent_l1 = walk_phys_ptr(parent_e & PPN_MASK);
            for l1 in 0..512 {
                let child_e = core::ptr::read(child_l1.add(l1));
                let parent_e = core::ptr::read(parent_l1.add(l1));
                if child_e == parent_e || child_e & V == 0 || child_e & (R | W | X) != 0 {
                    continue;
                }
                release_table_page(child_e & PPN_MASK);
            }
            release_table_page(child_l1_pa);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The release walk gives back the tables the fork built and nothing else (`KNOWN_ISSUES.md`
    /// item 39), including the RISC-V case that makes it delicate: the fork's split pass leaves one
    /// table shared between the two roots, so a walk that followed the child alone would release
    /// what the parent is still using.
    ///
    /// It cannot be driven through `vm_paging_fork` here -- that allocates, through a module that
    /// does not exist on the host -- so the trees are built by hand in a buffer and the releases
    /// are read from `released::LOG`.
    #[test]
    fn fork_release_walk_frees_tables_and_spares_leaves() {
        const PAGE: usize = 4096;
        #[repr(align(4096))]
        struct Pages([u8; PAGE * 8]);

        let mut pages = Pages([0u8; PAGE * 8]);
        let base = pages.0.as_mut_ptr() as u64;
        let frame = |i: u64| base + i * PAGE as u64;
        let (parent_root, child_root, parent_l1, child_l1, shared_l1, parent_l0, child_l0) = (
            frame(0),
            frame(1),
            frame(2),
            frame(3),
            frame(4),
            frame(5),
            frame(6),
        );

        const V: u64 = 0x001;
        const R: u64 = 0x002;
        const W: u64 = 0x004;
        const X: u64 = 0x008;
        let table = |pa: u64| pa | V;
        let leaf = |pa: u64, writable: bool| pa | V | R | X | if writable { W } else { 0 };

        let write = |page: u64, idx: usize, v: u64| {
            // SAFETY: every page here is a page of `pages`.
            unsafe { core::ptr::write_volatile((page as *mut u64).add(idx), v) }
        };

        // The split pass writes the SAME branch into both roots: equal entries, one shared table.
        write(parent_root, 2, table(shared_l1));
        write(child_root, 2, table(shared_l1));
        write(shared_l1, 0, leaf(frame(7), true));

        // Our own L1 under root[0], with the parent's L0 and ours beneath it.
        write(parent_root, 0, table(parent_l1));
        write(child_root, 0, table(child_l1));
        write(parent_l1, 0, table(parent_l0));
        write(child_l1, 0, table(child_l0));
        // A 2 MiB leaf the fork COW-protected: it differs from the parent's, and is still a leaf.
        write(parent_l1, 1, leaf(base + 0x20_0000, true));
        write(child_l1, 1, leaf(base + 0x20_0000, false));
        // A 1 GiB leaf, copied verbatim.
        write(parent_root, 1, leaf(base + 0x40_0000, true));
        write(child_root, 1, leaf(base + 0x40_0000, true));

        let _ = released::LOG.take();
        // SAFETY: both roots are `pages`, and `walk_phys_ptr` is the identity here.
        unsafe { free_fork_tables(parent_root as *const u64, child_root as *mut u64) };
        let (freed, n) = released::LOG.take();
        let released = &freed[..n];

        let mut got = released.to_vec();
        got.sort_unstable();
        let mut want = [child_l0, child_l1];
        want.sort_unstable();
        assert_eq!(got, want, "only the tables the fork built may be released");
        assert!(
            !released.contains(&parent_l1) && !released.contains(&parent_l0),
            "the parent's own tables must be left alone"
        );
        assert!(
            !released.contains(&shared_l1),
            "a table the split pass shares with the parent is not the fork's"
        );
        assert!(
            !released.contains(&child_root),
            "the root is the caller's own page"
        );
    }
}
