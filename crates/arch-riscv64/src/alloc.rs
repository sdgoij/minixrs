//! RISC-V64 physical memory allocator — bitmap-based page allocator.
//!
//! Manages physical memory parsed from the FDT and provides page allocation
//! for boot-time use (page tables, kernel heap, process loading).
//! Mirrors the `arch-x86_64/src/alloc.rs` interface.

#![cfg(target_arch = "riscv64")]
#![allow(static_mut_refs)]

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

/// Bitmap allocator state (covers up to 8 GB: 65536 × 32 × 4096).
static mut ALLOC_BITMAP: [u32; 65536] = [0u32; 65536];
static ALLOC_START: AtomicU64 = AtomicU64::new(0);
static ALLOC_END: AtomicU64 = AtomicU64::new(0);
static ALLOC_INIT: AtomicBool = AtomicBool::new(false);

/// Initialize the allocator from a memory map.
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
        let ptr = core::ptr::addr_of!(ALLOC_BITMAP) as *const u32;
        core::ptr::read(ptr.add(chunk))
    }
}

/// Write a bitmap entry.
unsafe fn write_bitmap(chunk: usize, val: u32) {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(ALLOC_BITMAP) as *mut u32;
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
    None
}

/// Allocate contiguous physical pages.
pub fn alloc_phys_contig(pages: usize) -> Option<u64> {
    if pages == 0 || !ALLOC_INIT.load(Ordering::Acquire) {
        return None;
    }
    let start = ALLOC_START.load(Ordering::Relaxed);
    let end = ALLOC_END.load(Ordering::Relaxed);
    let npages = ((end - start) / 4096) as usize;
    let max_chunks = npages.div_ceil(32).min(65536);

    for chunk in 0..max_chunks {
        let bits = unsafe { read_bitmap(chunk) };
        let mut run_start = 0;
        let mut run_len = 0;
        for bit in 0..32 {
            let page_idx = chunk * 32 + bit;
            if page_idx >= npages {
                break;
            }
            if bits & (1u32 << bit) == 0 {
                if run_len == 0 {
                    run_start = bit;
                }
                run_len += 1;
                if run_len >= pages {
                    let mut new_bits = bits;
                    for b in run_start..run_start + pages {
                        new_bits |= 1u32 << b;
                    }
                    unsafe { write_bitmap(chunk, new_bits) };
                    let pa = start + ((chunk * 32 + run_start) as u64) * 4096;
                    return Some(pa);
                }
            } else {
                run_len = 0;
            }
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
