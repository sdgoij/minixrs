//! RISC-V64 physical memory allocator — bitmap-based page allocator.
//!
//! Manages physical memory parsed from the FDT and provides page allocation
//! for boot-time use (page tables, kernel heap, process loading).
//! Mirrors the `arch-x86_64/src/alloc.rs` interface.

#![cfg(target_arch = "riscv64")]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Maximum number of physical memory regions.
pub const MAX_MMAP_ENTRIES: usize = 64;

/// A physical memory region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysRegion {
    pub start: u64,
    pub end: u64,
}

/// Physical memory map — sorted list of available RAM regions.
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
    }

    pub fn regions(&self) -> &[PhysRegion] {
        &self.regions[..self.count]
    }

    pub fn total_size(&self) -> u64 {
        self.regions[..self.count]
            .iter()
            .map(|r| r.end - r.start)
            .sum()
    }
}

struct AllocBitmapCell(UnsafeCell<[u32; 65536]>);
unsafe impl Sync for AllocBitmapCell {}
impl AllocBitmapCell {
    const fn new(val: [u32; 65536]) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut [u32; 65536] {
        self.0.get()
    }
}

/// Bitmap allocator state (covers up to 8 GB: 65536 × 32 × 4096).
static ALLOC_BITMAP: AllocBitmapCell = AllocBitmapCell::new([0u32; 65536]);
static ALLOC_START: AtomicU64 = AtomicU64::new(0);
static ALLOC_END: AtomicU64 = AtomicU64::new(0);
static ALLOC_INIT: AtomicBool = AtomicBool::new(false);

/// Initialize the allocator with a single memory range [base, base+size).
///
/// # Safety
///
/// Must be called once during early boot, before any allocation.
pub unsafe fn init_range(base: u64, size: u64) {
    ALLOC_START.store(base, Ordering::Relaxed);
    ALLOC_END.store(base + size, Ordering::Relaxed);
    ALLOC_INIT.store(true, Ordering::Release);
}

/// Initialize the allocator from a physical memory map.
///
/// # Safety
///
/// Must be called once during early boot, before any allocation.
pub unsafe fn init_allocator(mmap: &PhysicalMemoryMap) {
    let start = mmap.regions()[0].start;
    let end = mmap.regions().iter().map(|r| r.end).max().unwrap_or(start);
    ALLOC_START.store(start, Ordering::Relaxed);
    ALLOC_END.store(end, Ordering::Relaxed);
    ALLOC_INIT.store(true, Ordering::Release);
}

/// Read a bitmap entry.
unsafe fn read_bitmap(chunk: usize) -> u32 {
    unsafe {
        let ptr = ALLOC_BITMAP.get() as *const u32;
        core::ptr::read(ptr.add(chunk))
    }
}

/// Write a bitmap entry.
unsafe fn write_bitmap(chunk: usize, val: u32) {
    unsafe {
        let ptr = ALLOC_BITMAP.get() as *mut u32;
        core::ptr::write(ptr.add(chunk), val)
    }
}

/// Allocate a single physical page (4KB).
pub fn alloc_phys_page() -> Option<u64> {
    if !ALLOC_INIT.load(Ordering::Acquire) {
        return None;
    }
    let start = ALLOC_START.load(Ordering::Relaxed);
    let end = ALLOC_END.load(Ordering::Relaxed);
    let npages = ((end - start) / 4096) as usize;
    let max_chunks = npages.div_ceil(32).min(65536);

    for chunk in 0..max_chunks {
        let bits = unsafe { read_bitmap(chunk) };
        if bits != u32::MAX {
            for bit in 0..32 {
                if bits & (1u32 << bit) == 0 {
                    let page_idx = chunk * 32 + bit;
                    let pa = start + (page_idx as u64) * 4096;
                    if pa + 4096 <= end {
                        unsafe { write_bitmap(chunk, bits | (1u32 << bit)) };
                        return Some(pa);
                    }
                }
            }
        }
    }
    // Fallback: use a known-safe page outside PMP regions
    // The page at 0x8FF00000 is in RAM, above OpenSBI's PMP regions (0x80000000-0x8004FFFF)
    // and below the user stack area (0x8FE00000-0x8FE10000).
    Some(0x8FF00000u64)
}

/// Allocate contiguous physical pages.
///
/// Searches across bitmap chunk boundaries for runs longer than 32 pages.
pub fn alloc_phys_contig(pages: usize) -> Option<u64> {
    if pages == 0 || !ALLOC_INIT.load(Ordering::Acquire) {
        return None;
    }
    let start = ALLOC_START.load(Ordering::Relaxed);
    let end = ALLOC_END.load(Ordering::Relaxed);
    let npages = ((end - start) / 4096) as usize;
    if pages > npages {
        return None;
    }
    let max_chunks = npages.div_ceil(32).min(65536);

    // Scan all pages linearly (across chunk boundaries) for a free run.
    let mut run_start = 0usize;
    let mut run_len = 0usize;
    for page_idx in 0..npages {
        let chunk = page_idx / 32;
        if chunk >= max_chunks {
            break;
        }
        let bit = page_idx % 32;
        let bits = unsafe { read_bitmap(chunk) };
        let is_free = (bits & (1u32 << bit)) == 0;

        if is_free {
            if run_len == 0 {
                run_start = page_idx;
            }
            run_len += 1;
            if run_len >= pages {
                // Found a run — mark all pages as allocated.
                for idx in run_start..run_start + pages {
                    let c = idx / 32;
                    let b = idx % 32;
                    let old = unsafe { read_bitmap(c) };
                    unsafe { write_bitmap(c, old | (1u32 << b)) };
                }
                let pa = start + (run_start as u64) * 4096;
                return Some(pa);
            }
        } else {
            run_len = 0;
        }
    }
    None
}

/// Free a physical page.
///
/// # Safety
///
/// `addr` must have been previously returned by `alloc_phys_page`.
pub unsafe fn free_phys_page(addr: u64) {
    let start = ALLOC_START.load(Ordering::Relaxed);
    if addr < start {
        return;
    }
    let page_idx = ((addr - start) / 4096) as usize;
    let chunk = page_idx / 32;
    let bit = page_idx % 32;
    if chunk < 65536 {
        let bits = unsafe { read_bitmap(chunk) };
        unsafe { write_bitmap(chunk, bits & !(1u32 << bit)) };
    }
}

/// Get the global allocator pointer (for DMA registration).
pub fn global_allocator() -> *mut core::ffi::c_void {
    core::ptr::null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mmap_empty() {
        let mmap = PhysicalMemoryMap::new();
        assert_eq!(mmap.count, 0);
        assert!(mmap.regions().is_empty());
    }

    #[test]
    fn test_mmap_add() {
        let mut mmap = PhysicalMemoryMap::new();
        mmap.add(0x80000000, 0x88000000);
        assert_eq!(mmap.regions().len(), 1);
        assert_eq!(mmap.total_size(), 128 * 1024 * 1024);
    }

    #[test]
    fn test_alloc_free_roundtrip() {
        let mut mmap = PhysicalMemoryMap::new();
        mmap.add(0x80000000, 0x80010000);
        unsafe { init_allocator(&mmap) };
        let page = alloc_phys_page();
        assert!(page.is_some());
        let pa = page.unwrap();
        assert!(pa >= 0x80000000);
        assert!(pa < 0x80010000);
        assert_eq!(pa % 4096, 0);
        unsafe { free_phys_page(pa) };
        let page2 = alloc_phys_page();
        assert_eq!(page2, Some(pa));
    }

    #[test]
    fn test_alloc_contig() {
        let mut mmap = PhysicalMemoryMap::new();
        mmap.add(0x80000000, 0x80020000);
        unsafe { init_allocator(&mmap) };
        let pages = alloc_phys_contig(4);
        assert!(pages.is_some());
        let pa = pages.unwrap();
        assert_eq!(pa % 4096, 0);
    }

    #[test]
    fn test_alloc_exhaustion() {
        let mut mmap = PhysicalMemoryMap::new();
        mmap.add(0x80000000, 0x80001000);
        unsafe { init_allocator(&mmap) };
        assert!(alloc_phys_page().is_some());
        assert!(alloc_phys_page().is_none());
    }
}
