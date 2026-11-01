//! Reference counts for physical frames.
//!
//! One count byte per 4 KiB frame of the machine's physical window, indexed by
//! page number. A frame's count tells a teardown whether the frame has another
//! owner, and a write fault with more than one whether the writer has to copy it
//! first (copy-on-write after a fork).
//!
//! The counts do not live in VM's image: [`pb_init`] asks the kernel for the
//! physical allocator's geometry, allocates the bytes, and maps them into VM's
//! own address space. Until that runs (host tests, or a guest whose allocation
//! failed) a small static table covers the low physical addresses.
//!
//! Every lookup is one array index. The previous table was 16384 entries with a
//! linear scan, and `pb_find` ran on every write fault, so a process large
//! enough to matter made page faults quadratic; the capacity was also a wall a
//! single fork of a large process could reach. A frame outside the window is
//! simply untracked, and every caller reads "untracked" as "private": the fork
//! path copies such a page instead of sharing it, so "no count means no other
//! owner" holds whether or not the window exists.

/// Size of a physical frame.
const PAGE_SIZE: u64 = 4096;

use core::cell::UnsafeCell;

/// Frames covered before [`pb_init`] runs, and on the host.
const STATIC_FRAMES: usize = 8192;

/// Refcounts for the frames of one physical window, indexed by page number.
pub struct PhysRefs {
    base_pfn: u64,
    /// One count per frame from `base_pfn`; `len` bytes, all writable.
    counts: *mut u8,
    len: usize,
}

// Safety: VM is a single-threaded server and the only user of this type.
unsafe impl Sync for PhysRefs {}

impl PhysRefs {
    /// A table over an already-mapped window of `frames` count bytes.
    ///
    /// # Safety
    ///
    /// `counts` must point to `frames` writable bytes that outlive the table.
    pub const unsafe fn from_window(base_pfn: u64, counts: *mut u8, frames: usize) -> Self {
        Self {
            base_pfn,
            counts,
            len: frames,
        }
    }

    fn counts(&self) -> &[u8] {
        // SAFETY: `counts` points at `len` bytes (see `from_window`).
        unsafe { core::slice::from_raw_parts(self.counts, self.len) }
    }

    fn counts_mut(&mut self) -> &mut [u8] {
        // SAFETY: as `counts`, and `&mut self` rules out aliasing through this
        // table.
        unsafe { core::slice::from_raw_parts_mut(self.counts, self.len) }
    }

    /// Index of `pfn` in the window, if it is covered.
    fn index(&self, pfn: u64) -> Option<usize> {
        let offset = pfn.checked_sub(self.base_pfn)?;
        if offset < self.len as u64 {
            Some(offset as usize)
        } else {
            None
        }
    }

    fn slot_of(&self, phys: u64) -> Option<usize> {
        self.index(phys / PAGE_SIZE)
    }

    /// Start tracking `phys` with a count of one, unless it is already tracked.
    ///
    /// Returns the handle (the frame's page number), or `None` if the frame is
    /// outside the window; the caller then has to treat it as unshareable.
    pub fn allocate(&mut self, phys: u64) -> Option<usize> {
        let slot = self.slot_of(phys)?;
        if self.counts()[slot] == 0 {
            self.counts_mut()[slot] = 1;
        }
        Some((phys / PAGE_SIZE) as usize)
    }

    /// Add a reference to the frame `pfn` is the page number of.
    ///
    /// Returns false when the frame is not tracked (never allocated, already
    /// released, or outside the window).
    pub fn reference(&mut self, pfn: usize) -> bool {
        let Some(slot) = self.index(pfn as u64) else {
            return false;
        };
        let count = &mut self.counts_mut()[slot];
        if *count == 0 {
            return false;
        }
        *count = count.saturating_add(1);
        true
    }

    /// Drop a reference, returning the frame to the allocator when it was the
    /// last one.
    pub fn unreference(&mut self, pfn: usize) {
        let Some(slot) = self.index(pfn as u64) else {
            return;
        };
        let count = &mut self.counts_mut()[slot];
        if *count == 0 {
            return;
        }
        *count -= 1;
        if *count == 0 {
            drop_frame((pfn as u64) * PAGE_SIZE);
        }
    }

    /// Count for `pfn`, or 0 when the frame is untracked.
    pub fn count(&self, pfn: usize) -> u8 {
        self.index(pfn as u64).map_or(0, |slot| self.counts()[slot])
    }

    /// Handle for the frame at `phys`, if the table is tracking it.
    pub fn find(&self, phys: u64) -> Option<usize> {
        let slot = self.slot_of(phys)?;
        if self.counts()[slot] == 0 {
            None
        } else {
            Some((phys / PAGE_SIZE) as usize)
        }
    }

    /// Number of frames the window covers.
    pub fn capacity(&self) -> usize {
        self.len
    }
}

/// Return a frame whose last reference is gone to the allocator.
#[cfg(target_os = "minix")]
fn drop_frame(phys: u64) {
    // Frame 0 is below every allocator window: it is never a user page.
    if phys != 0 {
        crate::vm::vm_free_pages(phys, 1);
    }
}

/// Return a frame whose last reference is gone to the allocator.
///
/// The host has no allocator behind `vm_free_pages` (it is a kernel call), so
/// there is nothing to release there; the count is cleared either way, which is
/// what the host suites observe.
#[cfg(not(target_os = "minix"))]
fn drop_frame(_phys: u64) {}

struct CountsCell(UnsafeCell<[u8; STATIC_FRAMES]>);
unsafe impl Sync for CountsCell {}

impl CountsCell {
    const fn new() -> Self {
        Self(UnsafeCell::new([0; STATIC_FRAMES]))
    }
}

static STATIC_COUNTS: CountsCell = CountsCell::new();

struct TableCell(UnsafeCell<PhysRefs>);
unsafe impl Sync for TableCell {}

impl TableCell {
    const fn new() -> Self {
        // SAFETY: the counts are this module's own static, which outlives the
        // table, and nothing else writes them.
        Self(UnsafeCell::new(unsafe {
            PhysRefs::from_window(0, STATIC_COUNTS.0.get() as *mut u8, STATIC_FRAMES)
        }))
    }

    fn get(&self) -> *mut PhysRefs {
        self.0.get()
    }
}

static TABLE: TableCell = TableCell::new();

/// Point the counts at the machine's physical window.
///
/// Asks the kernel for the allocator's geometry, takes the bytes the counts
/// need from it, and maps them into VM's own address space at a scratch VA
/// that is never released. Returns false, leaving the static table in place,
/// when the kernel will not give them.
pub fn pb_init(base: u64, usable: u64) -> bool {
    #[cfg(target_os = "minix")]
    {
        use crate::vm::{vm_alloc_pages, vm_free_pages, vm_mappages};

        let frames = (usable / PAGE_SIZE) as usize;
        if frames == 0 {
            return false;
        }
        // One count byte per frame, so the counts need `frames` bytes, in whole
        // pages.
        let pages = frames.div_ceil(PAGE_SIZE as usize);
        let phys = vm_alloc_pages(pages);
        if phys == 0 {
            return false;
        }
        let flags = kernel::pagetable::MAP_USER | kernel::pagetable::MAP_WRITE;
        let va = vm_mappages(phys, pages, flags);
        if va == 0 {
            vm_free_pages(phys, pages);
            return false;
        }
        // SAFETY: the kernel just mapped `pages` frames at `va`, so `frames`
        // bytes are writable there for as long as VM runs.
        unsafe {
            *TABLE.get() = PhysRefs::from_window(base / PAGE_SIZE, va as *mut u8, frames);
        }
        true
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = (base, usable);
        false
    }
}

/// Start tracking `phys` with a count of one. Returns the handle, or `None`
/// when the frame is outside the window.
pub fn pb_new(phys: u64) -> Option<usize> {
    unsafe { (*TABLE.get()).allocate(phys) }
}

/// Add a reference to the frame `pfn` is the page number of.
pub fn pb_ref(pfn: usize) -> bool {
    unsafe { (*TABLE.get()).reference(pfn) }
}

/// Drop a reference, freeing the frame when it was the last one.
pub fn pb_unref(pfn: usize) {
    unsafe { (*TABLE.get()).unreference(pfn) }
}

/// Count for the frame `pfn` is the page number of, or 0 if untracked.
pub fn pb_count(pfn: usize) -> u8 {
    unsafe { (*TABLE.get()).count(pfn) }
}

/// Handle for the frame at `phys`, or `None` if it is not tracked.
pub fn pb_find(phys: u64) -> Option<usize> {
    unsafe { (*TABLE.get()).find(phys) }
}

/// Number of frames the window covers.
pub fn pb_capacity() -> usize {
    unsafe { (*TABLE.get()).capacity() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(base_pfn: u64, frames: usize, buf: &mut [u8]) -> PhysRefs {
        assert!(buf.len() >= frames);
        buf[..frames].fill(0);
        // SAFETY: `buf` outlives the returned table in every test below.
        unsafe { PhysRefs::from_window(base_pfn, buf.as_mut_ptr(), frames) }
    }

    #[test]
    fn a_new_frame_starts_with_one_reference() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        let idx = t.allocate(0x1000).unwrap();
        assert_eq!(idx, 1, "the handle is the page number");
        assert_eq!(t.count(idx), 1);
        assert_eq!(t.find(0x1000), Some(1));
    }

    #[test]
    fn references_accumulate() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        let idx = t.allocate(0x2000).unwrap();
        assert!(t.reference(idx));
        assert!(t.reference(idx));
        assert_eq!(t.count(idx), 3);
    }

    #[test]
    fn a_reference_to_an_untracked_frame_is_refused() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        assert!(!t.reference(3));
        assert_eq!(t.count(3), 0);
    }

    #[test]
    fn the_last_unreference_releases_the_frame() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        let idx = t.allocate(0x3000).unwrap();
        assert!(t.reference(idx));
        t.unreference(idx);
        assert_eq!(t.count(idx), 1);
        t.unreference(idx);
        assert_eq!(t.count(idx), 0);
        assert_eq!(t.find(0x3000), None);
    }

    #[test]
    fn unreferencing_an_untracked_frame_is_harmless() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        t.unreference(5);
        t.unreference(600);
        assert_eq!(t.count(5), 0);
    }

    #[test]
    fn find_distinguishes_frames() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        t.allocate(0x1000);
        let b = t.allocate(0x2000).unwrap();
        t.allocate(0x3000);
        assert_eq!(t.find(0x2000), Some(b));
        assert_eq!(t.find(0x4000), None);
    }

    #[test]
    fn the_count_saturates() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        let idx = t.allocate(0x5000).unwrap();
        for _ in 0..u8::MAX as usize + 3 {
            t.reference(idx);
        }
        assert_eq!(t.count(idx), u8::MAX);
    }

    #[test]
    fn a_window_offset_by_its_base_address_is_indexed_the_same_way() {
        // riscv64 and aarch64 hand VM a window that starts at the middle of
        // physical memory, so the base page number moves with it.
        let mut buf = [0u8; 4];
        let mut t = table(0x80000, 4, &mut buf);
        let base = 0x80000 * PAGE_SIZE;
        assert_eq!(t.allocate(base), Some(0x80000));
        assert_eq!(t.find(base), Some(0x80000));
        assert_eq!(t.find(base + 0x1000), None);
        assert_eq!(t.allocate(base - 0x1000), None, "below the window");
        assert_eq!(t.allocate(base + 4 * 0x1000), None, "above the window");
        assert_eq!(t.capacity(), 4);
    }

    #[test]
    fn a_frame_already_tracked_is_not_reset_by_allocate() {
        let mut buf = [0u8; 8];
        let mut t = table(0, 8, &mut buf);
        let idx = t.allocate(0x6000).unwrap();
        t.reference(idx);
        assert_eq!(t.allocate(0x6000), Some(idx));
        assert_eq!(t.count(idx), 2, "a second owner, not a fresh count");
    }

    #[test]
    fn the_global_table_tracks_and_releases() {
        // Distinct frames: the global table is shared by every test in the
        // binary, and the static window covers these page numbers.
        let a = pb_new(0x10000).unwrap();
        let b = pb_new(0x11000).unwrap();
        assert_ne!(a, b);
        assert!(pb_ref(a));
        assert_eq!(pb_count(a), 2);
        pb_unref(a);
        assert_eq!(pb_count(a), 1);
        pb_unref(a);
        assert_eq!(pb_find(0x10000), None);
        pb_unref(b);
        assert_eq!(pb_find(0x11000), None);
    }

    #[test]
    fn a_frame_outside_the_static_window_is_untracked() {
        let beyond = (STATIC_FRAMES as u64 + 16) * PAGE_SIZE;
        assert_eq!(pb_new(beyond), None);
        assert_eq!(pb_find(beyond), None);
        assert!(pb_capacity() >= STATIC_FRAMES);
    }
}
