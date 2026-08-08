//! AArch64 physical memory allocator.
//!
//! Port of the RISC-V buddy allocator, adjusted for QEMU virt
//! physical memory layout (RAM at 0x40000000, 256MB).

use core::cell::UnsafeCell;
use core::cmp::min;

/// A contiguous range of physical memory.
#[derive(Debug, Clone, Copy)]
pub struct MemoryRange {
    pub start: u64,
    pub end: u64, // exclusive
}

/// Physical memory map (collection of free ranges).
pub struct PhysicalMemoryMap {
    ranges: [MemoryRange; 16],
    count: usize,
}

impl PhysicalMemoryMap {
    pub const fn new() -> Self {
        Self {
            ranges: [MemoryRange { start: 0, end: 0 }; 16],
            count: 0,
        }
    }

    pub fn add(&mut self, start: u64, end: u64) {
        if self.count < self.ranges.len() && start < end {
            self.ranges[self.count] = MemoryRange { start, end };
            self.count += 1;
        }
    }
}

impl Default for PhysicalMemoryMap {
    fn default() -> Self {
        Self::new()
    }
}

struct AllocState {
    bitmap_ptr: *mut u64,
    bitmap_len: usize,
    base: u64,
    total_pages: usize,
}

struct AllocCell(UnsafeCell<AllocState>);
unsafe impl Sync for AllocCell {}

impl AllocCell {
    const fn new() -> Self {
        Self(UnsafeCell::new(AllocState {
            bitmap_ptr: core::ptr::null_mut(),
            bitmap_len: 0,
            base: 0,
            total_pages: 0,
        }))
    }

    fn get(&self) -> *mut AllocState {
        self.0.get()
    }
}

static ALLOC: AllocCell = AllocCell::new();

fn read_field<T>(ptr: *const T) -> T {
    unsafe { core::ptr::read_volatile(ptr) }
}

fn write_field<T>(ptr: *mut T, val: T) {
    unsafe { core::ptr::write_volatile(ptr, val) }
}

/// Initialize the physical page allocator.
///
/// # Safety
///
/// Must be called once during boot, before any allocations.
pub unsafe fn init_allocator(mmap: &PhysicalMemoryMap) {
    let range = mmap.ranges[0];
    let base = (range.start + 4095) & !4095;
    let end = range.end & !4095;
    if base >= end {
        return;
    }
    let total_pages = ((end - base) / 4096) as usize;
    let bitmap_words = total_pages.div_ceil(64);
    let bitmap_bytes = bitmap_words * 8;

    // Place the bitmap at the end of the free range.
    let bitmap_addr = (end - bitmap_bytes as u64) & !4095;
    let usable_end = bitmap_addr;
    let usable_pages = ((usable_end - base) / 4096) as usize;

    // Zero the bitmap with a manual loop (avoids compiler_builtins memset).
    for i in 0..bitmap_words {
        unsafe {
            core::ptr::write_volatile((bitmap_addr as *mut u64).add(i), 0);
        }
    }

    // Mark all usable pages as free.
    let usable_bits = min(usable_pages, total_pages);
    for i in 0..usable_bits {
        unsafe {
            let word = (bitmap_addr as *mut u64).add(i / 64);
            core::ptr::write_volatile(word, core::ptr::read_volatile(word) | (1u64 << (i % 64)));
        }
    }

    let state = ALLOC.get();
    unsafe {
        write_field(&raw mut (*state).bitmap_ptr, bitmap_addr as *mut u64);
        write_field(&raw mut (*state).bitmap_len, bitmap_words);
        write_field(&raw mut (*state).base, base);
        write_field(&raw mut (*state).total_pages, total_pages);
    }
}

/// Get the bitmap slice.
///
/// # Safety
///
/// Must be called after init_allocator.
unsafe fn bitmap_slice() -> &'static mut [u64] {
    let state = ALLOC.get();
    let ptr = unsafe { read_field(&raw const (*state).bitmap_ptr) };
    let len = unsafe { read_field(&raw const (*state).bitmap_len) };
    unsafe { core::slice::from_raw_parts_mut(ptr, len) }
}

pub fn total_pages() -> usize {
    let state = ALLOC.get();
    unsafe { read_field(&raw const (*state).total_pages) }
}

pub fn base() -> u64 {
    let state = ALLOC.get();
    unsafe { read_field(&raw const (*state).base) }
}

/// Allocate a single physical page.
pub fn alloc_phys_page() -> Option<u64> {
    alloc_phys_contig(1)
}

/// Allocate `count` contiguous physical pages.
pub fn alloc_phys_contig(count: usize) -> Option<u64> {
    let total = total_pages();
    let base_addr = base();

    if count == 0 || count > total {
        return None;
    }

    let bitmap = unsafe { bitmap_slice() };
    let words = bitmap.len();
    let mut run = 0usize;
    let mut start_idx = 0usize;

    for word_idx in 0..words {
        let mut word = bitmap[word_idx];
        let base_idx = word_idx * 64;
        for bit in 0..64 {
            let global_idx = base_idx + bit;
            if global_idx >= total {
                return None;
            }
            if word & 1 != 0 {
                if run == 0 {
                    start_idx = global_idx;
                }
                run += 1;
                if run >= count {
                    for j in 0..count {
                        let idx = start_idx + j;
                        bitmap[idx / 64] &= !(1u64 << (idx % 64));
                    }
                    return Some(base_addr + (start_idx as u64) * 4096);
                }
            } else {
                run = 0;
            }
            word >>= 1;
        }
    }
    None
}

/// Free `count` contiguous physical pages.
///
/// # Safety
///
/// `addr` must have been previously allocated via `alloc_phys_contig`.
pub unsafe fn free_phys_contig(addr: u64, count: usize) {
    let base_addr = base();
    let total = total_pages();
    let bitmap = unsafe { bitmap_slice() };
    let start_idx = ((addr - base_addr) / 4096) as usize;
    for j in 0..count {
        let idx = start_idx + j;
        if idx < total {
            bitmap[idx / 64] |= 1u64 << (idx % 64);
        }
    }
}

/// Return (total_pages, free_pages) for diagnostic use.
pub fn stats() -> (usize, usize) {
    let total = total_pages();
    let bitmap = unsafe { bitmap_slice() };
    let mut free = 0usize;
    for chunk in bitmap.iter() {
        free += chunk.count_ones() as usize;
    }
    (total, free)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_range_basics() {
        let mut mmap = PhysicalMemoryMap::new();
        mmap.add(0x40000000, 0x50000000);
        assert_eq!(mmap.count, 1);
        assert_eq!(mmap.ranges[0].start, 0x40000000);
        assert_eq!(mmap.ranges[0].end, 0x50000000);
    }
}
