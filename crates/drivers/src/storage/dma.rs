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

use core::sync::atomic::{AtomicUsize, Ordering};

/// Default page size (4 KB).  Used for page-count calculations.
pub const DMA_PAGE_SIZE: usize = 4096;

/// Maximum number of pages a single `DmaBuffer` can hold.
pub const DMA_MAX_PAGES: usize = 64; // 256 KB

/// Allocate `pages` contiguous physical pages.
type AllocFn = fn(usize) -> Option<(*mut u8, u64)>;

/// Free `pages` pages starting at `virt`.
type FreeFn = fn(*mut u8, usize);

fn stub_alloc(_pages: usize) -> Option<(*mut u8, u64)> {
    None
}
fn stub_free(_virt: *mut u8, _pages: usize) {}

fn allocfn_to_usize(f: AllocFn) -> usize {
    f as usize
}

fn freefn_to_usize(f: FreeFn) -> usize {
    f as usize
}

/// Convert a usize back to an alloc function pointer.
///
/// # Safety
///
/// The usize value must have been obtained from a previous call to
/// `allocfn_to_usize` and must not have been corrupted.
fn usize_to_allocfn(u: usize) -> AllocFn {
    unsafe { core::mem::transmute::<usize, AllocFn>(u) }
}

/// Convert a usize back to a free function pointer.
///
/// # Safety
///
/// The usize value must have been obtained from a previous call to
/// `freefn_to_usize` and must not have been corrupted.
fn usize_to_freefn(u: usize) -> FreeFn {
    unsafe { core::mem::transmute::<usize, FreeFn>(u) }
}

/// Registered allocator functions (or stubs), stored as bits in atomics.
static ALLOC_FN: AtomicUsize = AtomicUsize::new(0);
static FREE_FN: AtomicUsize = AtomicUsize::new(0);

/// Initialize the DMA allocator with default stub functions.
/// Must be called once before any usage.
pub fn init() {
    ALLOC_FN.store(allocfn_to_usize(stub_alloc), Ordering::Relaxed);
    FREE_FN.store(freefn_to_usize(stub_free), Ordering::Relaxed);
}

/// Register a DMA allocator backend.
/// Register a DMA allocator backend.
///
/// Must be called once during boot, before any DMA allocations.
///
/// # Safety
///
/// Caller must ensure exclusive access and that `alloc`/`free` are
/// safe to call with any page count up to `DMA_MAX_PAGES`.
pub unsafe fn register_allocator(alloc: AllocFn, free: FreeFn) {
    ALLOC_FN.store(allocfn_to_usize(alloc), Ordering::Relaxed);
    FREE_FN.store(freefn_to_usize(free), Ordering::Relaxed);
}

/// A contiguous DMA buffer with automatic deallocation.
pub struct DmaBuffer {
    virt: *mut u8,
    phys: u64,
    pages: usize,
}

impl DmaBuffer {
    pub fn allocate(size: usize) -> Option<Self> {
        if size == 0 {
            return None;
        }
        let pages = size.div_ceil(DMA_PAGE_SIZE);
        if pages > DMA_MAX_PAGES {
            return None;
        }
        let alloc = usize_to_allocfn(ALLOC_FN.load(Ordering::Relaxed));
        let (virt, phys) = alloc(pages)?;
        Some(Self { virt, phys, pages })
    }

    pub fn virt(&self) -> *mut u8 {
        self.virt
    }
    pub fn phys(&self) -> u64 {
        self.phys
    }
    pub fn page_count(&self) -> usize {
        self.pages
    }
    pub fn size(&self) -> usize {
        self.pages * DMA_PAGE_SIZE
    }

    /// View the buffer as a byte slice (read-only).
    ///
    /// # Safety
    ///
    /// The buffer must contain valid data at the given offset and length.
    /// The caller must ensure no aliasing violations.
    pub unsafe fn as_slice(&self, offset: usize, len: usize) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.virt.add(offset), len) }
    }

    /// View the buffer as a mutable byte slice.
    ///
    /// # Safety
    ///
    /// The caller must ensure no aliasing violations.
    /// The returned reference must not outlive the buffer.
    pub unsafe fn as_slice_mut(&mut self, offset: usize, len: usize) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.virt.add(offset), len) }
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        let free = usize_to_freefn(FREE_FN.load(Ordering::Relaxed));
        free(self.virt, self.pages);
    }
}

pub fn alloc_dma_buf(pages: usize) -> Option<DmaBuffer> {
    DmaBuffer::allocate(pages * DMA_PAGE_SIZE)
}

pub fn free_dma_buf(buf: DmaBuffer) {
    drop(buf);
}
pub fn dma_buf_phys(buf: &DmaBuffer) -> u64 {
    buf.phys()
}
pub fn dma_buf_page_count(buf: &DmaBuffer) -> usize {
    buf.page_count()
}
pub fn dma_buf_size(buf: &DmaBuffer) -> usize {
    buf.size()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::UnsafeCell;

    struct TestPool(UnsafeCell<[u8; DMA_PAGE_SIZE * 4]>);
    unsafe impl Sync for TestPool {}
    impl TestPool {
        const fn new() -> Self {
            Self(UnsafeCell::new([0u8; DMA_PAGE_SIZE * 4]))
        }
        fn get(&self) -> *mut [u8; DMA_PAGE_SIZE * 4] {
            self.0.get()
        }
    }

    static TEST_POOL: TestPool = TestPool::new();
    static TEST_POOL_USED: core::sync::atomic::AtomicBool =
        core::sync::atomic::AtomicBool::new(false);

    fn test_alloc(pages: usize) -> Option<(*mut u8, u64)> {
        if pages > 4 {
            return None;
        }
        if TEST_POOL_USED.load(Ordering::Relaxed) {
            return None;
        }
        TEST_POOL_USED.store(true, Ordering::Relaxed);
        Some((TEST_POOL.get().cast::<u8>(), TEST_POOL.get() as u64))
    }

    fn test_free(_virt: *mut u8, _pages: usize) {
        TEST_POOL_USED.store(false, Ordering::Relaxed);
    }

    unsafe fn use_test_alloc() {
        unsafe { register_allocator(test_alloc, test_free) };
    }
    unsafe fn reset_allocator() {
        unsafe { register_allocator(stub_alloc, stub_free) };
    }

    #[test]
    fn test_dma_lifecycle() {
        unsafe {
            init();
            reset_allocator();
            assert!(alloc_dma_buf(1).is_none());
            assert!(DmaBuffer::allocate(0).is_none());
            use_test_alloc();
            let buf = alloc_dma_buf(1).unwrap();
            assert_eq!(buf.page_count(), 1);
            assert_eq!(buf.size(), DMA_PAGE_SIZE);
            assert!(buf.phys() != 0);
            drop(buf);
            let buf = DmaBuffer::allocate(100).unwrap();
            assert_eq!(buf.page_count(), 1);
            drop(buf);
            let buf = DmaBuffer::allocate(DMA_PAGE_SIZE * 3).unwrap();
            assert_eq!(buf.page_count(), 3);
            drop(buf);
            assert!(DmaBuffer::allocate(DMA_PAGE_SIZE * (DMA_MAX_PAGES + 1)).is_none());
            assert!(alloc_dma_buf(5).is_none());
            let mut buf = DmaBuffer::allocate(64).unwrap();
            let slice = buf.as_slice_mut(0, 64);
            slice.copy_from_slice(&[0xABu8; 64]);
            let read = buf.as_slice(0, 64);
            assert_eq!(read, &[0xABu8; 64]);
            drop(buf);
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
