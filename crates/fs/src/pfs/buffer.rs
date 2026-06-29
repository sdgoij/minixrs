//! Pipe data buffer pool management — adapted from `minix/fs/pfs/buffer.c`
//!
//! PFS manages a pool of `Buf` structures, each holding `PIPE_BUF` bytes
//! of pipe data. Buffers are identified by (device, inode-number) pairs
//! and stored in a doubly-linked list (via indices into the static pool).

use crate::pfs::consts::*;
use crate::pfs::glo;

/// Initialize the buffer pool free list.
///
/// All buffers start with `b_dev == NO_DEV` (free) and are linked
/// into the free list.
// Reference: buffer.c buf_pool()
pub fn init_buffer_pool() {
    unsafe {
        *glo::BUF_FRONT.get() = None;
        *glo::BUF_REAR.get() = None;

        // Link all buffers into the free list
        for i in 0..PIPE_NR_BUFS {
            let idx = i as u16;
            let bp = glo::get_buf_ptr(i);
            (*bp).b_dev = NO_DEV;
            (*bp).b_num = 0;
            (*bp).b_bytes = 0;
            (*bp).b_count = 0;
            (*bp).b_data = [0; PIPE_BUF];

            let front_ptr = glo::BUF_FRONT.get();
            let rear_ptr = glo::BUF_REAR.get();

            (*bp).b_next = None;
            (*bp).b_prev = *rear_ptr;
            if let Some(prev_idx) = *rear_ptr {
                (*glo::get_buf_ptr(prev_idx as usize)).b_next = Some(idx);
            }
            if (*front_ptr).is_none() {
                *front_ptr = Some(idx);
            }
            *rear_ptr = Some(idx);
        }
    }
}

/// Find or allocate a buffer for the given (device, inode-number) pair.
///
/// Scans the buffer list for an existing buffer matching `dev` and `inum`.
/// If found, increments its reference count and returns it.
/// If not found, allocates a new buffer from the free list.
///
/// Returns `None` if the pool is exhausted.
// Reference: buffer.c get_block()
pub fn get_block(dev: u32, inum: u32) -> Option<u16> {
    unsafe {
        let front_ptr = glo::BUF_FRONT.get();
        let mut bp_idx = *front_ptr;

        // Search the list for an existing buffer
        while let Some(idx) = bp_idx {
            let bp = glo::get_buf_ptr(idx as usize);
            if (*bp).b_dev == dev && (*bp).b_num == inum {
                (*bp).b_count += 1;
                return Some(idx);
            }
            bp_idx = (*bp).b_next;
        }

        // Not found — allocate a new one from the free list
        let mut bp_idx = *front_ptr;
        while let Some(idx) = bp_idx {
            let bp = glo::get_buf_ptr(idx as usize);
            if (*bp).b_dev == NO_DEV && (*bp).b_count == 0 {
                // Found a free buffer
                (*bp).b_num = inum;
                (*bp).b_dev = dev;
                (*bp).b_bytes = 0;
                (*bp).b_count = 1;
                (*bp).b_data = [0; PIPE_BUF];
                return Some(idx);
            }
            bp_idx = (*bp).b_next;
        }

        // No free buffer available
        None
    }
}

/// Release a buffer back to the pool.
///
/// Decrements the reference count. When the count reaches zero,
/// the buffer is removed from the active list and marked as free.
///
/// # Safety
///
/// `idx` must be a valid index into `buf_pool`.
// Reference: buffer.c put_block()
pub fn put_block(dev: u32, inum: u32) {
    unsafe {
        // Find the buffer
        let front_ptr = glo::BUF_FRONT.get();

        let mut bp_idx = *front_ptr;
        let mut found_idx: Option<u16> = None;

        while let Some(idx) = bp_idx {
            let bp = glo::get_buf_ptr(idx as usize);
            if (*bp).b_dev == dev && (*bp).b_num == inum {
                found_idx = Some(idx);
                break;
            }
            bp_idx = (*bp).b_next;
        }

        if found_idx.is_none() {
            return; // Buffer not found, nothing to put
        }

        let idx = found_idx.unwrap();
        let bp = glo::get_buf_ptr(idx as usize);

        (*bp).b_count -= 1;
        if (*bp).b_count > 0 {
            return; // Still in use
        }

        // Keep the buffer in the global list with (dev, inum) intact.
        // The original C code uses malloc/free, but we use a static pool.
        // Clearing b_dev would prevent get_block from finding the buffer
        // on a subsequent read/write.
        (*bp).b_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            init_buffer_pool();
        }
    }

    #[test]
    fn test_init_buffer_pool_creates_free_list() {
        init();
        unsafe {
            let front = glo::BUF_FRONT.get();
            assert!((*front).is_some());
            let rear = glo::BUF_REAR.get();
            assert!((*rear).is_some());
        }
    }

    #[test]
    fn test_get_block_returns_valid_buffer() {
        init();
        let bp = get_block(1, 2);
        assert!(bp.is_some());
    }

    #[test]
    fn test_get_block_reuses_existing() {
        init();
        let bp1 = get_block(1, 42).unwrap();
        let bp2 = get_block(1, 42).unwrap();
        assert_eq!(bp1, bp2);
        unsafe {
            assert_eq!((*glo::get_buf_ptr(bp1 as usize)).b_count, 2);
        }
    }

    #[test]
    fn test_put_block_decrements_count() {
        init();
        let bp = get_block(1, 99).unwrap();
        put_block(1, 99); // Decrement from 1 to 0
        unsafe {
            let bbuf = glo::get_buf_ptr(bp as usize);
            assert_eq!((*bbuf).b_count, 0);
            // Buffer keeps (dev, inum) association so get_block can find it
            assert_eq!((*bbuf).b_dev, 1);
        }
    }

    #[test]
    fn test_get_block_after_put() {
        init();
        let bp1 = get_block(1, 77).unwrap();
        put_block(1, 77);
        let bp2 = get_block(1, 77).unwrap();
        assert_eq!(bp1, bp2); // Same buffer should be reused
        unsafe {
            assert_eq!((*glo::get_buf_ptr(bp2 as usize)).b_count, 1);
            assert_eq!((*glo::get_buf_ptr(bp2 as usize)).b_dev, 1);
        }
    }
}
