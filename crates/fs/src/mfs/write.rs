//! File write operations — adapted from `minix/fs/mfs/write.c`

use crate::mfs::cache::*;
use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::read::*;
use crate::mfs::super_block::get_block_size;
use crate::mfs::types::*;

use libs::libminixfs::cache::{lmfs_get_block, lmfs_get_block_ino, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::NO_READ;

// WMAP flags
const WMAP_FREE: u32 = 0x01;

/// Write a zone number into an indirect block at the given index.
pub unsafe fn wr_indir(data: *mut u8, index: i32, zone: u32) {
    let zone_tab = data as *mut u32;
    core::ptr::write(zone_tab.add(index as usize), zone);
}

/// Check if an indirect block contains only NO_ZONE entries.
pub fn empty_indir(data: *mut u8, block_size: usize) -> bool {
    let n_entries = block_size / 4;
    let zone_tab = data as *const u32;
    for i in 0..(n_entries as isize) {
        if unsafe { core::ptr::read(zone_tab.offset(i)) } != NO_ZONE {
            return false;
        }
    }
    true
}

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
        let nr_indirects = (*rip).i_nindirs as u64;

        // Direct zones (indices 0 .. zones-1).
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

        // Indirect block handling.
        let mut excess = zone - zones;
        let mut new_ind = false;
        let mut new_dbl = false;
        let single: bool;
        let mut z1: u32;
        let mut bp_dindir: *mut libs::libminixfs::types::Buf = core::ptr::null_mut();

        if excess < nr_indirects {
            // Single indirect block.
            z1 = (*rip).i_zone[zones as usize];
            single = true;
        } else {
            // Double indirect block.
            let dbl_z = (*rip).i_zone[zones as usize + 1];
            if dbl_z == NO_ZONE && (op & WMAP_FREE) == 0 {
                let alloc_z = alloc_zone((*rip).i_dev, (*rip).i_zone[0]);
                if alloc_z == NO_ZONE {
                    return (*glo::mfs_ptr()).err_code;
                }
                (*rip).i_zone[zones as usize + 1] = alloc_z;
                new_dbl = true;
            }

            let dbl_z_cur = (*rip).i_zone[zones as usize + 1];
            excess -= nr_indirects;
            let ind_ex = (excess / nr_indirects) as usize;
            excess %= nr_indirects;
            if ind_ex >= nr_indirects as usize {
                return EFBIG;
            }

            if dbl_z_cur == NO_ZONE && (op & WMAP_FREE) != 0 {
                z1 = NO_ZONE;
            } else {
                let b = (dbl_z_cur as u64) << scale;
                bp_dindir = lmfs_get_block((*rip).i_dev, b);
                if bp_dindir.is_null() {
                    return EIO;
                }
                if new_dbl {
                    core::ptr::write_bytes(
                        (*bp_dindir).data_ptr,
                        0,
                        (*bp_dindir).lmfs_bytes as usize,
                    );
                }
                z1 = crate::mfs::read::rd_indir((*bp_dindir).data_ptr, ind_ex as i32);
            }
            single = false;
        }

        // z1 is the single indirect zone (or NO_ZONE). Create if needed.
        if z1 == NO_ZONE && (op & WMAP_FREE) == 0 {
            z1 = alloc_zone((*rip).i_dev, (*rip).i_zone[0]);
            if z1 == NO_ZONE {
                return (*glo::mfs_ptr()).err_code;
            }
            if single {
                (*rip).i_zone[zones as usize] = z1;
            } else if !bp_dindir.is_null() {
                wr_indir((*bp_dindir).data_ptr, (excess / nr_indirects) as i32, z1);
                lmfs_markdirty(bp_dindir);
            }
            new_ind = true;
        }

        // Read/write the single indirect block.
        if z1 != NO_ZONE {
            let ex = excess as usize;
            let b = (z1 as u64) << scale;
            let bp = lmfs_get_block((*rip).i_dev, b);
            if bp.is_null() {
                return EIO;
            }
            if new_ind {
                core::ptr::write_bytes((*bp).data_ptr, 0, (*bp).lmfs_bytes as usize);
            }

            if (op & WMAP_FREE) != 0 {
                let old_zone = crate::mfs::read::rd_indir((*bp).data_ptr, ex as i32);
                if old_zone != NO_ZONE {
                    free_zone((*rip).i_dev, old_zone);
                    wr_indir((*bp).data_ptr, ex as i32, NO_ZONE);
                }
                if empty_indir((*bp).data_ptr, (*sp).s_block_size as usize) {
                    free_zone((*rip).i_dev, z1);
                    z1 = NO_ZONE;
                    if single {
                        (*rip).i_zone[zones as usize] = NO_ZONE;
                    } else if !bp_dindir.is_null() {
                        wr_indir(
                            (*bp_dindir).data_ptr,
                            (excess / nr_indirects) as i32,
                            NO_ZONE,
                        );
                        lmfs_markdirty(bp_dindir);
                    }
                } else {
                    lmfs_markdirty(bp);
                }
            } else {
                wr_indir((*bp).data_ptr, ex as i32, new_zone);
                lmfs_markdirty(bp);
            }

            lmfs_put_block(bp, INDIRECT_BLOCK);
        }

        // If the single indirect was freed and we had a double indirect,
        // check whether the double indirect block is now empty.
        if z1 == NO_ZONE && !single && !bp_dindir.is_null() {
            let dbl_z = (*rip).i_zone[zones as usize + 1];
            if dbl_z != NO_ZONE && empty_indir((*bp_dindir).data_ptr, (*sp).s_block_size as usize) {
                free_zone((*rip).i_dev, dbl_z);
                (*rip).i_zone[zones as usize + 1] = NO_ZONE;
            }
        }

        if !bp_dindir.is_null() {
            lmfs_put_block(bp_dindir, INDIRECT_BLOCK);
        }

        OK
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

        // Calculate the block number from the zone mapping.
        let sp = match (*rip).i_sp {
            Some(ref s) => *s as *const SuperBlock,
            None => return core::ptr::null_mut(),
        };
        let scale = (*sp).s_log_zone_size as u64;
        let block_size = (*sp).s_block_size as u64;
        let z = read_map(rip_idx, position, 1) as u64;
        if z == NO_BLOCK as u64 {
            return core::ptr::null_mut();
        }
        let base_block = z << scale;
        let zone_size = block_size << scale;
        let b = base_block + ((position as u64 % zone_size) / block_size);

        // Get a clean buffer (NO_READ — block will be entirely overwritten).
        let bp = lmfs_get_block_ino(
            (*rip).i_dev,
            b,
            NO_READ,
            (*rip).i_num as u64,
            (position as u64 / block_size) * block_size,
        );
        if bp.is_null() {
            return core::ptr::null_mut();
        }

        // Zero the block and mark dirty.
        core::ptr::write_bytes((*bp).data_ptr, 0, (*bp).lmfs_bytes as usize);
        lmfs_markdirty(bp);

        bp as *mut u8
    }
}

pub unsafe fn zero_block(bp: *mut u8, dev: u32) {
    if bp.is_null() {
        return;
    }
    let block_size = get_block_size(dev);
    if block_size == 0 {
        return;
    }
    core::ptr::write_bytes(bp, 0, block_size as usize);
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
    unsafe {
        let mfs = glo::mfs_ptr();

        // Read message fields.
        // VFS req_ftrunc writes:
        //   inode_nr (u32) at PAYLOAD_OFF + 0  → m1i1
        //   start    (i64) at PAYLOAD_OFF + 8  → m1i3 (low) + m1i4 (high)
        //   end      (i64) at PAYLOAD_OFF + 16 → m1i5 (low) + m1i6 (high)
        let inode_nr = (*mfs).m_in.m_payload.m1.m1i1 as u32;

        let start_low = (*mfs).m_in.m_payload.m1.m1i3 as u64;
        let start_high = (*mfs).m_in.m_payload.m1.m1i4 as u64;
        let start = ((start_high << 32) | start_low) as i64;

        let end_low = (*mfs).m_in.m_payload.m1.m1i5 as u64;
        let end_high = (*mfs).m_in.m_payload.m1.m1i6 as u64;
        let end = ((end_high << 32) | end_low) as i64;

        let dev = (*mfs).fs_dev;

        let rip = match crate::mfs::inode::find_inode(dev, inode_nr) {
            Some(idx) => idx,
            None => return EINVAL,
        };

        // Check read-only.
        let sp = (*crate::mfs::glo::get_inode_ptr(rip as usize))
            .i_sp
            .as_ref()
            .map_or(core::ptr::null(), |s| &**s as *const SuperBlock);
        if sp.is_null() {
            return EIO;
        }
        if (*sp).s_rd_only != 0 {
            return EROFS;
        }

        if end == 0 {
            truncate_inode(rip, start)
        } else {
            freesp_inode(rip, start, end)
        }
    }
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
        unsafe {
            crate::mfs::glo::mfs_init_globals();
        }
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
    fn test_fs_ftrunc_returns_einval_when_no_inode() {
        // With no inode cache populated, find_inode returns None → EINVAL.
        unsafe {
            crate::mfs::glo::mfs_init_globals();
        }
        assert_eq!(fs_ftrunc(), EINVAL);
    }

    #[test]
    fn test_zero_block_null_does_nothing() {
        // Null pointer should return early without panic.
        unsafe {
            zero_block(core::ptr::null_mut(), 0);
        }
    }

    #[test]
    fn test_zero_block_zeros_some_memory() {
        let mut buf = [0xabu8; 1024];
        unsafe {
            zero_block(buf.as_mut_ptr(), 0);
        }
        // No super block for dev=0, so get_block_size returns 0,
        // and zero_block returns early. Verify the memory is unchanged.
        assert!(buf.iter().all(|&b| b == 0xab));
    }
}
