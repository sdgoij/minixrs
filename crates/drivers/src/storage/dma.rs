//! Storage DMA API — contiguous physical memory allocation for I/O.
//!
//! Provides `DmaBuffer`, an RAII wrapper around contiguous physical
//! memory pages suitable for PRD tables, virtqueue descriptor rings,
//! and other DMA-capable hardware structures.
//!
//! # Architecture
//!
//! The allocator backend is pluggable.  On x86_64 it is wired to the
//! kernel's `PhysicalAllocator` at boot.  On other platforms (or before
//! the allocator is registered) the stub returns `None` for all requests.
//!
//! Register an allocator at boot (example pattern):
//! ```ignore
//! dma::register_allocator(my_alloc_fn, my_free_fn);
//! ```

use core::ptr::addr_of_mut;

// ── Constants ───────────────────────────────────────────────────────────────

/// Default page size (4 KB).  Used for page-count calculations.
pub const DMA_PAGE_SIZE: usize = 4096;

/// Maximum number of pages a single `DmaBuffer` can hold.
pub const DMA_MAX_PAGES: usize = 64; // 256 KB

// ── Allocator backend ───────────────────────────────────────────────────────

/// Allocate `pages` contiguous physical pages.
///
/// Returns `(virtual_address, physical_address)` on success.
type AllocFn = fn(usize) -> Option<(*mut u8, u64)>;

/// Free `pages` pages starting at `virt`.
type FreeFn = fn(*mut u8, usize);

/// Registered allocator functions (or stubs).
static mut ALLOC_FN: AllocFn = stub_alloc;
static mut FREE_FN: FreeFn = stub_free;

fn stub_alloc(_pages: usize) -> Option<(*mut u8, u64)> {
    None
}

fn stub_free(_virt: *mut u8, _pages: usize) {}

/// Register a DMA allocator backend.
///
/// Must be called once during boot, before any DMA allocations.
///
/// # Safety
///
/// Caller must ensure exclusive access and that `alloc`/`free` are
/// safe to call with any page count up to `DMA_MAX_PAGES`.
pub unsafe fn register_allocator(alloc: AllocFn, free: FreeFn) {
    unsafe {
        addr_of_mut!(ALLOC_FN).write(alloc);
        addr_of_mut!(FREE_FN).write(free);
    }
}

// ── DmaBuffer ───────────────────────────────────────────────────────────────

/// A contiguous DMA buffer with automatic deallocation.
///
/// When dropped, the buffer is returned to the allocator.
pub struct DmaBuffer {
    virt: *mut u8,
    phys: u64,
    pages: usize,
}

impl DmaBuffer {
    /// Allocate a DMA buffer of at least `size` bytes.
    ///
    /// The actual allocation is rounded up to whole pages.
    /// Returns `None` if no allocator is registered or memory is
    /// exhausted.
    pub fn allocate(size: usize) -> Option<Self> {
        if size == 0 {
            return None;
        }
        let pages = size.div_ceil(DMA_PAGE_SIZE);
        if pages > DMA_MAX_PAGES {
            return None;
        }
        let (virt, phys) = unsafe { (addr_of_mut!(ALLOC_FN).read())(pages) }?;
        Some(Self { virt, phys, pages })
    }

    /// Virtual address of the buffer.
    pub fn virt(&self) -> *mut u8 {
        self.virt
    }

    /// Physical address of the buffer.
    pub fn phys(&self) -> u64 {
        self.phys
    }

    /// Number of allocated pages.
    pub fn page_count(&self) -> usize {
        self.pages
    }

    /// Total allocated size in bytes.
    pub fn size(&self) -> usize {
        self.pages * DMA_PAGE_SIZE
    }

    /// View the buffer as a byte slice (read-only).
    ///
    /// # Safety
    ///
    /// The buffer must contain valid data at the given offset and length.
    pub unsafe fn as_slice(&self, offset: usize, len: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt.add(offset), len) }
    }

    /// View the buffer as a mutable byte slice.
    ///
    /// # Safety
    ///
    /// The caller must ensure no aliasing violations.
    pub unsafe fn as_slice_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt.add(offset), len) }
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        unsafe {
            (addr_of_mut!(FREE_FN).read())(self.virt, self.pages);
        }
    }
}

// ── Convenience helpers ─────────────────────────────────────────────────────

/// Allocate a DMA buffer for the given number of pages.
pub fn alloc_dma_buf(pages: usize) -> Option<DmaBuffer> {
    DmaBuffer::allocate(pages * DMA_PAGE_SIZE)
}

/// Free a previously allocated DMA buffer.
///
/// Prefer letting `DmaBuffer`'s `Drop` handle this automatically.
pub fn free_dma_buf(buf: DmaBuffer) {
    drop(buf);
}

/// Physical address of an allocated DMA buffer.
pub fn dma_buf_phys(buf: &DmaBuffer) -> u64 {
    buf.phys()
}

/// Page count of an allocated DMA buffer.
pub fn dma_buf_page_count(buf: &DmaBuffer) -> usize {
    buf.page_count()
}

/// Size in bytes of an allocated DMA buffer.
pub fn dma_buf_size(buf: &DmaBuffer) -> usize {
    buf.size()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A test allocator that uses a static buffer.
    static mut TEST_POOL: [u8; DMA_PAGE_SIZE * 4] = [0u8; DMA_PAGE_SIZE * 4];
    static mut TEST_POOL_USED: bool = false;

    fn test_alloc(pages: usize) -> Option<(*mut u8, u64)> {
        if pages > 4 {
            return None;
        }
        unsafe {
            if TEST_POOL_USED {
                return None;
            }
            TEST_POOL_USED = true;
            Some((
                addr_of_mut!(TEST_POOL) as *mut u8,
                addr_of_mut!(TEST_POOL) as u64,
            ))
        }
    }

    fn test_free(_virt: *mut u8, _pages: usize) {
        unsafe {
            TEST_POOL_USED = false;
        }
    }

    /// Register the test allocator.
    unsafe fn use_test_alloc() {
        register_allocator(test_alloc, test_free);
    }

    /// Reset the allocator to the default stub.
    unsafe fn reset_allocator() {
        register_allocator(stub_alloc, stub_free);
    }

    #[test]
    fn test_dma_lifecycle() {
        unsafe {
            // Start clean.
            reset_allocator();

            // Stub before registration.
            assert!(alloc_dma_buf(1).is_none());
            assert!(DmaBuffer::allocate(0).is_none());

            // Register test allocator.
            use_test_alloc();

            // Allocate and verify.
            let buf = alloc_dma_buf(1).unwrap();
            assert_eq!(buf.page_count(), 1);
            assert_eq!(buf.size(), DMA_PAGE_SIZE);
            assert!(buf.phys() != 0);
            drop(buf);

            // Partial page rounds up.
            let buf = DmaBuffer::allocate(100).unwrap();
            assert_eq!(buf.page_count(), 1);
            drop(buf);

            // Multi-page.
            let buf = DmaBuffer::allocate(DMA_PAGE_SIZE * 3).unwrap();
            assert_eq!(buf.page_count(), 3);
            drop(buf);

            // Exceeds max.
            assert!(DmaBuffer::allocate(DMA_PAGE_SIZE * (DMA_MAX_PAGES + 1)).is_none());

            // Exceeds test pool (4 pages).
            assert!(alloc_dma_buf(5).is_none());

            // Write then read.
            let mut buf = DmaBuffer::allocate(64).unwrap();
            let slice = buf.as_slice_mut(0, 64);
            slice.copy_from_slice(&[0xABu8; 64]);
            let read = buf.as_slice(0, 64);
            assert_eq!(read, &[0xABu8; 64]);
            drop(buf);

            // Helper functions.
            let buf = alloc_dma_buf(2).unwrap();
            assert_eq!(dma_buf_page_count(&buf), 2);
            assert_eq!(dma_buf_size(&buf), DMA_PAGE_SIZE * 2);
            assert!(dma_buf_phys(&buf) != 0);
            drop(buf);
        }
    }

    #[test]
    fn test_dma_constants() {
        assert_eq!(DMA_PAGE_SIZE, 4096);
        assert_eq!(DMA_MAX_PAGES, 64);
    }
}
