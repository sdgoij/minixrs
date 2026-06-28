//! Zone allocation / deallocation — adapted from `minix/fs/mfs/cache.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::super_block::*;
use crate::mfs::types::*;

pub fn alloc_zone(dev: u32, z: u32) -> u32 {
    let sp = match unsafe { get_super(dev).as_mut() } {
        Some(s) => s,
        None => return NO_ZONE,
    };
    let bit = if z == sp.s_firstdatazone {
        sp.s_zsearch
    } else {
        z - (sp.s_firstdatazone - 1)
    };
    let b = alloc_bit(sp, ZMAP, bit);
    if b == NO_BIT {
        unsafe {
            (*glo::mfs_ptr()).err_code = ENOSPC;
        }
        return NO_ZONE;
    }
    if z == sp.s_firstdatazone {
        sp.s_zsearch = b;
    }
    (sp.s_firstdatazone - 1) + b
}

pub fn free_zone(dev: u32, numb: u32) {
    let sp = match unsafe { get_super(dev).as_mut() } {
        Some(s) => s,
        None => return,
    };
    if numb < sp.s_firstdatazone || numb >= sp.s_zones {
        return;
    }
    let bit = numb - (sp.s_firstdatazone - 1);
    free_bit(sp, ZMAP, bit);
    if bit < sp.s_zsearch {
        sp.s_zsearch = bit;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_alloc_zone_no_dev_returns_no_zone() {
        // get_super(NO_DEV) returns null immediately, so alloc_zone
        // should return NO_ZONE without touching any other state.
        assert_eq!(alloc_zone(NO_DEV, 0), NO_ZONE);
    }

    #[test]
    fn test_free_zone_no_dev_does_not_panic() {
        // free_zone(NO_DEV, 0) should be a no-op when get_super
        // returns null.
        free_zone(NO_DEV, 0);
    }
}
