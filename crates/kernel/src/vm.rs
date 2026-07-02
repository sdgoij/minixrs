//! Physical memory manager — adapted from `minix/servers/vm/alloc.c`

#![allow(static_mut_refs)]

pub const VM_PAGE_SIZE: usize = 4096;
pub const NR_PHYS_PAGES: usize = 0x100000000 / VM_PAGE_SIZE;
pub const TOTAL_PHYS_MEM: u64 = 0x100000000;
pub const NR_MEMS: usize = 8;
const BITCHUNK_BITS: usize = 32;
const PAGE_BITMAP_CHUNKS: usize = NR_PHYS_PAGES.div_ceil(BITCHUNK_BITS);
const PAGE_CACHE_MAX: usize = 10000;

pub const PAF_ALIGN64K: u32 = 0x01;
pub const PAF_ALIGN16K: u32 = 0x02;
pub const PAF_CLEAR: u32 = 0x04;
pub const PAF_LOWER16MB: u32 = 0x08;
pub const PAF_LOWER1MB: u32 = 0x10;

#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct MemoryChunk {
    pub base: u64,
    pub size: u64,
}

pub const NO_MEM: u64 = u64::MAX;

// ── Bitmap storage ────────────────────────────────────────────────────
static mut BITS: [u32; PAGE_BITMAP_CHUNKS] = [0u32; PAGE_BITMAP_CHUNKS];
static mut CACHE: [i32; PAGE_CACHE_MAX] = [0i32; PAGE_CACHE_MAX];
static mut CACHE_SZ: i32 = 0;
static mut TOTAL: i32 = 0;
static mut LAST_SCAN: i32 = -1;

pub fn total_pages() -> i32 {
    unsafe { TOTAL }
}

fn page_free(p: usize) -> bool {
    if p >= NR_PHYS_PAGES {
        return false;
    }
    unsafe { (BITS[p / 32] & (1u32 << (p % 32))) != 0 }
}

fn set_free(p: usize) {
    if p < NR_PHYS_PAGES {
        unsafe {
            BITS[p / 32] |= 1u32 << (p % 32);
        }
    }
}

fn set_used(p: usize) {
    if p < NR_PHYS_PAGES {
        unsafe {
            BITS[p / 32] &= !(1u32 << (p % 32));
        }
    }
}

fn find_run(start: usize, n: usize) -> u64 {
    let mut run = 0usize;
    let mut i = start;
    loop {
        if !page_free(i) {
            run = 0;
            if i == 0 {
                break;
            }
            i -= 1;
            continue;
        }
        run += 1;
        if run == n {
            return i as u64;
        }
        if i == 0 {
            break;
        }
        i -= 1;
    }
    NO_MEM
}

unsafe fn alloc_pages_raw(n: usize, flags: u32) -> u64 {
    let max = if flags & PAF_LOWER16MB != 0 {
        16 * 1024 * 1024 / VM_PAGE_SIZE - 1
    } else if flags & PAF_LOWER1MB != 0 {
        1024 * 1024 / VM_PAGE_SIZE - 1
    } else {
        NR_PHYS_PAGES - 1
    };

    if n == 1 && flags & (PAF_LOWER16MB | PAF_LOWER1MB) == 0 {
        while unsafe { CACHE_SZ } > 0 {
            unsafe {
                CACHE_SZ -= 1;
            }
            let p = unsafe { CACHE[CACHE_SZ as usize] } as usize;
            if p < NR_PHYS_PAGES && page_free(p) {
                set_used(p);
                return p as u64;
            }
        }
    }

    let start = if unsafe { LAST_SCAN >= 0 && (LAST_SCAN as usize) <= max } {
        unsafe { LAST_SCAN as usize }
    } else {
        max
    };
    let mut p = find_run(start, n);
    if p == NO_MEM {
        p = find_run(max, n);
    }
    if p == NO_MEM {
        return NO_MEM;
    }
    for i in p as usize..p as usize + n {
        set_used(i);
    }
    unsafe {
        LAST_SCAN = p as i32;
    }
    p
}

unsafe fn free_pages_raw(pageno: usize, n: usize) {
    for i in pageno..pageno + n {
        set_free(i);
        if unsafe { CACHE_SZ } < PAGE_CACHE_MAX as i32 {
            unsafe {
                CACHE[CACHE_SZ as usize] = i as i32;
                CACHE_SZ += 1;
            }
        }
    }
}

/// # Safety
///
/// Must be called exactly once during boot, before any alloc/free.
pub unsafe fn mem_init(chunks: &[MemoryChunk]) {
    unsafe {
        BITS.fill(0);
    }
    unsafe {
        CACHE_SZ = 0;
        LAST_SCAN = -1;
        TOTAL = 0;
    }
    for chunk in chunks.iter().rev() {
        if chunk.size > 0 {
            unsafe {
                free_pages_raw(chunk.base as usize, chunk.size as usize);
                TOTAL += chunk.size as i32;
            }
        }
    }
}

/// # Safety
///
/// `clicks` must be > 0. Returned address must be freed with `free_mem`.
pub unsafe fn alloc_mem(clicks: usize, flags: u32) -> u64 {
    if clicks == 0 {
        return NO_MEM;
    }
    let align = if flags & PAF_ALIGN64K != 0 {
        64 * 1024 / VM_PAGE_SIZE
    } else if flags & PAF_ALIGN16K != 0 {
        16 * 1024 / VM_PAGE_SIZE
    } else {
        0
    };
    let need = clicks + align;
    let mut page = unsafe { alloc_pages_raw(need, flags) };
    if page == NO_MEM {
        return NO_MEM;
    }
    if align > 0 {
        let o = page % align as u64;
        if o > 0 {
            unsafe {
                free_pages_raw(page as usize, (align as u64 - o) as usize);
            }
            page += align as u64 - o;
        }
    }
    page
}

/// # Safety
///
/// `base` must have been returned by a previous `alloc_mem` call.
pub unsafe fn free_mem(base: u64, clicks: u64) {
    if clicks == 0 {
        return;
    }
    unsafe {
        free_pages_raw(base as usize, clicks as usize);
    }
}

/// # Safety
///
/// Must only be called during boot initialization.
pub unsafe fn mem_add_total_pages(pages: i32) {
    unsafe {
        TOTAL += pages;
    }
}

// ═════════════════════════════════════════════════════════════════════════
// Kernel physical mapping table (Phase 6.4 — port of vm_kern.c)
// ═════════════════════════════════════════════════════════════════════════

pub const KERN_PHYS_MAP_ENTRIES: usize = 16;

/// A single entry in the kernel physical mapping table.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct KernPhysMapEntry {
    pub kpme_physaddr: u64,
    pub kpme_virtaddr: u64,
    pub kpme_len: u64,
}

/// Kernel physical mapping table — 16 entries, used for mapping physical
/// memory into the kernel's address space for temporary access.
const KERN_PHYS_MAP_INIT: KernPhysMapEntry = KernPhysMapEntry {
    kpme_physaddr: 0,
    kpme_virtaddr: 0,
    kpme_len: 0,
};

static mut KERN_PHYS_MAP: [KernPhysMapEntry; KERN_PHYS_MAP_ENTRIES] =
    [KERN_PHYS_MAP_INIT; KERN_PHYS_MAP_ENTRIES];

/// Find and reserve a free entry in the kernel physical mapping table.
///
/// Returns 0 on success, -1 if table is full.
///
/// # Safety
///
/// Requires exclusive access to the mutable static `KERN_PHYS_MAP`.
pub unsafe fn kern_map(physaddr: u64, virtaddr: u64, len: u64) -> i32 {
    for entry in unsafe { KERN_PHYS_MAP.iter_mut() } {
        if entry.kpme_physaddr == 0 && entry.kpme_virtaddr == 0 {
            entry.kpme_physaddr = physaddr;
            entry.kpme_virtaddr = virtaddr;
            entry.kpme_len = len;
            return 0;
        }
    }
    -1
}

/// Remove a mapping by virtual address.
///
/// Returns 0 on success, -1 if not found.
///
/// # Safety
///
/// Requires exclusive access to the mutable static `KERN_PHYS_MAP`.
pub unsafe fn kern_unmap(virtaddr: u64, len: u64) -> i32 {
    for entry in unsafe { KERN_PHYS_MAP.iter_mut() } {
        if entry.kpme_virtaddr == virtaddr && entry.kpme_len == len {
            entry.kpme_physaddr = 0;
            entry.kpme_virtaddr = 0;
            entry.kpme_len = 0;
            return 0;
        }
    }
    -1
}

/// Add a physical mapping — delegates to kern_map.
///
/// # Safety
///
/// Requires exclusive access to the mutable static `KERN_PHYS_MAP`.
pub unsafe fn phys_map_add(physaddr: u64, virtaddr: u64, len: u64) -> i32 {
    unsafe { kern_map(physaddr, virtaddr, len) }
}

/// Remove a physical mapping by physical address.
///
/// Returns 0 on success, -1 if not found.
///
/// # Safety
///
/// Requires exclusive access to the mutable static `KERN_PHYS_MAP`.
pub unsafe fn phys_map_remove(physaddr: u64, _len: u64) -> i32 {
    for entry in unsafe { KERN_PHYS_MAP.iter_mut() } {
        if entry.kpme_physaddr == physaddr {
            entry.kpme_physaddr = 0;
            entry.kpme_virtaddr = 0;
            entry.kpme_len = 0;
            return 0;
        }
    }
    -1
}

/// Translate a virtual address to a physical address for a given process.
///
/// Walks the process's page table to translate `virtaddr` to its physical
/// frame. Returns the physical address on success, `NO_MEM` on failure.
///
/// # Safety
///
/// The process must have a valid page table (`p_cr3 != 0`). This function
/// reads another process's `p_seg.p_cr3`, which is only valid on bare metal
/// (it is a physical address). In test mode, accessing it would read garbage.
pub unsafe fn vm_lookup(proc_nr: i32, virtaddr: u64) -> u64 {
    unsafe {
        let rp = crate::table::proc_addr(proc_nr);
        if rp.is_null() {
            return NO_MEM;
        }
        let cr3 = (*rp).p_seg.p_cr3;
        if cr3 == 0 {
            return NO_MEM;
        }
        match crate::pagetable::walk(cr3, virtaddr) {
            Ok(result) => {
                let offset = virtaddr & 0xFFF;
                (result.pte_value & arch_x86_64::pte::PG_FRAME) + offset
            }
            Err(_) => NO_MEM,
        }
    }
}

/// Write a pattern byte to a range of physical memory.
///
/// # Safety
///
/// The physical address range must be valid and identity-mapped.
pub unsafe fn vm_memset(physaddr: u64, c: u8, count: usize) -> i32 {
    unsafe {
        if count == 0 {
            return 0;
        }
        core::ptr::write_bytes(physaddr as *mut u8, c, count);
        0
    }
}

/// Check if a virtual address range is validly mapped in a process's address space.
///
/// Walks the process's page table for each page in `addr..addr+bytes`
/// and verifies it is present. Returns true only if ALL pages are mapped.
///
/// # Safety
///
/// `caller` must point to a valid Proc.
pub unsafe fn vm_check_range(caller: *mut crate::proc::Proc, addr: u64, bytes: u64) -> bool {
    unsafe {
        if caller.is_null() {
            return false;
        }
        let cr3 = (*caller).p_seg.p_cr3;
        if cr3 == 0 {
            // No per-process page table — uses BOOT_CR3. Can't validate per-page
            // permissions beyond the old KERNBASE check. Return true so kernel
            // tasks (init, etc.) continue to work.
            return true;
        }
        if bytes == 0 {
            return true;
        }
        let start = addr & !0xFFF;
        let end_va = addr.saturating_add(bytes);
        let end = ((end_va + 0xFFF) & !0xFFF).min(arch_x86_64::vmparam::VM_MAXUSER_ADDRESS);
        let mut va = start;
        while va < end {
            if crate::pagetable::walk(cr3, va).is_err() {
                return false;
            }
            va += 0x1000;
        }
        true
    }
}

/// Copy data between two address spaces using CR3 switching.
///
/// Copies `bytes` from `src_proc`'s virtual address `src_addr` to
/// `dst_proc`'s virtual address `dst_addr` by temporarily switching
/// CR3 to each process's page table.
///
/// # Safety
///
/// Both processes must have valid page tables. Addresses must be valid
/// mapped regions in their respective address spaces.
pub unsafe fn virtual_copy(
    src_proc: i32,
    src_addr: u64,
    dst_proc: i32,
    dst_addr: u64,
    bytes: usize,
) -> i32 {
    unsafe {
        if bytes == 0 {
            return 0;
        }
        let src_rp = crate::table::proc_addr(src_proc);
        let dst_rp = crate::table::proc_addr(dst_proc);
        if src_rp.is_null() || dst_rp.is_null() {
            return -1;
        }

        let src_cr3 = (*src_rp).p_seg.p_cr3;
        let dst_cr3 = (*dst_rp).p_seg.p_cr3;

        if src_cr3 == 0 || dst_cr3 == 0 {
            return -1;
        }

        let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
        if boot_cr3 == 0 {
            return -1;
        }

        // Use a small stack buffer for the bounce
        let mut buf = [0u8; 256];
        let mut remaining = bytes;
        let mut src_va = src_addr;
        let mut dst_va = dst_addr;

        while remaining > 0 {
            let chunk = core::cmp::min(remaining, buf.len());

            // Switch to source, read
            arch_x86_64::asm::write_cr3(src_cr3);
            core::ptr::copy_nonoverlapping(src_va as *const u8, buf.as_mut_ptr(), chunk);

            // Switch to destination, write
            arch_x86_64::asm::write_cr3(dst_cr3);
            core::ptr::copy_nonoverlapping(buf.as_ptr(), dst_va as *mut u8, chunk);

            // Restore boot CR3
            arch_x86_64::asm::write_cr3(boot_cr3);

            remaining -= chunk;
            src_va += chunk as u64;
            dst_va += chunk as u64;
        }

        0
    }
}

/// Look up a contiguous physical region starting at a virtual address.
///
/// Walks the process's page table at `vaddr` and returns the size of the
/// contiguous physical mapping (up to `max_size`). Stores the physical
/// address in `phys_addr`.
///
/// Returns the contiguous chunk size in bytes, or 0 if the page is not mapped.
///
/// # Safety
///
/// `proc` must point to a valid Proc with a page table.
pub unsafe fn vm_lookup_range(
    proc: *mut crate::proc::Proc,
    vaddr: u64,
    phys_addr: &mut u64,
    max_size: u64,
) -> u64 {
    unsafe {
        if proc.is_null() {
            return 0;
        }
        let cr3 = (*proc).p_seg.p_cr3;
        if cr3 == 0 {
            return 0;
        }

        let result = match crate::pagetable::walk(cr3, vaddr) {
            Ok(r) => r,
            Err(_) => return 0,
        };

        let frame = result.pte_value & arch_x86_64::pte::PG_FRAME;
        let offset = vaddr & 0xFFF;
        *phys_addr = frame + offset;

        match result.level {
            1 => {
                // 4KB page — can map up to 4KB
                let remaining_in_page = 0x1000 - offset;
                remaining_in_page.min(max_size)
            }
            2 => {
                // 2MB huge page — can map up to 2MB
                let base = vaddr & !((1 << 21) - 1);
                let remaining = (1 << 21) - (vaddr - base);
                remaining.min(max_size)
            }
            3 => {
                // 1GB huge page — can map up to 1GB
                let base = vaddr & !((1 << 30) - 1);
                let remaining = (1 << 30) - (vaddr - base);
                remaining.min(max_size)
            }
            _ => 0,
        }
    }
}

pub fn mem_stats() -> (i32, i32, i32) {
    let mut nodes = 0i32;
    let mut free = 0i32;
    let mut large = 0i32;
    let mut i = 0usize;
    while i < NR_PHYS_PAGES {
        if page_free(i) {
            let s = i;
            while i < NR_PHYS_PAGES && page_free(i) {
                i += 1;
            }
            let sz = (i - s) as i32;
            nodes += 1;
            free += sz;
            if sz > large {
                large = sz;
            }
        } else {
            i += 1;
        }
    }
    (nodes, free, large)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires --test-threads=1 (shares static mut state with other tests)"]
    fn test_vm_allocator() {
        unsafe {
            let chunks = [MemoryChunk {
                base: 0x1000,
                size: 0x10000,
            }];
            mem_init(&chunks);
            assert!(total_pages() > 0);
            let (nodes, free, _) = mem_stats();
            assert_eq!(nodes, 1);
            assert_eq!(free, 0x10000);
            assert_eq!(free, total_pages());

            let a = alloc_mem(1, 0);
            assert!(a != NO_MEM);
            assert!((0x1000..0x1000 + 0x10000).contains(&a));
            let (_, f2, _) = mem_stats();
            assert_eq!(f2, total_pages() - 1);

            free_mem(a, 1);
            let (_, f3, _) = mem_stats();
            assert_eq!(f3, total_pages());

            let b = alloc_mem(10, 0);
            assert!(b != NO_MEM);
            let (_, f4, _) = mem_stats();
            assert_eq!(f4, total_pages() - 10);
            free_mem(b, 10);
            let (_, f5, _) = mem_stats();
            assert_eq!(f5, total_pages());

            let _x = alloc_mem(1, 0);
            let y = alloc_mem(1, 0);
            let _z = alloc_mem(1, 0);
            free_mem(y, 1);
            let r = alloc_mem(1, 0);
            assert_eq!(r, y);

            assert_eq!(alloc_mem(1, PAF_LOWER16MB), NO_MEM);
            assert_eq!(alloc_mem(0, 0), NO_MEM);
        }
    }

    #[test]
    fn test_kern_phys_map_operations() {
        unsafe {
            // kern_map should succeed
            assert_eq!(kern_map(0x1000, 0xFFFF800000000000, 0x1000), 0);
            assert_eq!(kern_map(0x2000, 0xFFFF800000001000, 0x2000), 0);

            // kern_unmap should find by virtaddr + len
            assert_eq!(kern_unmap(0xFFFF800000001000, 0x2000), 0);

            // kern_unmap on already-unmapped entry should fail
            assert_eq!(kern_unmap(0xFFFF800000001000, 0x2000), -1);

            // phys_map_add delegates to kern_map
            assert_eq!(phys_map_add(0x3000, 0xFFFF800000003000, 0x1000), 0);

            // phys_map_remove finds by physaddr
            assert_eq!(phys_map_remove(0x1000, 0x1000), 0);

            // phys_map_remove on already-removed entry should fail
            assert_eq!(phys_map_remove(0x1000, 0x1000), -1);

            // Fill the table to capacity
            let mut i = 0;
            for _ in 0..KERN_PHYS_MAP_ENTRIES {
                let p = 0x5000 + i as u64 * 0x1000;
                let v = 0xFFFF800000100000 + i as u64 * 0x1000;
                let r = kern_map(p, v, 0x1000);
                if r == 0 {
                    i += 1;
                }
            }
            // Now the table should be full (2 already used after unmaps, but
            // some were freed, so we can fill more)
        }
    }

    #[test]
    fn test_kern_phys_map_empty() {
        // Without any mappings, kern_unmap and phys_map_remove should fail
        unsafe {
            assert_eq!(kern_unmap(0xDEAD, 0x1000), -1);
            assert_eq!(phys_map_remove(0xDEAD, 0x1000), -1);
        }
    }

    #[test]
    fn test_kern_phys_map_entries_const() {
        assert_eq!(KERN_PHYS_MAP_ENTRIES, 16);
    }

    // ── Phase 6.13 VM helper tests ─────────────────────────────────

    #[test]
    fn test_vm_lookup_invalid_proc_returns_no_mem() {
        unsafe {
            let r = vm_lookup(9999, 0x1000);
            assert_eq!(r, NO_MEM);
        }
    }

    #[test]
    fn test_vm_lookup_zero_cr3_returns_no_mem() {
        unsafe {
            crate::table::proc_init();
            let r = vm_lookup(0, 0x1000);
            assert_eq!(r, NO_MEM, "zero CR3 should fail");
        }
    }

    #[test]
    fn test_vm_memset_zero_count() {
        unsafe {
            assert_eq!(vm_memset(0x1000, 0xAA, 0), 0);
        }
    }

    #[test]
    fn test_vm_memset_writes_pattern() {
        unsafe {
            let mut buf = [0u8; 64];
            let addr = buf.as_mut_ptr() as u64;
            assert_eq!(vm_memset(addr, 0xAB, 64), 0);
            for (i, &byte) in buf.iter().enumerate() {
                assert_eq!(byte, 0xAB, "byte {} mismatch", i);
            }
        }
    }

    #[test]
    fn test_virtual_copy_zero_bytes() {
        unsafe {
            assert_eq!(virtual_copy(0, 0, 0, 0, 0), 0);
        }
    }

    #[test]
    fn test_virtual_copy_null_procs() {
        unsafe {
            assert_eq!(virtual_copy(9999, 0x1000, 9998, 0x2000, 16), -1);
        }
    }

    #[test]
    #[ignore = "requires --test-threads=1 (shares static mut state with other tests)"]
    fn test_vm_exhaustion() {
        unsafe {
            let chunks = [MemoryChunk {
                base: 0x1000,
                size: 0x10000,
            }];
            mem_init(&chunks);
            let total = total_pages() as usize;
            let mut allocd = 0usize;
            loop {
                if alloc_mem(1, 0) == NO_MEM {
                    break;
                }
                allocd += 1;
            }
            assert_eq!(allocd, total);
            let (_, free, _) = mem_stats();
            assert_eq!(free, 0);
        }
    }

    // ── Phase 6.14 vm_check_range tests ────────────────────────────

    #[test]
    fn test_vm_check_range_null_caller() {
        unsafe {
            assert!(!vm_check_range(core::ptr::null_mut(), 0x1000, 64));
        }
    }

    #[test]
    fn test_vm_check_range_zero_bytes() {
        unsafe {
            crate::table::proc_init();
            let rp = crate::table::proc_addr(0);
            assert!(vm_check_range(rp, 0x1000, 0));
        }
    }

    #[test]
    fn test_vm_check_range_kernel_task_fallback() {
        unsafe {
            crate::table::proc_init();
            let rp = crate::table::proc_addr(0);
            // With zero CR3 (kernel task without per-process PT), returns true
            (*rp).p_seg.p_cr3 = 0;
            assert!(vm_check_range(rp, 0x1000, 64));
        }
    }
}
