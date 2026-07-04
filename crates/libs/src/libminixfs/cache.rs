//! Block cache — the central buffer cache used by all MFS-family filesystems.
//!
//! This module provides a pool of fixed-size buffers that cache disk blocks.
//! Callers acquire buffers with [`lmfs_get_block`] (or [`lmfs_get_block_ino`]),
//! use the data, and release with [`lmfs_put_block`].
//!
//! The implementation is ported from Minix 3.3.0's `libminixfs/cache.c`.

use alloc::alloc;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::libminixfs::constants::*;
use crate::libminixfs::errors::*;
use crate::libminixfs::types::*;

// Block I/O callback — set by the server to perform actual disk reads/writes

/// Block I/O function type.
///
/// `dev`: device number (major << MINOR | minor)
/// `block`: first block number to read/write
/// `nblocks`: number of consecutive blocks
/// `bufs`: pointer to array of `nblocks` buffer data pointers
/// `block_size`: size of each block in bytes
/// `rw_flag`: `READING` (0) or `WRITING` (1)
///
/// Returns the number of blocks successfully transferred, or a negative
/// error code on failure.
pub type BlockIoFn = unsafe fn(
    dev: u32,
    block: u64,
    nblocks: usize,
    bufs: *const *mut u8,
    block_size: usize,
    rw_flag: i32,
) -> i32;

/// Registered block I/O callback.  The MFS (or other FS) server sets this
/// at init time so the cache can perform actual disk I/O.
///
/// Stored as `AtomicUsize` because `BlockIoFn` is a function pointer
/// (pointer-sized). `0` = `None`, non-zero = the function address passed
/// through `transmute`.
static BLOCK_IO: AtomicUsize = AtomicUsize::new(0);

/// Register the block I/O callback.
///
/// # Safety
///
/// Must be called once during server init, before any block I/O is attempted.
pub unsafe fn lmfs_set_block_io(f: BlockIoFn) {
    BLOCK_IO.store(f as usize, Ordering::Release);
}

/// Read the registered block I/O callback, if any.
fn get_block_io() -> Option<BlockIoFn> {
    let val = BLOCK_IO.load(Ordering::Acquire);
    if val == 0 {
        None
    } else {
        // SAFETY: `val` was stored as `f as usize` where `f: BlockIoFn`,
        // so transmuting back is valid.
        Some(unsafe { core::mem::transmute::<usize, BlockIoFn>(val) })
    }
}

// Static (global) state

/// Hash table for fast buffer lookup by block number.
static mut BUF_HASH: *mut *mut Buf = ptr::null_mut();

/// Front of the LRU free list (least recently used).
static mut FRONT: *mut Buf = ptr::null_mut();

/// Rear of the LRU free list (most recently used).
static mut REAR: *mut Buf = ptr::null_mut();

/// The buffer array (allocated at pool init).
static mut BUF: *mut Buf = ptr::null_mut();

/// Total number of buffers.
static mut NR_BUFS: usize = 0;

/// Number of buffers currently in use (not on free list).
static mut BUFS_IN_USE: i32 = 0;

/// Current filesystem block size.
static mut FS_BLOCK_SIZE: u32 = PAGE_SIZE;

/// VM secondary cache enabled flag.
static mut VMCACHE: i32 = 0;

/// Whether the VM secondary cache is allowed for this FS.
static mut MAY_USE_VMCACHE: i32 = 0;

/// Last read/write error code.
static mut RDWT_ERR: i32 = OK;

/// Quiet mode flag (suppress diagnostic output).
static mut QUIET: i32 = 0;

// Helpers to safely read/write static mut via raw pointers

macro_rules! static_read {
    ($var:ident) => {{
        // SAFETY: single-threaded; no concurrent access
        #[allow(unused_unsafe)]
        unsafe {
            ptr::addr_of_mut!($var).read()
        }
    }};
}

macro_rules! static_write {
    ($var:ident, $val:expr) => {{
        // SAFETY: single-threaded; no concurrent access
        #[allow(unused_unsafe)]
        unsafe {
            ptr::addr_of_mut!($var).write($val)
        }
    }};
}

// Internal helpers

/// Hash function: maps a block number to a hash bucket index.
fn bufhash(block: u64) -> usize {
    let nr = static_read!(NR_BUFS);
    if nr == 0 { 0 } else { (block as usize) % nr }
}

const MINBUFS: usize = 6;

/// Remove a buffer from the LRU chain.
///
/// # Safety
///
/// `bp` must point to a valid `Buf` that is currently on an LRU chain.
unsafe fn rm_lru(bp: *mut Buf) {
    // SAFETY: caller guarantees bp is valid.
    let next_ptr = unsafe { (*bp).lmfs_next };
    let prev_ptr = unsafe { (*bp).lmfs_prev };

    if !prev_ptr.is_null() {
        unsafe { (*prev_ptr).lmfs_next = next_ptr };
    } else {
        static_write!(FRONT, next_ptr);
    }

    if !next_ptr.is_null() {
        unsafe { (*next_ptr).lmfs_prev = prev_ptr };
    } else {
        static_write!(REAR, prev_ptr);
    }
}

/// Increment reference count on a buffer.
///
/// # Safety
///
/// `bp` must point to a valid `Buf`.
unsafe fn raisecount(bp: *mut Buf) {
    debug_assert!(static_read!(BUFS_IN_USE) >= 0);
    debug_assert!(unsafe { (*bp).lmfs_count >= 0 });
    unsafe { (*bp).lmfs_count += 1 };
    if unsafe { (*bp).lmfs_count == 1 } {
        let in_use = static_read!(BUFS_IN_USE);
        static_write!(BUFS_IN_USE, in_use + 1);
    }
    debug_assert!(static_read!(BUFS_IN_USE) > 0);
}

/// Decrement reference count on a buffer.
///
/// # Safety
///
/// `bp` must point to a valid `Buf`.
unsafe fn lowercount(bp: *mut Buf) {
    debug_assert!(static_read!(BUFS_IN_USE) > 0);
    debug_assert!(unsafe { (*bp).lmfs_count > 0 });
    unsafe { (*bp).lmfs_count -= 1 };
    if unsafe { (*bp).lmfs_count == 0 } {
        let in_use = static_read!(BUFS_IN_USE);
        static_write!(BUFS_IN_USE, in_use - 1);
    }
    debug_assert!(static_read!(BUFS_IN_USE) >= 0);
}

/// Free a block's data memory and reset it to the initial (NO_DEV) state.
///
/// # Safety
///
/// `bp` must point to a valid `Buf` with `lmfs_count == 0`.
unsafe fn freeblock(bp: *mut Buf) {
    debug_assert!(unsafe { (*bp).lmfs_count == 0 });

    if unsafe { (*bp).lmfs_dev != NO_DEV } {
        if !unsafe { (*bp).is_clean() } {
            // SAFETY: flushall accesses global state but we hold BP.
            unsafe { flushall((*bp).lmfs_dev) };
        }
        debug_assert!(unsafe { (*bp).lmfs_bytes == static_read!(FS_BLOCK_SIZE) });
        unsafe { (*bp).lmfs_dev = NO_DEV };
    }

    // Mark clean (NO_DEV blocks may be marked dirty).
    unsafe { (*bp).lmfs_flags &= !VMMC_DIRTY };

    if unsafe { (*bp).lmfs_bytes > 0 } {
        debug_assert!(!unsafe { (*bp).data_ptr.is_null() });
        // TODO: actually free the mapped memory when allocator is available.
        unsafe { (*bp).lmfs_bytes = 0 };
        unsafe { (*bp).data_ptr = ptr::null_mut() };
    } else {
        debug_assert!(unsafe { (*bp).data_ptr.is_null() });
    }
}

/// Flush all dirty blocks for one device to disk.
///
/// # Safety
///
/// Must only be called when the global state is in a consistent state.
unsafe fn flushall(dev: u32) {
    let nr = static_read!(NR_BUFS);
    let buf_ptr = static_read!(BUF);

    if buf_ptr.is_null() || nr == 0 {
        return;
    }

    // Collect dirty buffers for this device.
    let mut dirty: [*mut Buf; 1024] = [ptr::null_mut(); 1024];
    let mut ndirty: usize = 0;

    for i in 0..nr {
        let bp = unsafe { buf_ptr.add(i) };
        if !unsafe { (*bp).is_clean() } && unsafe { (*bp).lmfs_dev == dev } && ndirty < dirty.len()
        {
            dirty[ndirty] = bp;
            ndirty += 1;
        }
    }

    if ndirty > 0 {
        // SAFETY: we own the global state.
        unsafe { lmfs_rw_scattered(dev, dirty.as_mut_ptr(), ndirty as i32, WRITING) };
    }
}

/// Read a block from disk into the buffer.
///
/// Calls the registered block I/O callback to perform the actual read.
/// Falls back to zero-filled data if no callback is registered.
///
/// # Safety
///
/// `bp` must point to a valid `Buf` with `data_ptr` already allocated.
unsafe fn read_block(bp: *mut Buf) {
    unsafe {
        let dev = (*bp).lmfs_dev;
        let block = (*bp).lmfs_blocknr;
        let block_size = (*bp).lmfs_bytes as usize;
        let data = (*bp).data_ptr;
        if data.is_null() || block_size == 0 {
            return;
        }
        if let Some(f) = get_block_io() {
            let bufs = &data as *const *mut u8;
            let n = f(dev, block, 1, bufs, block_size, READING);
            if n <= 0 {
                // I/O failed — leave buffer zero-filled.
            }
        }
        // No callback → leave buffer zero-filled (from allocation).
    }
}

/// Re-evaluate the cache size based on a heuristic.
fn cache_heuristic_check(_major: i32) {
    // Stub: VM stats (`fs_blockstats`, `vm_info_stats`) are not available yet.
}

/// Resize the buffer pool.
///
/// # Safety
///
/// Must not be called while any buffers are in use.
unsafe fn cache_resize(blocksize: u32, bufs: usize) {
    debug_assert!(blocksize > 0);
    debug_assert!(bufs >= MINBUFS);

    let nr = static_read!(NR_BUFS);
    let buf_ptr = static_read!(BUF);
    for i in 0..nr {
        let bp = unsafe { buf_ptr.add(i) };
        if unsafe { (*bp).lmfs_count != 0 } {
            // Cannot resize with buffers in use.
            return;
        }
    }

    // SAFETY: we checked no buffers are in use.
    unsafe { lmfs_buf_pool(bufs as i32) };
    static_write!(FS_BLOCK_SIZE, blocksize);
}

// Public API

/// Allocate a block of data memory for a buffer.
///
/// # Safety
///
/// `bp` must point to a valid `Buf` (typically freshly taken from the free
/// list) with no existing data allocation.
pub unsafe fn lmfs_alloc_block(bp: *mut Buf) {
    debug_assert!(unsafe { (*bp).data_ptr.is_null() });
    debug_assert!(unsafe { (*bp).lmfs_bytes == 0 });

    let block_size = static_read!(FS_BLOCK_SIZE);

    let layout = alloc::Layout::from_size_align(block_size as usize, PAGE_SIZE as usize)
        .expect("bad block size alignment");
    let ptr = unsafe { alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        // Free unused blocks and try again.
        let ptr2 = unsafe { alloc::alloc_zeroed(layout) };
        if ptr2.is_null() {
            panic!("libminixfs: could not allocate block");
        }
        unsafe { (*bp).data_ptr = ptr2 };
    } else {
        unsafe { (*bp).data_ptr = ptr };
    }
    debug_assert!(!unsafe { (*bp).data_ptr.is_null() });
    unsafe { (*bp).lmfs_bytes = block_size };
    unsafe { (*bp).lmfs_needsetcache = 1 };
}

/// Convenience wrapper: get a block with no inode tracking.
///
/// # Safety
///
/// This function accesses global mutable state and must be called in a
/// single-threaded context.
pub unsafe fn lmfs_get_block(dev: u32, block: u64) -> *mut Buf {
    // SAFETY: single-threaded usage expected.
    unsafe { lmfs_get_block_ino(dev, block, NORMAL, VMC_NO_INODE, 0) }
}

/// Look up a block in the cache (with inode/offset for VM secondary cache).
///
/// If the block is found in the cache, it is locked and returned.
/// If not found and `only_search` is `NORMAL`, a free buffer is taken,
/// the data is read from disk, and the buffer is returned.
///
/// # Safety
///
/// This function accesses global mutable state and must be called in a
/// single-threaded context.
pub unsafe fn lmfs_get_block_ino(
    dev: u32,
    block: u64,
    only_search: i32,
    ino: u64,
    ino_off: u64,
) -> *mut Buf {
    let buf_hash = static_read!(BUF_HASH);
    let buf = static_read!(BUF);
    let nr_bufs_val = static_read!(NR_BUFS);

    debug_assert!(!buf_hash.is_null());
    debug_assert!(!buf.is_null());
    debug_assert!(nr_bufs_val > 0);
    debug_assert!(static_read!(FS_BLOCK_SIZE) > 0);
    debug_assert!(dev != NO_DEV);

    // Search the hash chain for (dev, block).
    let b = bufhash(block);
    let mut bp = unsafe { ptr::read(buf_hash.add(b)) };

    while !bp.is_null() {
        if unsafe { (*bp).lmfs_blocknr == block && (*bp).lmfs_dev == dev } {
            if unsafe { (*bp).lmfs_flags & VMMC_EVICTED != 0 } {
                // We had it but VM evicted it; invalidate it.
                debug_assert!(unsafe { (*bp).lmfs_count == 0 });
                debug_assert!(unsafe { (*bp).lmfs_flags & VMMC_BLOCK_LOCKED == 0 });
                debug_assert!(unsafe { (*bp).lmfs_flags & VMMC_DIRTY == 0 });
                unsafe { (*bp).lmfs_dev = NO_DEV };
                unsafe { (*bp).lmfs_bytes = 0 };
                unsafe { (*bp).data_ptr = ptr::null_mut() };
                break;
            }
            // Block found.
            if unsafe { (*bp).lmfs_count == 0 } {
                // SAFETY: bp is on LRU list.
                unsafe { rm_lru(bp) };
                debug_assert!(unsafe { (*bp).lmfs_needsetcache == 0 });
                debug_assert!(unsafe { (*bp).lmfs_flags & VMMC_BLOCK_LOCKED == 0 });
                unsafe { (*bp).lmfs_flags |= VMMC_BLOCK_LOCKED };
            }
            // SAFETY: bp is valid.
            unsafe { raisecount(bp) };
            debug_assert!(unsafe { (*bp).lmfs_bytes == static_read!(FS_BLOCK_SIZE) });
            debug_assert!(unsafe { (*bp).lmfs_dev == dev });
            debug_assert!(unsafe { (*bp).lmfs_dev != NO_DEV });
            debug_assert!(unsafe { (*bp).lmfs_flags & VMMC_BLOCK_LOCKED != 0 });
            debug_assert!(!unsafe { (*bp).data_ptr.is_null() });

            if ino != VMC_NO_INODE
                && unsafe {
                    (*bp).lmfs_inode == VMC_NO_INODE
                        || (*bp).lmfs_inode != ino
                        || (*bp).lmfs_inode_offset != ino_off
                }
            {
                unsafe { (*bp).lmfs_inode = ino };
                unsafe { (*bp).lmfs_inode_offset = ino_off };
                unsafe { (*bp).lmfs_needsetcache = 1 };
            }

            return bp;
        } else {
            // Move to next block on hash chain.
            bp = unsafe { (*bp).lmfs_hash };
        }
    }

    // Desired block is not on available chain. Find a free block to use.
    if !bp.is_null() {
        debug_assert!(unsafe { (*bp).lmfs_flags & VMMC_EVICTED != 0 });
    } else {
        bp = static_read!(FRONT);
        if bp.is_null() {
            panic!("all buffers in use: {}", nr_bufs_val);
        }
    }
    debug_assert!(!bp.is_null());

    // SAFETY: bp is a valid buffer on the LRU list.
    unsafe { rm_lru(bp) };

    // Remove the block from its old hash chain.
    let b = bufhash(unsafe { (*bp).lmfs_blocknr });
    let mut prev_ptr = unsafe { ptr::read(buf_hash.add(b)) };
    if prev_ptr == bp {
        unsafe { ptr::write(buf_hash.add(b), (*bp).lmfs_hash) };
    } else {
        while !unsafe { (*prev_ptr).lmfs_hash.is_null() } {
            if unsafe { (*prev_ptr).lmfs_hash == bp } {
                unsafe { (*prev_ptr).lmfs_hash = (*bp).lmfs_hash };
                break;
            } else {
                prev_ptr = unsafe { (*prev_ptr).lmfs_hash };
            }
        }
    }

    // SAFETY: bp has count == 0.
    unsafe { freeblock(bp) };

    unsafe { (*bp).lmfs_inode = ino };
    unsafe { (*bp).lmfs_inode_offset = ino_off };
    unsafe { (*bp).lmfs_flags = VMMC_BLOCK_LOCKED };
    unsafe { (*bp).lmfs_needsetcache = 0 };
    unsafe { (*bp).lmfs_dev = dev };
    unsafe { (*bp).lmfs_blocknr = block };
    debug_assert!(unsafe { (*bp).lmfs_count == 0 });
    // SAFETY: bp is valid.
    unsafe { raisecount(bp) };

    let b = bufhash(unsafe { (*bp).lmfs_blocknr });
    unsafe { (*bp).lmfs_hash = ptr::read(buf_hash.add(b)) };
    unsafe { ptr::write(buf_hash.add(b), bp) };

    debug_assert!(dev != NO_DEV);
    debug_assert!(unsafe { (*bp).data_ptr.is_null() });
    debug_assert!(unsafe { (*bp).lmfs_bytes == 0 });

    // Try VM secondary cache first.
    if static_read!(VMCACHE) != 0 {
        // TODO: vm_map_cacheblock(dev, dev_off, ino, ino_off, &flags, fs_block_size)
    }

    // Allocate memory and read from disk.
    // SAFETY: bp is a valid buffer with no existing data.
    unsafe { lmfs_alloc_block(bp) };
    debug_assert!(!unsafe { (*bp).data_ptr.is_null() });

    match only_search {
        PREFETCH => {
            // Don't do I/O; mark dev as NO_DEV so callers know it's not valid.
            unsafe { (*bp).lmfs_dev = NO_DEV };
        }
        NORMAL => {
            // SAFETY: bp has allocated data.
            unsafe { read_block(bp) };
        }
        NO_READ => {
            // This block will be overwritten, so no I/O needed.
        }
        _ => {
            panic!("unexpected only_search value: {}", only_search);
        }
    }

    debug_assert!(!unsafe { (*bp).data_ptr.is_null() });
    bp
}

/// Release a block back to the cache.
///
/// Depending on `_block_type`, the block is placed on the front or rear of
/// the LRU chain.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_put_block(bp: *mut Buf, _block_type: i32) {
    if bp.is_null() {
        return;
    }

    // SAFETY: bp is valid.
    unsafe { lowercount(bp) };
    if unsafe { (*bp).lmfs_count != 0 } {
        return; // still in use
    }

    let dev = unsafe { (*bp).lmfs_dev };

    // Put this block back on the LRU chain.
    if dev == DEV_RAM || (_block_type & ONE_SHOT) != 0 {
        // Put on front (will be evicted soon).
        unsafe { (*bp).lmfs_prev = ptr::null_mut() };
        unsafe { (*bp).lmfs_next = static_read!(FRONT) };
        if unsafe { (*bp).lmfs_next.is_null() } {
            static_write!(REAR, bp);
        } else {
            unsafe { (*(*bp).lmfs_next).lmfs_prev = bp };
        }
        static_write!(FRONT, bp);
    } else {
        // Put on rear (will stay in cache longer).
        unsafe { (*bp).lmfs_prev = static_read!(REAR) };
        unsafe { (*bp).lmfs_next = ptr::null_mut() };
        if unsafe { (*bp).lmfs_prev.is_null() } {
            static_write!(FRONT, bp);
        } else {
            unsafe { (*(*bp).lmfs_prev).lmfs_next = bp };
        }
        static_write!(REAR, bp);
    }

    debug_assert!(unsafe { (*bp).lmfs_flags & VMMC_BLOCK_LOCKED != 0 });
    unsafe { (*bp).lmfs_flags &= !VMMC_BLOCK_LOCKED };

    // If VM cache is enabled, register this block with the VM.
    if static_read!(VMCACHE) != 0 && unsafe { (*bp).lmfs_needsetcache != 0 } && dev != NO_DEV {
        // TODO: vm_set_cacheblock(...)
    }
    unsafe { (*bp).lmfs_needsetcache = 0 };
}

/// Mark a buffer as dirty (modified).
///
/// # Safety
///
/// `bp` must point to a valid `Buf`.
pub unsafe fn lmfs_markdirty(bp: *mut Buf) {
    unsafe { (*bp).lmfs_flags |= VMMC_DIRTY };
}

/// Mark a buffer as clean (unmodified).
///
/// # Safety
///
/// `bp` must point to a valid `Buf`.
pub unsafe fn lmfs_markclean(bp: *mut Buf) {
    unsafe { (*bp).lmfs_flags &= !VMMC_DIRTY };
}

/// Check whether a buffer is clean.
///
/// # Safety
///
/// `bp` must point to a valid `Buf`.
pub unsafe fn lmfs_isclean(bp: *mut Buf) -> i32 {
    if unsafe { (*bp).lmfs_flags & VMMC_DIRTY == 0 } {
        1
    } else {
        0
    }
}

/// Flush all dirty blocks for all devices.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_flushall() {
    let nr = static_read!(NR_BUFS);
    let buf_ptr = static_read!(BUF);
    if buf_ptr.is_null() {
        return;
    }
    for i in 0..nr {
        let bp = unsafe { buf_ptr.add(i) };
        if unsafe { (*bp).lmfs_dev != NO_DEV && !(*bp).is_clean() } {
            // SAFETY: flushall accesses global state.
            unsafe { flushall((*bp).lmfs_dev) };
        }
    }
}

/// Invalidate (remove) all blocks belonging to `device` from the cache.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_invalidate(device: u32) {
    let nr = static_read!(NR_BUFS);
    let buf_ptr = static_read!(BUF);
    if buf_ptr.is_null() {
        return;
    }
    for i in 0..nr {
        let bp = unsafe { buf_ptr.add(i) };
        if unsafe { (*bp).lmfs_dev == device } {
            debug_assert!(!unsafe { (*bp).data_ptr.is_null() });
            debug_assert!(unsafe { (*bp).lmfs_bytes > 0 });
            // TODO: munmap_t(bp->data, bp->lmfs_bytes)
            unsafe { (*bp).lmfs_dev = NO_DEV };
            unsafe { (*bp).lmfs_bytes = 0 };
            unsafe { (*bp).data_ptr = ptr::null_mut() };
        }
    }
}

/// The writing flag for `lmfs_rw_scattered`.
pub const READING: i32 = 0;
pub const WRITING: i32 = 1;

/// Temporary device constant for RAM disk (DEV_RAM).
const DEV_RAM: u32 = 0;

/// Read or write scattered data from/to a device.
///
/// `buf_vec` is a pointer to an array of `num` buffer pointers, sorted by
/// block number. Adjacent blocks are merged into a single I/O vector.
///
/// # Safety
///
/// This function accesses global mutable state and dereferences raw pointers.
pub unsafe fn lmfs_rw_scattered(dev: u32, buf_vec: *mut *mut Buf, num: i32, rw_flag: i32) {
    if num == 0 {
        return;
    }

    let start_in_use = static_read!(BUFS_IN_USE);
    let start_bufqsize = num;

    // For READING, all buffers must be held (count > 0).
    if rw_flag == READING {
        for i in 0..num {
            let bp = unsafe { *buf_vec.add(i as usize) };
            debug_assert!(!bp.is_null());
            debug_assert!(unsafe { (*bp).lmfs_count > 0 });
        }
        debug_assert!(start_in_use >= start_bufqsize);
    }

    debug_assert!(dev != NO_DEV);

    // Shell-sort buffers on lmfs_blocknr.
    let mut gap = 1usize;
    let bufqsize = num as usize;
    loop {
        gap = 3 * gap + 1;
        if gap > bufqsize {
            break;
        }
    }
    while gap != 1 {
        gap /= 3;
        for j in gap..bufqsize {
            let mut i = j - gap;
            while unsafe { (**buf_vec.add(i)).lmfs_blocknr > (**buf_vec.add(i + gap)).lmfs_blocknr }
            {
                let tmp = unsafe { *buf_vec.add(i) };
                unsafe { *buf_vec.add(i) = *buf_vec.add(i + gap) };
                unsafe { *buf_vec.add(i + gap) = tmp };
                if i < gap {
                    break;
                }
                i -= gap;
            }
        }
    }

    let mut remaining = buf_vec;
    let mut remaining_count = num as isize;

    while remaining_count > 0 {
        let mut nblocks = 0;
        let first_block = unsafe { (**remaining.add(0)).lmfs_blocknr };

        // Count consecutive blocks.
        while nblocks < remaining_count {
            let bp = unsafe { *remaining.add(nblocks as usize) };
            if bp.is_null() {
                break;
            }
            if unsafe { (*bp).lmfs_blocknr != first_block + nblocks as u64 } {
                break;
            }
            nblocks += 1;
        }

        if nblocks == 0 {
            break;
        }

        debug_assert!(nblocks > 0);

        // Perform I/O via registered callback.
        let block_size = static_read!(FS_BLOCK_SIZE) as usize;
        let transferred = if let Some(f) = get_block_io() {
            // Build array of buffer data pointers for this consecutive range.
            // SAFETY: we own the buffers; they are valid for the duration of the call.
            let mut addrs = [core::ptr::null_mut::<u8>(); 64];
            let count = nblocks.min(64) as usize;
            for (j, slot) in addrs.iter_mut().enumerate().take(count) {
                let bp = unsafe { *remaining.add(j) };
                if !bp.is_null() {
                    *slot = unsafe { (*bp).data_ptr };
                }
            }
            unsafe { f(dev, first_block, count, addrs.as_ptr(), block_size, rw_flag) }
        } else {
            0 // No callback — zero blocks transferred.
        };
        let i = if transferred > 0 {
            transferred as usize
        } else {
            0
        };

        remaining = unsafe { remaining.add(i as usize) };
        remaining_count -= i as isize;

        if rw_flag == READING {
            // Release extra buffers.
            while remaining_count > 0 {
                // SAFETY: we're releasing buffers back to the cache.
                unsafe { lmfs_put_block(*remaining, PARTIAL_DATA_BLOCK) };
                remaining = unsafe { remaining.add(1) };
                remaining_count -= 1;
            }
        }

        if rw_flag == WRITING && i == 0 {
            // Not making progress; break to avoid infinite loop.
            break;
        }
    }
}

/// Set the block size for the cache.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_set_blocksize(new_block_size: u32, _major: i32) {
    let nr = static_read!(NR_BUFS);
    if nr == 0 {
        // Not initialized yet; just set the block size.
        static_write!(FS_BLOCK_SIZE, new_block_size);
        return;
    }

    // SAFETY: we own global state.
    unsafe { cache_resize(new_block_size, MINBUFS) };
    cache_heuristic_check(_major);

    // Decide whether to use VM secondary cache.
    static_write!(VMCACHE, 0);
    if static_read!(MAY_USE_VMCACHE) != 0 && new_block_size.is_multiple_of(PAGE_SIZE) {
        static_write!(VMCACHE, 1);
    }
}

/// Initialise (or re-initialise) the buffer pool.
///
/// Allocates a new array of `new_nr_bufs` buffers and a hash table of the
/// same size. If a pool already exists, it is torn down first.
///
/// # Safety
///
/// This function accesses global mutable state and allocates memory.
pub unsafe fn lmfs_buf_pool(new_nr_bufs: i32) {
    let new_nr = new_nr_bufs as usize;
    debug_assert!(new_nr >= MINBUFS);

    let old_nr = static_read!(NR_BUFS);
    let old_buf = static_read!(BUF);

    if old_nr > 0 {
        debug_assert!(!old_buf.is_null());
        // TODO: fs_sync()
        for i in 0..old_nr {
            let bp = unsafe { old_buf.add(i) };
            if !unsafe { (*bp).data_ptr.is_null() } {
                debug_assert!(unsafe { (*bp).lmfs_bytes > 0 });
                // TODO: munmap_t(bp->data, bp->lmfs_bytes)
            }
        }
    }

    if !old_buf.is_null() {
        // TODO: free(old_buf) — for now just leak
    }

    // Allocate new buffer array (zeroed).
    let buf_layout = alloc::Layout::array::<Buf>(new_nr).expect("bad Buf array layout");
    let new_buf = unsafe { alloc::alloc_zeroed(buf_layout) as *mut Buf };
    if new_buf.is_null() {
        panic!("couldn't allocate buf list ({})", new_nr);
    }

    // Allocate new hash table (zeroed).
    let hash_layout = alloc::Layout::array::<*mut Buf>(new_nr).expect("bad hash array layout");
    let new_hash = unsafe { alloc::alloc_zeroed(hash_layout) as *mut *mut Buf };
    if new_hash.is_null() {
        panic!("couldn't allocate buf hash list ({})", new_nr);
    }

    static_write!(BUF, new_buf);
    static_write!(BUF_HASH, new_hash);
    static_write!(NR_BUFS, new_nr);
    static_write!(BUFS_IN_USE, 0);

    // Set up the LRU chain.
    static_write!(FRONT, new_buf);
    static_write!(REAR, unsafe { new_buf.add(new_nr - 1) });

    for i in 0..new_nr {
        let bp = unsafe { new_buf.add(i) };
        unsafe { (*bp).lmfs_blocknr = NO_BLOCK };
        unsafe { (*bp).lmfs_dev = NO_DEV };
        unsafe { (*bp).lmfs_next = new_buf.add(i + 1) };
        unsafe {
            (*bp).lmfs_prev = if i == 0 {
                ptr::null_mut()
            } else {
                new_buf.add(i - 1)
            }
        };
        unsafe { (*bp).data_ptr = ptr::null_mut() };
        unsafe { (*bp).lmfs_bytes = 0 };
    }
    // Fix up first and last.
    unsafe { (*new_buf).lmfs_prev = ptr::null_mut() };
    unsafe { (*new_buf.add(new_nr - 1)).lmfs_next = ptr::null_mut() };

    // Set up hash chain (for now, chain all buffers together on bucket 0).
    for i in 0..new_nr {
        let bp = unsafe { new_buf.add(i) };
        if i + 1 < new_nr {
            unsafe { (*bp).lmfs_hash = new_buf.add(i + 1) };
        } else {
            unsafe { (*bp).lmfs_hash = ptr::null_mut() };
        }
    }
    unsafe { ptr::write(new_hash, new_buf) };
}

/// Return the number of buffers currently in use.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_bufs_in_use() -> i32 {
    static_read!(BUFS_IN_USE)
}

/// Return the total number of buffers.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_nr_bufs() -> i32 {
    static_read!(NR_BUFS) as i32
}

/// Track block count changes (delta) for cache re-evaluation.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_blockschange(_dev: u32, _delta: i32) {
    // Stub.
}

/// Get the device from a buffer.
///
/// # Safety
///
/// `bp` must point to a valid `Buf`.
pub unsafe fn lmfs_dev(bp: *const Buf) -> u32 {
    unsafe { (*bp).lmfs_dev }
}

/// Get the byte count from a buffer.
///
/// # Safety
///
/// `bp` must point to a valid `Buf`.
pub unsafe fn lmfs_bytes(bp: *const Buf) -> i32 {
    unsafe { (*bp).lmfs_bytes as i32 }
}

/// Get the last read/write error.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_rdwt_err() -> i32 {
    static_read!(RDWT_ERR)
}

/// Reset the last read/write error.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_reset_rdwt_err() {
    static_write!(RDWT_ERR, OK);
}

/// Enable or disable VM secondary cache for this FS.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_may_use_vmcache(yesno: i32) {
    static_write!(MAY_USE_VMCACHE, yesno);
}

/// Get the current filesystem block size.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_fs_block_size() -> u32 {
    static_read!(FS_BLOCK_SIZE)
}

/// "Block peek" — ensure a range of blocks is in the VM cache.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_do_bpeek(dev: u32, start: u64, len: u64) -> i32 {
    if static_read!(VMCACHE) == 0 {
        return ENXIO;
    }

    let block_size = static_read!(FS_BLOCK_SIZE);
    debug_assert!(block_size > 0);
    debug_assert!(dev != NO_DEV);

    let start_block = start / block_size as u64;
    let num_blocks = len.div_ceil(block_size as u64);

    for b in start_block..start_block + num_blocks {
        let bp = unsafe { lmfs_get_block(dev, b) };
        debug_assert!(!bp.is_null());
        // SAFETY: we just got the block.
        unsafe { lmfs_put_block(bp, FULL_DATA_BLOCK) };
    }

    OK
}

/// Re-evaluate cache size based on usage.
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn lmfs_cache_reevaluate(dev: u32) {
    if static_read!(BUFS_IN_USE) == 0 && dev != NO_DEV {
        // cache_heuristic_check(major(dev));
    }
}

/// Set quiet mode.
/// Set quiet mode.
///
/// # Safety
///
/// This function accesses global mutable state and must be called in a
/// single-threaded context.
pub unsafe fn lmfs_setquiet(q: i32) {
    static_write!(QUIET, q);
}

/// Determine buffer count from VM stats (heuristic).
///
/// # Safety
///
/// This function accesses global mutable state.
pub unsafe fn fs_bufs_heuristic() -> u32 {
    if static_read!(QUIET) == 0 {
        // printf("fslib: heuristic info fail: default to %d bufs\n", 1024);
    }
    1024
}

// Tests

#[cfg(test)]
#[path = "tests.rs"]
mod test_module;
