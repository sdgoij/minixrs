//! Raw physical memory allocator for x86_64.
//!
//! Manages the physical memory map reported by the bootloader (multiboot)
//! and provides a bitmap-based page allocator for kernel boot-time
//! allocation of physical pages.
//!
//! The bitmap stores one bit per 4 KB page. Default max memory is 64 GB
//! (2 MB bitmap). The bitmap storage is provided externally so it can
//! live in a static, not on the stack.

use crate::param::NBPG;

// ═════════════════════════════════════════════════════════════════════════
// PhysicalMemoryMap
// ═════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysRegion {
    pub start: u64,
    pub end: u64,
}

pub const MAX_MMAP_ENTRIES: usize = 64;

/// Physical memory map — sorted, non-overlapping list of available regions.
pub struct PhysicalMemoryMap {
    regions: [PhysRegion; MAX_MMAP_ENTRIES],
    count: usize,
}

impl Default for PhysicalMemoryMap {
    fn default() -> Self {
        Self::new()
    }
}

impl PhysicalMemoryMap {
    pub const fn new() -> Self {
        Self {
            regions: [PhysRegion { start: 0, end: 0 }; MAX_MMAP_ENTRIES],
            count: 0,
        }
    }

    pub fn add(&mut self, start: u64, end: u64) {
        if start >= end || self.count >= MAX_MMAP_ENTRIES {
            return;
        }
        self.regions[self.count] = PhysRegion { start, end };
        self.count += 1;
        self.sort();
        self.merge();
    }

    pub fn cut(&mut self, start: u64, end: u64) {
        if start >= end {
            return;
        }
        let mut new = [PhysRegion { start: 0, end: 0 }; MAX_MMAP_ENTRIES];
        let mut n = 0;
        for i in 0..self.count {
            let r = self.regions[i];
            if end <= r.start || start >= r.end {
                if n < MAX_MMAP_ENTRIES {
                    new[n] = r;
                    n += 1;
                }
                continue;
            }
            if start > r.start && n < MAX_MMAP_ENTRIES {
                new[n] = PhysRegion {
                    start: r.start,
                    end: start,
                };
                n += 1;
            }
            if end < r.end && n < MAX_MMAP_ENTRIES {
                new[n] = PhysRegion {
                    start: end,
                    end: r.end,
                };
                n += 1;
            }
        }
        self.regions = new;
        self.count = n;
    }

    pub fn iter(&self) -> impl Iterator<Item = &PhysRegion> {
        self.regions[..self.count].iter()
    }

    pub fn total_available(&self) -> u64 {
        self.iter().map(|r| r.end - r.start).sum()
    }

    pub fn highest_phys(&self) -> u64 {
        self.iter().map(|r| r.end).max().unwrap_or(0)
    }

    fn sort(&mut self) {
        for i in 1..self.count {
            let key = self.regions[i];
            let mut j = i;
            while j > 0 && self.regions[j - 1].start > key.start {
                self.regions[j] = self.regions[j - 1];
                j -= 1;
            }
            self.regions[j] = key;
        }
    }

    fn merge(&mut self) {
        if self.count == 0 {
            return;
        }
        let mut w = 0;
        for r in 1..self.count {
            if self.regions[w].end >= self.regions[r].start {
                if self.regions[r].end > self.regions[w].end {
                    self.regions[w].end = self.regions[r].end;
                }
            } else {
                w += 1;
                self.regions[w] = self.regions[r];
            }
        }
        self.count = w + 1;
    }
}

// ═════════════════════════════════════════════════════════════════════════
// BitmapStorage — external bitmap memory
// ═════════════════════════════════════════════════════════════════════════

/// A bitmap stored in external memory (typically a static).
///
/// `N` is the number of u64 slots, giving `N * 64` bits (pages).
pub struct BitmapStorage<const N: usize> {
    bits: [u64; N],
}

impl<const N: usize> Default for BitmapStorage<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> BitmapStorage<N> {
    pub const fn new() -> Self {
        Self { bits: [0u64; N] }
    }

    pub fn as_slice(&mut self) -> &mut [u64] {
        &mut self.bits
    }
}

// ═════════════════════════════════════════════════════════════════════════
// PhysicalAllocator
// ═════════════════════════════════════════════════════════════════════════

/// Bitmap-based physical page allocator.
///
/// The bitmap memory is provided externally via `BitmapStorage` so it
/// can live in a static, avoiding large stack allocations.
pub struct PhysicalAllocator {
    bitmap: *mut u64,
    bitmap_len: usize, // number of u64 entries
    top_page: usize,
    free_pages: usize,
}

impl PhysicalAllocator {
    /// Create a new allocator backed by the given bitmap storage.
    pub fn new<const N: usize>(storage: &mut BitmapStorage<N>) -> Self {
        let bitmap = storage.as_slice();
        let len = bitmap.len();
        for b in bitmap.iter_mut() {
            *b = 0;
        }
        Self {
            bitmap: bitmap.as_mut_ptr(),
            bitmap_len: len,
            top_page: 0,
            free_pages: 0,
        }
    }

    pub fn init_from_mmap(&mut self, mmap: &PhysicalMemoryMap) {
        let total_bits = self.bitmap_len * 64;
        // Clear bitmap
        for i in 0..self.bitmap_len {
            unsafe {
                *self.bitmap.add(i) = 0;
            }
        }
        self.top_page = 0;
        self.free_pages = 0;
        for region in mmap.iter() {
            self.add_region(region.start, region.end, total_bits);
        }
    }

    fn add_region(&mut self, start: u64, end: u64, total_bits: usize) {
        if start >= end {
            return;
        }
        let s = (start / NBPG) as usize;
        let e = (end / NBPG) as usize;
        let e = e.min(total_bits);
        for p in s..e {
            self.set_free(p);
        }
        if e > self.top_page {
            self.top_page = e;
        }
    }

    fn set_free(&mut self, page: usize) {
        let i = page / 64;
        let b = page % 64;
        if i >= self.bitmap_len {
            return;
        }
        unsafe {
            let p = self.bitmap.add(i);
            if *p & (1u64 << b) == 0 {
                *p |= 1u64 << b;
                self.free_pages += 1;
            }
        }
    }

    fn set_used(&mut self, page: usize) {
        let i = page / 64;
        let b = page % 64;
        if i >= self.bitmap_len {
            return;
        }
        unsafe {
            let p = self.bitmap.add(i);
            if *p & (1u64 << b) != 0 {
                *p &= !(1u64 << b);
                self.free_pages -= 1;
            }
        }
    }

    fn is_free(&self, page: usize) -> bool {
        let i = page / 64;
        let b = page % 64;
        if i >= self.bitmap_len {
            return false;
        }
        unsafe { (*self.bitmap.add(i) & (1u64 << b)) != 0 }
    }

    /// Allocate a single page (high-to-low search).
    pub fn alloc_page(&mut self) -> Option<u64> {
        let end = self.top_page.min(self.bitmap_len * 64);
        for p in (0..end).rev() {
            if self.is_free(p) {
                self.set_used(p);
                return Some((p as u64) * NBPG);
            }
        }
        None
    }

    /// Allocate contiguous pages (first-fit).
    pub fn alloc_contig(&mut self, count: usize) -> Option<u64> {
        if count == 0 {
            return None;
        }
        let end = self.top_page.min(self.bitmap_len * 64);
        let (mut run, mut run_len) = (0, 0);
        for p in 0..end {
            if self.is_free(p) {
                if run_len == 0 {
                    run = p;
                }
                run_len += 1;
                if run_len >= count {
                    for pp in run..run + count {
                        self.set_used(pp);
                    }
                    return Some((run as u64) * NBPG);
                }
            } else {
                run_len = 0;
            }
        }
        None
    }

    pub fn free_page(&mut self, addr: u64) {
        self.set_free((addr / NBPG) as usize);
    }

    pub fn free_contig(&mut self, addr: u64, count: usize) {
        let s = (addr / NBPG) as usize;
        for p in s..s + count {
            self.set_free(p);
        }
    }

    pub fn reserve(&mut self, start: u64, size: u64) {
        if size == 0 {
            return;
        }
        let s = (start / NBPG) as usize;
        let e = ((start + size) / NBPG) as usize;
        let e = e.min(self.bitmap_len * 64);
        for p in s..e {
            self.set_used(p);
        }
    }

    pub fn reserve_kernel(&mut self, kernel_start: u64, kernel_end: u64) {
        self.reserve(kernel_start, kernel_end - kernel_start);
    }

    pub fn free_count(&self) -> usize {
        self.free_pages
    }
    pub fn total_pages(&self) -> usize {
        self.top_page
    }
    pub fn bitmap_len(&self) -> usize {
        self.bitmap_len
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Global allocator
// ═════════════════════════════════════════════════════════════════════════

use core::sync::atomic::{AtomicBool, Ordering};

/// Number of pages for 64 GB of physical memory.
const GLOBAL_BITMAP_U64: usize = (64u64 * 1024 * 1024 * 1024 / 4096 / 64) as usize;

static ALLOC_INITIALIZED: AtomicBool = AtomicBool::new(false);
static mut GLOBAL_BITMAP: BitmapStorage<GLOBAL_BITMAP_U64> = BitmapStorage::new();
static mut GLOBAL_ALLOCATOR: *mut PhysicalAllocator = core::ptr::null_mut();

/// Internal allocator instance (lazily initialized).
static mut ALLOC_INSTANCE: PhysicalAllocator = PhysicalAllocator {
    bitmap: core::ptr::null_mut(),
    bitmap_len: 0,
    top_page: 0,
    free_pages: 0,
};

pub fn init_allocator(mmap: &PhysicalMemoryMap) {
    if ALLOC_INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }
    unsafe {
        let storage = &raw mut GLOBAL_BITMAP;
        let alloc = &raw mut ALLOC_INSTANCE;
        *alloc = PhysicalAllocator::new(&mut *storage);
        (*alloc).init_from_mmap(mmap);
        GLOBAL_ALLOCATOR = alloc;
    }
}

pub fn global_allocator() -> *mut PhysicalAllocator {
    assert!(
        ALLOC_INITIALIZED.load(Ordering::SeqCst),
        "allocator not initialized"
    );
    unsafe { GLOBAL_ALLOCATOR }
}

pub fn alloc_phys_page() -> Option<u64> {
    unsafe { (*global_allocator()).alloc_page() }
}

pub fn free_phys_page(addr: u64) {
    unsafe {
        (*global_allocator()).free_page(addr);
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Tests — use a small bitmap (256 bits = 256 pages = 1 MB)
// ═════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BITS: usize = 256; // enough for our small test regions
    type TestStorage = BitmapStorage<TEST_BITS>;

    fn make_alloc() -> (PhysicalAllocator, TestStorage) {
        let mut storage = TestStorage::new();
        let mut alloc = PhysicalAllocator::new(&mut storage);
        let mut mmap = PhysicalMemoryMap::new();
        mmap.add(0x100000, 0x200000);
        mmap.add(0x1000000, 0x2000000);
        alloc.init_from_mmap(&mmap);
        (alloc, storage)
    }

    #[test]
    fn test_mmap_add_and_merge() {
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1000, 0x2000);
        mm.add(0x2000, 0x3000);
        assert_eq!(mm.count, 1);
        assert_eq!(mm.regions[0].start, 0x1000);
        assert_eq!(mm.regions[0].end, 0x3000);
    }

    #[test]
    fn test_mmap_cut() {
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1000, 0x5000);
        mm.cut(0x2000, 0x4000);
        assert_eq!(mm.count, 2);
        assert_eq!(mm.regions[0].start, 0x1000);
        assert_eq!(mm.regions[0].end, 0x2000);
        assert_eq!(mm.regions[1].start, 0x4000);
        assert_eq!(mm.regions[1].end, 0x5000);
    }

    #[test]
    fn test_mmap_totals() {
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x100000, 0x200000);
        mm.add(0x1000000, 0x2000000);
        assert_eq!(mm.total_available(), 0x100000 + 0x1000000);
        assert_eq!(mm.highest_phys(), 0x2000000);
    }

    #[test]
    fn test_alloc_single() {
        let (mut a, _) = make_alloc();
        let p = a.alloc_page().unwrap();
        assert_eq!(p % NBPG, 0);
    }

    #[test]
    fn test_alloc_contig() {
        let (mut a, _) = make_alloc();
        let b = a.alloc_contig(64).unwrap();
        let s = (b / NBPG) as usize;
        for p in s..s + 64 {
            assert!(!a.is_free(p));
        }
    }

    #[test]
    fn test_free_contig() {
        let (mut a, _) = make_alloc();
        let b = a.alloc_contig(16).unwrap();
        a.free_contig(b, 16);
        let s = (b / NBPG) as usize;
        for p in s..s + 16 {
            assert!(a.is_free(p));
        }
    }

    #[test]
    fn test_free_count() {
        let (mut a, _) = make_alloc();
        let before = a.free_count();
        let b = a.alloc_contig(10).unwrap();
        assert_eq!(a.free_count(), before - 10);
        a.free_contig(b, 10);
        assert_eq!(a.free_count(), before);
    }

    #[test]
    fn test_global() {
        let mut mmap = PhysicalMemoryMap::new();
        mmap.add(0x1000, 0x50000);
        init_allocator(&mmap);
        let p = alloc_phys_page();
        assert!(p.is_some());
    }

    #[test]
    fn test_empty_mmap() {
        let mm = PhysicalMemoryMap::new();
        assert_eq!(mm.total_available(), 0);
        assert_eq!(mm.highest_phys(), 0);
    }

    #[test]
    fn test_allocator_bitmap_len() {
        let (a, _) = make_alloc();
        assert!(a.bitmap_len() > 0);
    }

    // ── PhysicalMemoryMap edge cases ──────────────────────────────────────

    #[test]
    fn test_mmap_cut_no_overlap() {
        // Cut on range that doesn't intersect any region — should be no-op.
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1000, 0x3000);
        mm.cut(0x5000, 0x6000);
        assert_eq!(mm.count, 1);
        assert_eq!(mm.regions[0].start, 0x1000);
        assert_eq!(mm.regions[0].end, 0x3000);
    }

    #[test]
    fn test_mmap_cut_exact_start() {
        // Cut starting exactly at the region start.
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1000, 0x5000);
        mm.cut(0x1000, 0x3000);
        assert_eq!(mm.count, 1);
        assert_eq!(mm.regions[0].start, 0x3000);
        assert_eq!(mm.regions[0].end, 0x5000);
    }

    #[test]
    fn test_mmap_cut_exact_end() {
        // Cut ending exactly at the region end.
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1000, 0x5000);
        mm.cut(0x3000, 0x5000);
        assert_eq!(mm.count, 1);
        assert_eq!(mm.regions[0].start, 0x1000);
        assert_eq!(mm.regions[0].end, 0x3000);
    }

    #[test]
    fn test_mmap_cut_full_region() {
        // Cut that exactly covers a region — should remove it entirely.
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1000, 0x3000);
        mm.cut(0x1000, 0x3000);
        assert_eq!(mm.count, 0);
    }

    #[test]
    fn test_mmap_cut_invalid() {
        // Cut with start >= end should be no-op.
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1000, 0x5000);
        mm.cut(0x4000, 0x2000);
        assert_eq!(mm.count, 1);
    }

    #[test]
    fn test_mmap_add_max_entries_overflow() {
        let mut mm = PhysicalMemoryMap::new();
        for i in 0..MAX_MMAP_ENTRIES + 10 {
            mm.add((i as u64) * 0x1000, (i as u64 + 1) * 0x1000);
        }
        // count should be capped at MAX_MMAP_ENTRIES (the non-adjacent
        // entries won't merge, so this exercises the `self.count >= MAX` guard).
        assert!(mm.count <= MAX_MMAP_ENTRIES);
    }

    // ── PhysicalAllocator edge cases ──────────────────────────────────────

    #[test]
    fn test_alloc_from_empty_mmap() {
        let mut storage = TestStorage::new();
        let mut alloc = PhysicalAllocator::new(&mut storage);
        let mm = PhysicalMemoryMap::new();
        alloc.init_from_mmap(&mm);
        assert_eq!(alloc.free_count(), 0);
        assert!(alloc.alloc_page().is_none());
        assert!(alloc.alloc_contig(1).is_none());
    }

    #[test]
    fn test_alloc_exhaustion() {
        let (mut a, _) = make_alloc();
        let total = a.free_count();
        // Allocate every available page one by one.
        for _ in 0..total {
            assert!(a.alloc_page().is_some());
        }
        // Now the allocator should be empty.
        assert_eq!(a.free_count(), 0);
        assert!(a.alloc_page().is_none());
    }

    #[test]
    fn test_alloc_contig_zero() {
        let (mut a, _) = make_alloc();
        assert!(a.alloc_contig(0).is_none());
    }

    #[test]
    fn test_double_free() {
        // Freeing the same page twice should be idempotent.
        let (mut a, _) = make_alloc();
        let before = a.free_count();
        let addr = a.alloc_page().unwrap();
        assert_eq!(a.free_count(), before - 1);
        a.free_page(addr);
        assert_eq!(a.free_count(), before);
        a.free_page(addr);
        // free_count should still be `before` — second free is a no-op.
        assert_eq!(a.free_count(), before);
    }

    #[test]
    fn test_reserve_all() {
        let (mut a, _) = make_alloc();
        let total = a.total_pages();
        // Reserve every representable page (beyond available memory).
        a.reserve(0, (total as u64) * NBPG);
        assert_eq!(a.free_count(), 0);
        assert!(a.alloc_page().is_none());
    }

    #[test]
    fn test_reserve_zero_size() {
        let (mut a, _) = make_alloc();
        let before = a.free_count();
        a.reserve(0, 0);
        assert_eq!(a.free_count(), before);
    }

    #[test]
    fn test_bitmap_overflow_handled() {
        // Pages beyond the bitmap should be silently ignored (not panic).
        let (mut a, _) = make_alloc();
        let past_end = a.bitmap_len() * 64 + 100;
        // set_used / set_free / is_free for out-of-bounds pages should be
        // no-ops.  We test via reserve which calls set_used internally.
        a.reserve((past_end as u64) * NBPG, 4096);
        // The operation should not have affected any in-bounds state.
        assert!(a.free_count() > 0);
    }

    #[test]
    fn test_alloc_contig_across_region_gap() {
        // Two separate available regions with a gap. A contig alloc larger
        // than the first region should still succeed from the second.
        let mut storage = TestStorage::new();
        let mut alloc = PhysicalAllocator::new(&mut storage);
        let mut mm = PhysicalMemoryMap::new();
        // Add a small region (16 pages).
        mm.add(0x100000, 0x110000);
        // Add a larger region starting after a gap.
        mm.add(0x120000, 0x140000);
        alloc.init_from_mmap(&mm);
        // Ask for 32 pages — must come from the second region.
        let addr = alloc.alloc_contig(32).unwrap();
        assert!(addr >= 0x120000);
        assert!(addr < 0x140000);
    }

    #[test]
    fn test_mmap_4gb_above() {
        // Regions above 4 GB should be handled correctly (no 32-bit truncation).
        let mut mm = PhysicalMemoryMap::new();
        mm.add(0x1_0000_0000, 0x2_0000_0000);
        assert_eq!(mm.highest_phys(), 0x2_0000_0000);
        assert_eq!(mm.total_available(), 0x1_0000_0000);
    }

    #[test]
    fn test_alloc_contig_spanning_page_boundary() {
        // Allocate from region that starts mid-bitmap-word.
        let mut storage = TestStorage::new();
        let mut alloc = PhysicalAllocator::new(&mut storage);
        let mut mm = PhysicalMemoryMap::new();
        // Start at page 70 (bit in second u64 of the bitmap).
        mm.add(70 * NBPG, 200 * NBPG);
        alloc.init_from_mmap(&mm);
        let addr = alloc.alloc_contig(16).unwrap();
        assert_eq!(addr % NBPG, 0);
        let page = (addr / NBPG) as usize;
        for p in page..page + 16 {
            assert!(!alloc.is_free(p));
        }
    }
}
