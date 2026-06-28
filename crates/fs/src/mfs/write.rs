//! File write operations — adapted from `minix/fs/mfs/write.c`

use crate::mfs::cache::*;
use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::read::*;
use crate::mfs::types::*;

pub fn write_map(rip_idx: u16, position: i64, new_zone: u32, op: u32) -> i32 {
    unsafe {
        let rip = &mut *glo::get_inode_ptr(rip_idx as usize);
        (*rip).i_dirt = IN_DIRTY;
        let sp = match (*rip).i_sp {
            Some(ref mut s) => *s as *mut SuperBlock,
            None => return EIO,
        };
        let scale = (*sp).s_log_zone_size as u64;
        let zone = (position as u64 / (*sp).s_block_size as u64) >> scale;
        let zones = (*rip).i_ndzones as u64;

        if zone < zones {
            let zindex = zone as usize;
            if (*rip).i_zone[zindex] != NO_ZONE && (op & WMAP_FREE) != 0 {
                free_zone((*rip).i_dev, (*rip).i_zone[zindex]);
                (*rip).i_zone[zindex] = NO_ZONE;
            } else {
                (*rip).i_zone[zindex] = new_zone;
            }
            return OK;
        }
        todo!("write_map: indirect block not yet wired");
    }
}

pub fn clear_zone(rip_idx: u16, _pos: i64, _flag: i32) {
    unsafe {
        let rip = &*glo::get_inode_ptr(rip_idx as usize);
        debug_assert!((*rip).i_sp.as_ref().map_or(0, |sp| sp.s_log_zone_size) == 0);
    }
}

pub fn new_block(rip_idx: u16, position: i64) -> *mut u8 {
    unsafe {
        let rip = &mut *glo::get_inode_ptr(rip_idx as usize);
        if read_map(rip_idx, position, 0) == NO_BLOCK {
            let z = if (*rip).i_zsearch == NO_ZONE {
                (*rip).i_zone[0]
            } else {
                (*rip).i_zsearch
            };
            let z = if z == NO_ZONE {
                (*rip)
                    .i_sp
                    .as_ref()
                    .map_or(NO_ZONE, |sp| sp.s_firstdatazone)
            } else {
                z
            };
            let z = alloc_zone((*rip).i_dev, z);
            if z == NO_ZONE {
                return core::ptr::null_mut();
            }
            (*rip).i_zsearch = z;
            let r = write_map(rip_idx, position, z, 0);
            if r != OK {
                free_zone((*rip).i_dev, z);
                (*glo::mfs_ptr()).err_code = r;
                return core::ptr::null_mut();
            }
        }
        todo!("new_block: buffer cache not yet wired");
    }
}

pub fn zero_block(_bp: *mut u8) {
    todo!("zero_block: not yet wired")
}

pub fn truncate_inode(rip_idx: u16, newsize: i64) -> i32 {
    unsafe {
        let rip = &mut *glo::get_inode_ptr(rip_idx as usize);
        let ft = (*rip).i_mode & I_TYPE;
        if ft == I_CHAR_SPECIAL || ft == I_BLOCK_SPECIAL {
            return EINVAL;
        }
        let max_sz = (*rip).i_sp.as_ref().map_or(0, |sp| sp.s_max_size as i64);
        if newsize > max_sz {
            return EFBIG;
        }
        if newsize < (*rip).i_size as i64 {
            let r = freesp_inode(rip_idx, newsize, (*rip).i_size as i64);
            if r != OK {
                return r;
            }
        }
        if newsize > (*rip).i_size as i64 {
            clear_zone(rip_idx, (*rip).i_size as i64, 0);
        }
        (*rip).i_size = newsize as i32;
        (*rip).i_update |= CTIME | MTIME;
        (*rip).i_dirt = IN_DIRTY;
        OK
    }
}

pub fn fs_ftrunc() -> i32 {
    todo!("fs_ftrunc: not yet wired")
}

fn freesp_inode(rip_idx: u16, start: i64, mut end: i64) -> i32 {
    unsafe {
        let rip = &mut *glo::get_inode_ptr(rip_idx as usize);
        if end > (*rip).i_size as i64 {
            end = (*rip).i_size as i64;
        }
        if end <= start {
            return EINVAL;
        }
        let zs = (*rip)
            .i_sp
            .as_ref()
            .map_or(0, |sp| (sp.s_block_size as i64) << sp.s_log_zone_size) as u64;
        let e = end as u64 / zs;
        let mut p = ((start as u64 + zs - 1) / zs) * zs;
        while p < e * zs {
            let r = write_map(rip_idx, p as i64, NO_ZONE, WMAP_FREE);
            if r != OK {
                return r;
            }
            p += zs;
        }
        (*rip).i_update |= CTIME | MTIME;
        (*rip).i_dirt = IN_DIRTY;
        OK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe { crate::mfs::glo::mfs_init_globals(); }
    }

    #[test]
    fn test_clear_zone_no_super_does_not_panic() {
        // After init, inode_table[0].i_sp is None; the debug_assert
        // in clear_zone checks map_or(0, ...) == 0, which holds.
        init();
        clear_zone(0, 0, 0);
    }

    #[test]
    fn test_truncate_inode_chr_special_returns_einval() {
        // An inode with mode I_CHAR_SPECIAL causes truncate_inode to
        // return EINVAL before touching any allocator.
        init();
        unsafe {
            let rip = &mut *crate::mfs::glo::get_inode_ptr(0);
            rip.i_mode = I_CHAR_SPECIAL;
        }
        assert_eq!(truncate_inode(0, 0), EINVAL);
    }

    #[test]
    fn test_truncate_inode_blk_special_returns_einval() {
        init();
        unsafe {
            let rip = &mut *crate::mfs::glo::get_inode_ptr(0);
            rip.i_mode = I_BLOCK_SPECIAL;
        }
        assert_eq!(truncate_inode(0, 0), EINVAL);
    }

    #[test]
    fn test_write_map_returns_eio_when_no_super() {
        // After init, inode_table[0].i_sp is None, so write_map
        // returns EIO.
        init();
        assert_eq!(write_map(0, 0, 0, 0), EIO);
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_ftrunc_panics() {
        fs_ftrunc();
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_zero_block_panics() {
        zero_block(core::ptr::null_mut());
    }
}
