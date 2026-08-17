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

    pub fn total_available(&self) -> u64 {
        self.ranges[..self.count]
            .iter()
            .map(|r| r.end - r.start)
            .sum()
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

/// Size (bytes) of the *usable* free-RAM window — the allocator's bitmap
/// lives at the top of the window, and aliases must never wrap onto it.
/// Equals `bitmap_addr - base`.
pub fn usable_size() -> u64 {
    let state = ALLOC.get();
    let bitmap_addr = unsafe { read_field(&raw const (*state).bitmap_ptr) } as u64;
    let base = unsafe { read_field(&raw const (*state).base) };
    bitmap_addr.saturating_sub(base)
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

/// Cached low-GB alias window geometry. The kernel builds the alias tables
/// from its own physical allocator (`create_low_gb_pmd_table`), and VM's
/// teardown walks must use the SAME window to recognize alias leaves. VM's
/// copy of the arch allocator is never initialized (aarch64 `init_phys_alloc`
/// is a no-op), so `crate::alloc::base()`/`usable_size()` read zeros there;
/// both sides therefore read this cache, set by the kernel at boot and by VM
/// via the VM_PAGING_MEMINFO kernel call.
static ALIAS_BASE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static ALIAS_USABLE: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Record the low-GB alias window: `base` is the kernel allocator's first
/// frame, `usable` its bitmap-excluded size. Must be called by the kernel
/// after `init_allocator` and by VM after querying the kernel.
pub fn set_alias_window(base: u64, usable: u64) {
    ALIAS_BASE.store(base, core::sync::atomic::Ordering::Relaxed);
    ALIAS_USABLE.store(usable, core::sync::atomic::Ordering::Relaxed);
}

/// Return the cached (base, usable) alias window.
pub fn alias_window() -> (u64, u64) {
    (
        ALIAS_BASE.load(core::sync::atomic::Ordering::Relaxed),
        ALIAS_USABLE.load(core::sync::atomic::Ordering::Relaxed),
    )
}

/// True when `frame` is a shared low-GB alias/identity frame for user VA
/// `va` — a frame no process owns: the device-MMIO identity window
/// (0x08000000..0x10000000), the RAM identity case (frame == va), or a RAM
/// alias frame from `create_low_gb_pmd_table`. Single source of truth for
/// `pte_user_owned` (teardown walks must not free these) and the aarch64
/// fork (alias leaves are shared verbatim, not deep-copied). Returns false
/// when the window is unknown (host builds — the cache is never set), so
/// callers fall back to treating the page as process-owned.
pub fn is_alias_frame(frame: u64, va: u64) -> bool {
    const DEV_BASE: u64 = 0x0800_0000;
    const DEV_END: u64 = 0x1000_0000;
    const USER_LOW: u64 = 0x100_0000;
    if frame == va {
        return true; // identity mapping (RAM identity or device MMIO)
    }
    if (DEV_BASE..DEV_END).contains(&va) {
        return true; // device MMIO window
    }
    if va < USER_LOW {
        return false;
    }
    let (win_base, usable) = alias_window();
    let win_size = (usable / 0x20_0000) * 0x20_0000;
    win_size != 0 && frame == win_base + ((va - USER_LOW) % win_size)
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

/// Serializes tests that mutate the shared alias-window cache (the cache is
/// a process-global, and cargo runs the crate's tests in parallel).
/// no_std-compatible spinlock with a Drop guard (a panic releases it).
#[cfg(test)]
pub(crate) struct WindowTestLock(core::sync::atomic::AtomicBool);

#[cfg(test)]
impl WindowTestLock {
    pub(crate) const fn new() -> Self {
        Self(core::sync::atomic::AtomicBool::new(false))
    }

    pub(crate) fn lock(&self) -> WindowTestGuard<'_> {
        while self.0.swap(true, core::sync::atomic::Ordering::Acquire) {
            core::hint::spin_loop();
        }
        WindowTestGuard(self)
    }
}

#[cfg(test)]
pub(crate) struct WindowTestGuard<'a>(&'a WindowTestLock);

#[cfg(test)]
impl Drop for WindowTestGuard<'_> {
    fn drop(&mut self) {
        self.0.0.store(false, core::sync::atomic::Ordering::Release);
    }
}

#[cfg(test)]
pub(crate) static WINDOW_TEST_LOCK: WindowTestLock = WindowTestLock::new();

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

    #[test]
    fn test_is_alias_frame() {
        let _guard = super::WINDOW_TEST_LOCK.lock();
        // Unknown window (host default) — nothing is an alias.
        set_alias_window(0, 0);
        assert!(!is_alias_frame(0x5000, 0x1000000));
        assert!(!is_alias_frame(0x5000, 0x20000000));

        let base = 0x40_0000_0000u64;
        let usable = 0x1000_0000u64; // 256 MiB -> window 0x10000000
        set_alias_window(base, usable);

        // RAM alias frame: win_base + ((va - USER_LOW) % win_size).
        let va = 0x2000_0000u64;
        let alias = base + ((va - 0x100_0000) % 0x1000_0000);
        assert!(is_alias_frame(alias, va));
        assert!(
            !is_alias_frame(alias + 0x1000, va),
            "neighbor frame is owned"
        );

        // Device MMIO window: any leaf in it is shared.
        assert!(is_alias_frame(0x900_0000, 0x900_0000));
        assert!(
            is_alias_frame(0x7000, 0x900_0000),
            "dev window, other frame"
        );

        // Identity (frame == va) is shared.
        assert!(is_alias_frame(0x0123_4000, 0x0123_4000));

        // Below USER_LOW: never an alias.
        assert!(!is_alias_frame(base, 0x1000));

        set_alias_window(0, 0);
    }
}
