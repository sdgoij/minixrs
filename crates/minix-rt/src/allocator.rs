//! Mmap-backed free-list allocator — the `rt` global allocator.
//!
//! Replaces the original bump allocator, which never freed memory: servers
//! and userland binaries that link `minix-rt` with the `rt` feature now
//! reclaim freed blocks instead of leaking them until process exit.
//!
//! The allocator maps page-aligned chunks via the VM server's `mmap`
//! (see `crate::vmem`) and carves them into variable-size blocks served
//! from a first-fit free list; a chunk whose blocks are all free is
//! returned to the kernel with `munmap`, so the heap can shrink as well
//! as grow. This mirrors the fork's std PAL allocator
//! (`rust/library/std/src/sys/pal/minix/alloc.rs`) so both Rust allocator
//! paths share one shape.
//!
//! Thread-safe: a futex-backed lock serializes the heap metadata (the
//! Minix kernel has 1:1 threads — see THREADS.md).

use core::alloc::Layout;

#[cfg(all(target_os = "minix", feature = "rt"))]
use crate::BrkLock;

/// Header size (two `usize` words: `size` at +0, `flags` at +8). The header
/// stays at the block start for the block's whole lifetime so free-list walks
/// and coalescing can always read it.
const HDR: usize = 16;

/// Block flag: the block is allocated.
const IN_USE: usize = 1;
/// Block flag: first block of an mmap chunk (its bounds live in the chunk
/// table below).
const CHUNK_START: usize = 2;

/// Minimum payload offset from the block start. The payload is aligned up and
/// the block base is recorded at `payload - HDR`; forcing at least 32 bytes of
/// header+slack keeps that back-pointer clear of the block header even for
/// 16-byte-aligned payloads.
const MIN_PAYLOAD_OFF: usize = 32;

/// Smallest block we ever split off (header + minimal payload).
const MIN_BLOCK: usize = 48;

/// Size of an mmap chunk: 1 MiB (256 pages). Large chunks keep the region
/// count low (the VM server tracks at most `MAX_REGIONS` per process).
const CHUNK_SIZE: usize = 1024 * 1024;

const PAGE_SIZE: usize = 4096;

/// Maximum tracked chunks. The VM server caps live regions at
/// `MAX_REGIONS` (16) per process, so this is generous.
const MAX_CHUNKS: usize = 16;

/// A live mmap chunk, for returning fully-free chunks to the kernel.
#[repr(C)]
#[derive(Clone, Copy)]
struct Chunk {
    base: usize,
    len: usize,
}

/// The heap's mutable state: free-list head + live chunks.
///
/// Not thread-safe by itself; the global instance is guarded by a futex
/// lock. Kept free of syscalls so the block/chunk logic is host-testable.
struct Heap {
    /// Head of the free list (0 = empty). A free block stores the next
    /// block's address at `block + HDR` (its payload area).
    free_head: usize,
    /// Live mmap chunks; `len == 0` marks a free slot.
    chunks: [Chunk; MAX_CHUNKS],
}

impl Heap {
    const fn new() -> Self {
        Heap {
            free_head: 0,
            chunks: [Chunk { base: 0, len: 0 }; MAX_CHUNKS],
        }
    }

    // ---- free list ----

    /// Push a free block onto the free list (head insert).
    unsafe fn free_push(&mut self, b: usize) {
        unsafe {
            let head = self.free_head;
            *core::ptr::with_exposed_provenance_mut::<usize>(b + HDR) = head;
            self.free_head = b;
        }
    }

    /// Walk the free list for the first block with `hdr_size >= need`,
    /// unlinking it. Returns the block, or 0.
    unsafe fn free_pop_fit(&mut self, need: usize) -> usize {
        unsafe {
            let mut prev = 0usize;
            let mut cur = self.free_head;
            while cur != 0 {
                if hdr_size(cur) >= need {
                    let next = *core::ptr::with_exposed_provenance::<usize>(cur + HDR);
                    if prev == 0 {
                        self.free_head = next;
                    } else {
                        *core::ptr::with_exposed_provenance_mut::<usize>(prev + HDR) = next;
                    }
                    return cur;
                }
                prev = cur;
                cur = *core::ptr::with_exposed_provenance::<usize>(cur + HDR);
            }
            0
        }
    }

    /// Unlink `b` from the free list (used when coalescing or munmapping a
    /// chunk).
    unsafe fn free_unlink(&mut self, b: usize) {
        unsafe {
            let mut prev = 0usize;
            let mut cur = self.free_head;
            while cur != 0 && cur != b {
                prev = cur;
                cur = *core::ptr::with_exposed_provenance::<usize>(cur + HDR);
            }
            if cur == b {
                let next = *core::ptr::with_exposed_provenance::<usize>(b + HDR);
                if prev == 0 {
                    self.free_head = next;
                } else {
                    *core::ptr::with_exposed_provenance_mut::<usize>(prev + HDR) = next;
                }
            }
        }
    }

    // ---- chunk table ----

    /// Record a new mmap chunk. Returns false when the table is full (the
    /// chunk is then simply never returned to the kernel).
    unsafe fn chunk_add(&mut self, base: usize, len: usize) -> bool {
        unsafe {
            for i in 0..MAX_CHUNKS {
                if self.chunks[i].len == 0 {
                    self.chunks[i] = Chunk { base, len };
                    return true;
                }
            }
            false
        }
    }

    /// Find the chunk containing `addr`.
    fn chunk_find(&self, addr: usize) -> Option<(usize, usize)> {
        for i in 0..MAX_CHUNKS {
            let c = self.chunks[i];
            if c.len != 0 && addr >= c.base && addr < c.base + c.len {
                return Some((c.base, c.len));
            }
        }
        None
    }

    /// Drop the chunk at `base` from the table.
    unsafe fn chunk_remove(&mut self, base: usize) {
        unsafe {
            for i in 0..MAX_CHUNKS {
                if self.chunks[i].base == base {
                    self.chunks[i].len = 0;
                    return;
                }
            }
        }
    }

    // ---- allocation ----

    /// Carve a used block out of free block `block` (which must hold the
    /// request — see [`Heap::alloc`]'s `need`), splitting off a tail free
    /// block when the leftover is large enough. Returns the aligned payload.
    unsafe fn carve(&mut self, block: usize, layout: Layout) -> *mut u8 {
        unsafe {
            let size = layout.size().max(1);
            let align = layout.align().max(16);
            let bsize = hdr_size(block);
            let flags = hdr_flags(block);

            // Aligned payload, at least MIN_PAYLOAD_OFF from the block start
            // so the back-pointer at `payload - HDR` clears the block header.
            let mut payload = align_up(block + HDR, align);
            if payload < block + MIN_PAYLOAD_OFF {
                payload = align_up(block + MIN_PAYLOAD_OFF, align);
            }
            let mut used_size = align_up(payload + size - block, 16);

            if bsize - used_size >= MIN_BLOCK {
                let tail = block + used_size;
                set_hdr(tail, bsize - used_size, 0);
                self.free_push(tail);
            } else {
                // No room for a tail block: the used block spans the whole
                // free block so the block walk never leaves a dead gap.
                used_size = bsize;
            }
            set_hdr(block, used_size, flags | IN_USE);

            *core::ptr::with_exposed_provenance_mut::<usize>(payload - HDR) = block;
            core::ptr::with_exposed_provenance_mut::<u8>(payload)
        }
    }

    /// Allocate `layout` from the existing free list. Returns the payload,
    /// or null when no free block is big enough (the caller must insert a
    /// chunk and retry).
    unsafe fn alloc(&mut self, layout: Layout) -> *mut u8 {
        unsafe {
            let size = layout.size().max(1);
            let align = layout.align().max(16);
            // The aligned payload lands at most `align + 16` bytes past the
            // block start (floor 32), so a fitting block must be at least
            // this large. Using the same formula for the carve keeps the
            // invariant that a freed block can satisfy the layout again.
            let need = align_up(size + align + 16, 16);
            let block = self.free_pop_fit(need);
            if block == 0 {
                return core::ptr::null_mut();
            }
            self.carve(block, layout)
        }
    }

    /// Record a freshly-mapped chunk as one free chunk-start block, ready
    /// for the next [`Heap::alloc`] to carve. Returns false when the chunk
    /// table is full.
    unsafe fn insert_chunk(&mut self, base: usize, len: usize) -> bool {
        unsafe {
            if !self.chunk_add(base, len) {
                return false;
            }
            set_hdr(base, len, CHUNK_START);
            self.free_push(base);
            true
        }
    }

    /// Free `ptr`, coalescing with the next block. Returns the chunk
    /// (`(base, len)`) to return to the kernel when it became fully free,
    /// or None.
    unsafe fn dealloc(&mut self, ptr: *mut u8) -> Option<(usize, usize)> {
        unsafe {
            if ptr.is_null() {
                return None;
            }
            let payload = ptr.addr();
            // SAFETY: `ptr` was returned by `alloc`; the back-pointer was
            // written at `payload - HDR` by `carve`.
            let block = *core::ptr::with_exposed_provenance::<usize>(payload - HDR);
            let size = hdr_size(block);
            let flags = hdr_flags(block);
            debug_assert!(flags & IN_USE != 0);
            set_hdr(block, size, flags & !IN_USE);

            let (cbase, clen) = self.chunk_find(block)?;
            let cend = cbase + clen;

            // Coalesce with the next block when it is free and inside the
            // chunk.
            let next = block + size;
            if next < cend && hdr_flags(next) & IN_USE == 0 {
                let nsize = hdr_size(next);
                self.free_unlink(next);
                set_hdr(block, size + nsize, 0);
            }

            if chunk_fully_free(cbase, cend) {
                // A fully-free chunk goes back to the kernel.
                let mut b = cbase;
                while b < cend {
                    self.free_unlink(b);
                    let bsize = hdr_size(b);
                    if bsize == 0 {
                        break;
                    }
                    b += bsize;
                }
                self.chunk_remove(cbase);
                return Some((cbase, clen));
            }

            self.free_push(block);
            None
        }
    }
}

/// True when every block in `[cbase, cend)` is free (so the chunk can be
/// returned to the kernel).
unsafe fn chunk_fully_free(cbase: usize, cend: usize) -> bool {
    let mut b = cbase;
    while b < cend {
        if hdr_flags(b) & IN_USE != 0 {
            return false;
        }
        let size = hdr_size(b);
        // A zero or out-of-range header means the walk left the chunk.
        if size == 0 || b + size > cend {
            return false;
        }
        b += size;
    }
    true
}

// ---- block helpers ----

#[inline]
fn hdr_size(b: usize) -> usize {
    unsafe { *core::ptr::with_exposed_provenance::<usize>(b) }
}

#[inline]
fn hdr_flags(b: usize) -> usize {
    unsafe { *core::ptr::with_exposed_provenance::<usize>(b + 8) }
}

#[inline]
unsafe fn set_hdr(b: usize, size: usize, flags: usize) {
    unsafe {
        *core::ptr::with_exposed_provenance_mut::<usize>(b) = size;
        *core::ptr::with_exposed_provenance_mut::<usize>(b + 8) = flags;
    }
}

#[inline]
fn align_up(n: usize, a: usize) -> usize {
    (n + a - 1) & !(a - 1)
}

// ---- global allocator (minix rt builds) ----

/// Map an anonymous, private, read/write chunk of `size` bytes via the VM
/// server. Returns the page-aligned base, or 0 on failure.
#[cfg(target_os = "minix")]
unsafe fn mmap_chunk(size: usize) -> usize {
    unsafe {
        let r = crate::vmem::mmap(
            core::ptr::null_mut(),
            size,
            crate::vmem::PROT_READ | crate::vmem::PROT_WRITE,
            crate::vmem::MAP_PRIVATE | crate::vmem::MAP_ANONYMOUS,
            -1,
            0,
        );
        let base = r.addr();
        if base == usize::MAX || base == 0 {
            return 0;
        }
        base
    }
}

/// Return a fully-free chunk to the kernel. No-op outside Minix (host
/// tests drive the heap with plain buffers).
#[cfg(target_os = "minix")]
unsafe fn release_chunk(base: usize, len: usize) {
    unsafe {
        let _ = crate::vmem::munmap(core::ptr::with_exposed_provenance_mut::<u8>(base), len);
    }
}

#[cfg(not(target_os = "minix"))]
unsafe fn release_chunk(_base: usize, _len: usize) {}

#[cfg(all(target_os = "minix", feature = "rt"))]
static LOCK: BrkLock = BrkLock::new();

#[cfg(all(target_os = "minix", feature = "rt"))]
static mut HEAP: Heap = Heap::new();

/// The global allocator entry points, guarded by [`LOCK`].
#[cfg(all(target_os = "minix", feature = "rt"))]
unsafe fn alloc_global(layout: Layout, zero: u8) -> *mut u8 {
    unsafe {
        LOCK.lock();
        let h = &mut *core::ptr::addr_of_mut!(HEAP);
        let mut p = h.alloc(layout);
        if p.is_null() {
            // No free block big enough: map a new chunk (at least a page for
            // the request, otherwise a full chunk for headroom).
            let size = layout.size().max(1);
            let align = layout.align().max(16);
            let need = align_up(size + align + 16, 16);
            let chunk_size = CHUNK_SIZE.max(align_up(need, PAGE_SIZE));
            let base = mmap_chunk(chunk_size);
            if base != 0 && h.insert_chunk(base, chunk_size) {
                p = h.alloc(layout);
            }
        }
        if !p.is_null() && zero != 0 {
            core::ptr::write_bytes(p, 0, layout.size());
        }
        LOCK.unlock();
        p
    }
}

/// The global allocator dealloc, guarded by [`LOCK`].
#[cfg(all(target_os = "minix", feature = "rt"))]
unsafe fn dealloc_global(ptr: *mut u8) {
    unsafe {
        LOCK.lock();
        let h = &mut *core::ptr::addr_of_mut!(HEAP);
        let released = h.dealloc(ptr);
        LOCK.unlock();
        if let Some((base, len)) = released {
            release_chunk(base, len);
        }
    }
}

/// The global allocator realloc, guarded by [`LOCK`].
#[cfg(all(target_os = "minix", feature = "rt"))]
unsafe fn realloc_global(ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
    unsafe {
        // In-place fast path: the payload fits in the current block.
        let payload = ptr.addr();
        let block = *core::ptr::with_exposed_provenance::<usize>(payload - HDR);
        let bsize = hdr_size(block);
        let padding = payload - block - HDR;
        let capacity = bsize - HDR - padding;
        if new_size <= capacity {
            return ptr;
        }
        let new_ptr = alloc_global(
            Layout::from_size_align_unchecked(new_size, layout.align()),
            0,
        );
        if !new_ptr.is_null() {
            core::ptr::copy_nonoverlapping(ptr, new_ptr, core::cmp::min(layout.size(), new_size));
            dealloc_global(ptr);
        }
        new_ptr
    }
}

/// Allocate `layout` from the global mmap-backed heap.
///
/// `rt` binaries do not link the `alloc` crate, so `#[global_allocator]`
/// is never invoked for them; this is the direct entry point for no_std
/// heap users (currently the alloc-churn QEMU probe).
///
/// # Safety
///
/// `layout` must have non-zero size and a power-of-two alignment, and the
/// result must be freed with [`dealloc`].
#[cfg(all(target_os = "minix", feature = "rt"))]
pub unsafe fn alloc(layout: Layout) -> *mut u8 {
    unsafe { alloc_global(layout, 0) }
}

/// Free `ptr` previously returned by [`alloc`].
///
/// # Safety
///
/// `ptr` must come from [`alloc`] (or the global allocator) and must not
/// have been freed already.
#[cfg(all(target_os = "minix", feature = "rt"))]
pub unsafe fn dealloc(ptr: *mut u8) {
    unsafe { dealloc_global(ptr) }
}

/// The `rt` global allocator.
#[cfg(all(target_os = "minix", feature = "rt"))]
pub struct MmapAllocator;

#[cfg(all(target_os = "minix", feature = "rt"))]
unsafe impl core::alloc::GlobalAlloc for MmapAllocator {
    #[inline]
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { alloc_global(layout, 0) }
    }

    #[inline]
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe { alloc_global(layout, 1) }
    }

    #[inline]
    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { dealloc_global(ptr) }
    }

    #[inline]
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe { realloc_global(ptr, layout, new_size) }
    }
}

/// The global allocator instance.
#[cfg(all(target_os = "minix", feature = "rt"))]
#[global_allocator]
static ALLOCATOR: MmapAllocator = MmapAllocator;

// Tests — the heap core is host-testable with plain buffers.

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    fn layout(size: usize, align: usize) -> Layout {
        unsafe { Layout::from_size_align_unchecked(size, align) }
    }

    /// A heap with one chunk backed by `buf`.
    fn heap_over(buf: &mut [u8]) -> Heap {
        let base = buf.as_mut_ptr() as usize;
        assert_eq!(base % 16, 0, "test chunk base must be 16-aligned");
        let mut h = Heap::new();
        assert!(
            unsafe { h.insert_chunk(base, buf.len()) },
            "test chunk must be accepted"
        );
        h
    }

    #[test]
    fn alloc_frees_and_reuses() {
        // The regression pin for the bump → free-list swap: a freed block
        // must be handed out again. The old bump allocator never reused.
        let mut buf = std::vec![0u8; 16384];
        let mut h = heap_over(&mut buf);
        let l = layout(64, 8);
        let a = unsafe { h.alloc(l) };
        let b = unsafe { h.alloc(l) };
        assert!(!a.is_null() && !b.is_null());
        assert_ne!(a, b);

        unsafe { h.dealloc(a) };
        let c = unsafe { h.alloc(l) };
        assert_eq!(c, a, "freed block must be reused, not freshly carved");
    }

    #[test]
    fn dealloc_coalesces_adjacent_blocks() {
        // Two adjacent frees must merge so a larger allocation fits. Freeing
        // in reverse order exercises forward coalescing (block a merges into
        // the already-free block b).
        let mut buf = std::vec![0u8; 16384];
        let mut h = heap_over(&mut buf);

        // a and b: 512 bytes each; c consumes the rest of the chunk so the
        // only reusable space is a + b coalesced. need(a) = need(b) = 544,
        // tail after both = 16384 - 1088 = 15296, c sized to use it exactly.
        let a = unsafe { h.alloc(layout(512, 8)) };
        let b = unsafe { h.alloc(layout(512, 8)) };
        let c = unsafe { h.alloc(layout(15264, 8)) };
        assert!(!a.is_null() && !b.is_null() && !c.is_null());

        // A request that only fits if a and b (plus headers) merged.
        let big = layout(512 + 512 - 32, 8); // need = 1088
        assert!(
            unsafe { h.alloc(big) }.is_null(),
            "must not fit while a, b, c are allocated"
        );

        // Freeing b first, then a, forward-coalesces them into one block.
        unsafe { h.dealloc(b) };
        unsafe { h.dealloc(a) };
        let merged = unsafe { h.alloc(big) };
        assert!(
            !merged.is_null(),
            "coalesced free space must satisfy the big allocation"
        );
    }

    #[test]
    fn alignment_is_respected() {
        let mut buf = std::vec![0u8; 65536];
        let mut h = heap_over(&mut buf);
        for align in [16usize, 32, 64, 256, 4096] {
            let p = unsafe { h.alloc(layout(1, align)) };
            assert!(!p.is_null());
            assert_eq!(
                p.addr() % align,
                0,
                "payload must be {align}-aligned (got 0x{:x})",
                p.addr()
            );
        }
    }

    #[test]
    fn split_leaves_reusable_tail() {
        let mut buf = std::vec![0u8; 16384];
        let mut h = heap_over(&mut buf);
        // A small allocation from a big chunk splits a tail free block.
        let a = unsafe { h.alloc(layout(32, 8)) };
        assert!(!a.is_null());
        // The tail must be usable by subsequent allocations.
        let b = unsafe { h.alloc(layout(4096, 8)) };
        let c = unsafe { h.alloc(layout(4096, 8)) };
        assert!(!b.is_null() && !c.is_null());
    }

    #[test]
    fn fully_free_chunk_is_returned() {
        let mut buf = std::vec![0u8; 65536];
        let mut h = heap_over(&mut buf);
        let base = buf.as_mut_ptr() as usize;
        let len = buf.len();

        let a = unsafe { h.alloc(layout(64, 8)) };
        let b = unsafe { h.alloc(layout(64, 8)) };
        assert!(!a.is_null() && !b.is_null());

        // Still partly in use: the chunk must not be released.
        assert!(unsafe { h.dealloc(a) }.is_none(), "chunk still in use");
        // Both freed: the chunk is fully free and must be handed back.
        assert_eq!(
            unsafe { h.dealloc(b) },
            Some((base, len)),
            "fully-free chunk must be released with its bounds"
        );
        // The chunk is gone from the table.
        assert!(h.chunk_find(base).is_none());
    }

    #[test]
    fn null_when_exhausted() {
        // A tiny chunk cannot satisfy a huge request.
        let mut buf = std::vec![0u8; 4096];
        let mut h = heap_over(&mut buf);
        let p = unsafe { h.alloc(layout(1 << 20, 8)) };
        assert!(p.is_null());
    }

    #[test]
    fn churn_keeps_data_intact() {
        let mut buf = std::vec![0u8; 262144];
        let mut h = heap_over(&mut buf);

        let mut live: Vec<(*mut u8, usize)> = Vec::new();
        let (chunk_base, chunk_len) = {
            let b = buf.as_mut_ptr() as usize;
            (b, buf.len())
        };
        for i in 0..200usize {
            let size = 16 + (i * 7) % 512;
            let mut p = unsafe { h.alloc(layout(size, 16)) };
            if p.is_null() {
                // The real allocator mmaps a fresh chunk when the free list
                // is empty (e.g. the chunk was fully freed and returned to
                // the kernel); the test buffer stands in for it.
                assert!(
                    h.chunk_find(chunk_base).is_none(),
                    "alloc failed with a chunk still in the table"
                );
                assert!(unsafe { h.insert_chunk(chunk_base, chunk_len) });
                p = unsafe { h.alloc(layout(size, 16)) };
            }
            assert!(!p.is_null(), "alloc {i} failed");
            // Write a recognizable pattern.
            for j in 0..size {
                unsafe { core::ptr::write_volatile(p.add(j), (i as u8).wrapping_add(j as u8)) };
            }
            live.push((p, size));

            // Free every third allocation.
            if i % 3 == 0 {
                let (old, _) = live.remove(0);
                unsafe { h.dealloc(old) };
            }
        }

        // Verify every surviving allocation's pattern.
        for &(p, size) in &live {
            let i = unsafe { core::ptr::read_volatile(p) };
            for j in 0..size {
                let v = unsafe { core::ptr::read_volatile(p.add(j)) };
                assert_eq!(
                    v,
                    i.wrapping_add(j as u8),
                    "data corrupted at 0x{:x}",
                    p.addr() + j
                );
            }
        }
    }

    #[test]
    fn allocator_is_send_sync() {
        fn check_send<T: Send>(_: &T) {}
        fn check_sync<T: Sync>(_: &T) {}
        #[cfg(all(target_os = "minix", feature = "rt"))]
        {
            let alloc = MmapAllocator;
            check_send(&alloc);
            check_sync(&alloc);
        }
        let h = Heap::new();
        check_send(&h);
        check_sync(&h);
    }
}
