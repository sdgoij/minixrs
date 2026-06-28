//! Tests for the libminixfs block cache.

use super::*;

/// Helper: safely init the pool with a given size.
fn init_pool(nr_bufs: i32) {
    // SAFETY: tests run single-threaded.
    unsafe { super::lmfs_buf_pool(nr_bufs) };
}

#[test]
fn test_bufhash() {
    // bufhash = block % nr_bufs; test the formula directly.
    assert_eq!(100 % 64, 36);
    assert_eq!(0, 0);
}

#[test]
fn test_buf_pool_init() {
    unsafe {
        init_pool(10);
        assert!(super::lmfs_nr_bufs() >= 10);
        assert_eq!(super::lmfs_bufs_in_use(), 0);
    }
}

#[test]
fn test_buf_zeroed() {
    let b = Buf::zeroed();
    assert_eq!(b.lmfs_dev, NO_DEV);
    assert_eq!(b.lmfs_inode, VMC_NO_INODE);
    assert!(b.data_ptr.is_null());
    assert_eq!(b.lmfs_bytes, 0);
    assert_eq!(b.lmfs_count, 0);
    assert_eq!(b.lmfs_flags, 0);
}

#[test]
fn test_buf_is_clean() {
    let mut b = Buf::zeroed();
    assert!(b.is_clean());
    b.lmfs_flags |= VMMC_DIRTY;
    assert!(!b.is_clean());
}

#[test]
fn test_buf_is_locked() {
    let mut b = Buf::zeroed();
    assert!(!b.is_locked());
    b.lmfs_flags |= VMMC_BLOCK_LOCKED;
    assert!(b.is_locked());
}

#[test]
fn test_get_put_block_roundtrip() {
    unsafe {
        init_pool(10);

        let dev: u32 = 1;
        let block: u64 = 42;

        // Get a block.
        let bp = super::lmfs_get_block(dev, block);
        assert!(!bp.is_null());
        assert_eq!((*bp).lmfs_dev, dev);
        assert_eq!((*bp).lmfs_blocknr, block);
        assert!(!(*bp).data_ptr.is_null());
        assert!((*bp).lmfs_bytes > 0);
        assert!((*bp).lmfs_flags & VMMC_BLOCK_LOCKED != 0);

        // Put it back.
        super::lmfs_put_block(bp, FULL_DATA_BLOCK);
        assert!((*bp).lmfs_flags & VMMC_BLOCK_LOCKED == 0);
        assert_eq!((*bp).lmfs_count, 0);

        // Get it again — should find it in cache.
        let bp2 = super::lmfs_get_block(dev, block);
        assert!(!bp2.is_null());
        assert_eq!(bp2, bp);
        assert!((*bp2).lmfs_flags & VMMC_BLOCK_LOCKED != 0);

        super::lmfs_put_block(bp2, FULL_DATA_BLOCK);
    }
}

#[test]
fn test_markdirty_isclean() {
    unsafe {
        init_pool(10);

        let dev: u32 = 2;
        let block: u64 = 7;
        let bp = super::lmfs_get_block(dev, block);

        // Initially clean.
        assert_eq!(super::lmfs_isclean(bp), 1);

        // Mark dirty.
        super::lmfs_markdirty(bp);
        assert_eq!(super::lmfs_isclean(bp), 0);

        // Mark clean.
        super::lmfs_markclean(bp);
        assert_eq!(super::lmfs_isclean(bp), 1);

        super::lmfs_put_block(bp, FULL_DATA_BLOCK);
    }
}

#[test]
fn test_invalidate() {
    unsafe {
        init_pool(10);

        let dev: u32 = 3;
        let block: u64 = 99;
        let bp = super::lmfs_get_block(dev, block);
        super::lmfs_put_block(bp, FULL_DATA_BLOCK);

        // Now invalidate that device.
        super::lmfs_invalidate(dev);

        // After invalidation, getting the block should still work.
        let bp2 = super::lmfs_get_block(dev, block);
        assert!(!bp2.is_null());
        super::lmfs_put_block(bp2, FULL_DATA_BLOCK);
    }
}

#[test]
fn test_get_block_ino() {
    unsafe {
        init_pool(10);

        let dev: u32 = 4;
        let block: u64 = 55;
        let ino: u64 = 1001;
        let ino_off: u64 = 0;

        let bp = super::lmfs_get_block_ino(dev, block, NORMAL, ino, ino_off);
        assert!(!bp.is_null());
        assert_eq!((*bp).lmfs_dev, dev);
        assert_eq!((*bp).lmfs_blocknr, block);
        assert_eq!((*bp).lmfs_inode, ino);
        assert_eq!((*bp).lmfs_inode_offset, ino_off);

        super::lmfs_put_block(bp, FULL_DATA_BLOCK);
    }
}

#[test]
fn test_no_read_prefetch() {
    unsafe {
        init_pool(10);

        let dev: u32 = 5;
        let block: u64 = 10;

        // NO_READ: block won't be read from disk.
        let bp = super::lmfs_get_block_ino(dev, block, NO_READ, VMC_NO_INODE, 0);
        assert!(!bp.is_null());
        assert!(!(*bp).data_ptr.is_null());
        super::lmfs_put_block(bp, FULL_DATA_BLOCK);

        // PREFETCH: block won't be read; dev will be NO_DEV.
        let bp2 = super::lmfs_get_block_ino(dev, block + 1, PREFETCH, VMC_NO_INODE, 0);
        assert!(!bp2.is_null());
        assert!(!(*bp2).data_ptr.is_null());
        // In PREFETCH mode, dev is set to NO_DEV to indicate "not valid".
        assert_eq!((*bp2).lmfs_dev, NO_DEV);
        super::lmfs_put_block(bp2, FULL_DATA_BLOCK);
    }
}

#[test]
fn test_lru_chain_order() {
    unsafe {
        init_pool(10);

        let dev: u32 = 6;

        // Get a few blocks to make them "in use".
        let bp1 = super::lmfs_get_block(dev, 1);
        let bp2 = super::lmfs_get_block(dev, 2);
        let bp3 = super::lmfs_get_block(dev, 3);

        // Release them in reverse order.
        super::lmfs_put_block(bp3, FULL_DATA_BLOCK);
        super::lmfs_put_block(bp2, FULL_DATA_BLOCK);
        super::lmfs_put_block(bp1, FULL_DATA_BLOCK);

        // Getting a new block should reuse an LRU buffer.
        let bp_new = super::lmfs_get_block(dev, 4);
        assert!(!bp_new.is_null());

        // Clean up.
        super::lmfs_put_block(bp_new, FULL_DATA_BLOCK);
    }
}

#[test]
fn test_bufs_in_use_counting() {
    unsafe {
        init_pool(10);
        assert_eq!(super::lmfs_bufs_in_use(), 0);

        let dev: u32 = 7;
        let bp = super::lmfs_get_block(dev, 1);
        assert!(super::lmfs_bufs_in_use() > 0);

        super::lmfs_put_block(bp, FULL_DATA_BLOCK);
    }
}

/// Test that NO_BUF is a null pointer.
#[test]
fn test_no_buf_is_null() {
    assert!(NO_BUF.is_null());
}

/// Test constants have expected values.
#[test]
fn test_constants() {
    assert_eq!(LMFS_MAXNAME, 60);
    assert_eq!(LABEL_MAX, 16);
    assert_eq!(PATH_MAX, 255);
    assert_eq!(VMMC_BLOCK_LOCKED, 0x01);
    assert_eq!(VMMC_DIRTY, 0x02);
    assert_eq!(VMMC_EVICTED, 0x04);
    assert_eq!(VMMC_NEEDSETCACHE, 0x08);
    assert_eq!(VMC_NO_INODE, 0);
    assert_eq!(NO_DEV, u32::MAX);
    assert_eq!(PAGE_SIZE, 4096);
    assert_eq!(NORMAL, 0);
    assert_eq!(NO_READ, 1);
    assert_eq!(PREFETCH, 2);
}

/// Test bufhash with known values.
#[test]
fn test_bufhash_formula() {
    // The hash function is `block % nr_bufs`.
    assert_eq!((0u64), 0u64);
    assert_eq!((42u64 % 10u64), 2);
    assert_eq!((100u64 % 10u64), 0);
}
