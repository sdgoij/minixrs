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
            assert!(a >= 0x1000 && a < 0x1000 + 0x10000);
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
}
