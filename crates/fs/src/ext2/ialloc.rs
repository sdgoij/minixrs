//! Inode allocation and deallocation — adapted from `minix/fs/ext2/ialloc.c`

use crate::ext2::balloc::*;
use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::inode::*;
use crate::ext2::super_::*;
use crate::ext2::types::BitchunkT;
use crate::ext2::types::*;
use crate::ext2::utility::*;
use libs::libminixfs::cache::{lmfs_get_block, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::FULL_DATA_BLOCK;
use libs::libminixfs::types::Buf;

/// Allocate a free inode on parent's device.
pub unsafe fn alloc_inode(parent: *mut Inode, bits: u16) -> *mut Inode {
    let sp = get_super((*parent).i_dev);
    if sp.is_null() || (*sp).s_rd_only != 0 {
        unsafe {
            let ext2 = glo::ext2_ptr();
            (*ext2).err_code = EROFS;
        }
        return core::ptr::null_mut();
    }
    let sp = &mut *sp;

    let is_dir = (bits & I_TYPE) == I_DIRECTORY;
    let b = alloc_inode_bit(sp, parent, is_dir);
    if b == NO_BIT {
        unsafe {
            let ext2 = glo::ext2_ptr();
            (*ext2).err_code = ENOSPC;
        }
        return core::ptr::null_mut();
    }

    let inumb = b;

    let rip = get_inode(NO_DEV, inumb);
    if rip.is_null() {
        free_inode_bit(sp, b, is_dir);
    } else {
        (*rip).i_mode = bits;
        (*rip).i_links_count = NO_LINK;
        unsafe {
            let ext2 = glo::ext2_ptr();
            (*rip).i_uid = (*ext2).caller_uid;
            (*rip).i_gid = (*ext2).caller_gid;
        }
        (*rip).i_dev = (*parent).i_dev;
        (*rip).i_sp = Some(sp as &mut SuperBlock);

        wipe_inode(rip);
    }

    rip
}

/// Free an inode.
pub unsafe fn free_inode(rip: *mut Inode) {
    let sp = get_super((*rip).i_dev);
    if sp.is_null() {
        return;
    }
    let sp = &mut *sp;
    let b = (*rip).i_num;
    let mode = (*rip).i_mode;

    if b <= NO_ENTRY || b > sp.s_inodes_count {
        return;
    }
    free_inode_bit(sp, b, (mode & I_TYPE) == I_DIRECTORY);
    (*rip).i_mode = I_NOT_ALLOC;
}

/// Allocate an inode bitmap bit.
fn alloc_inode_bit(sp: &mut SuperBlock, parent: *mut Inode, is_dir: bool) -> u32 {
    if sp.s_rd_only != 0 {
        return NO_BIT;
    }

    let opt = glo::OPT.get();
    let group: i32;
    unsafe {
        if (*opt).mfsalloc != 0 {
            group = find_group_any(sp);
        } else {
            if is_dir {
                if (*opt).use_orlov != 0 {
                    group = find_group_orlov(sp, parent);
                } else {
                    group = find_group_dir(sp);
                }
            } else {
                group = find_group_hashalloc(sp, parent);
            }
        }
    }

    if group < 0 {
        return NO_BIT;
    }

    let gd = get_group_desc(sp, group as u32);
    if gd.is_null() {
        return NO_BIT;
    }

    unsafe {
        if (*gd).free_inodes_count == 0 {
            return NO_BIT;
        }

        let bp = lmfs_get_block(sp.s_dev, (*gd).inode_bitmap as u64);
        if bp.is_null() {
            return NO_BIT;
        }

        let block_size = sp.s_block_size as usize;
        let bitmap_ptr = (*bp).data_ptr as *mut BitchunkT;
        let bitmap_len = block_size / core::mem::size_of::<BitchunkT>();
        let bitmap = core::slice::from_raw_parts_mut(bitmap_ptr, bitmap_len);

        let bit = setbit(bitmap, sp.s_inodes_per_group, 0);
        if bit == -1 {
            lmfs_put_block(bp, FULL_DATA_BLOCK);
            return NO_BIT;
        }

        let inumber = (group as u32) * sp.s_inodes_per_group + (bit as u32) + 1;

        // Extra check: inumber should be valid
        if inumber > sp.s_inodes_count {
            lmfs_put_block(bp, FULL_DATA_BLOCK);
            return NO_BIT;
        }

        lmfs_markdirty(bp);
        lmfs_put_block(bp, FULL_DATA_BLOCK);

        (*gd).free_inodes_count -= 1;
        sp.s_free_inodes_count -= 1;

        if is_dir {
            (*gd).used_dirs_count += 1;
            sp.s_dirs_counter += 1;
        }

        return inumber;
    }
}

/// Free an inode bitmap bit.
fn free_inode_bit(sp: &mut SuperBlock, bit_returned: u32, is_dir: bool) {
    if sp.s_rd_only != 0 {
        return;
    }

    if bit_returned > sp.s_inodes_count || bit_returned < ext2_first_ino(sp) {
        return;
    }

    let group = (bit_returned - 1) / sp.s_inodes_per_group;
    let bit = (bit_returned - 1) % sp.s_inodes_per_group;

    let gd = get_group_desc(sp, group);
    if gd.is_null() {
        return;
    }

    let bp = unsafe { lmfs_get_block(sp.s_dev, (*gd).inode_bitmap as u64) };
    if bp.is_null() {
        return;
    }

    unsafe {
        let block_size = sp.s_block_size as usize;
        let bitmap_ptr = (*bp).data_ptr as *mut BitchunkT;
        let bitmap_len = block_size / core::mem::size_of::<BitchunkT>();
        let bitmap = core::slice::from_raw_parts_mut(bitmap_ptr, bitmap_len);

        if unsetbit(bitmap, bit) != 0 {
            lmfs_put_block(bp, FULL_DATA_BLOCK);
            return;
        }

        lmfs_markdirty(bp);
        lmfs_put_block(bp, FULL_DATA_BLOCK);

        (*gd).free_inodes_count += 1;
        sp.s_free_inodes_count += 1;

        if is_dir {
            (*gd).used_dirs_count -= 1;
            sp.s_dirs_counter -= 1;
        }
    }

    if group < sp.s_igsearch as u32 {
        sp.s_igsearch = group as i32;
    }
}


fn find_group_any(sp: &SuperBlock) -> i32 {
    let ngroups = sp.s_groups_count;
    let mut group = sp.s_igsearch;

    while (group as u32) < ngroups {
        let gd = get_group_desc(sp, group as u32);
        if gd.is_null() {
            group += 1;
            continue;
        }
        unsafe {
            if (*gd).free_inodes_count > 0 {
                let sp_mut = sp as *const SuperBlock as *mut SuperBlock;
                (*sp_mut).s_igsearch = group;
                return group;
            }
        }
        group += 1;
    }
    -1
}

fn find_group_dir(sp: &SuperBlock) -> i32 {
    let avefreei = sp.s_free_inodes_count / sp.s_groups_count;
    let mut best_group = -1i32;
    let mut best_free_blocks = 0u16;

    for group in 0..sp.s_groups_count {
        let gd = get_group_desc(sp, group);
        if gd.is_null() {
            continue;
        }
        unsafe {
            if (*gd).free_inodes_count == 0 {
                continue;
            }
            if (*gd).free_inodes_count < avefreei as u16 {
                continue;
            }
            if best_group < 0 || (*gd).free_blocks_count > best_free_blocks {
                best_group = group as i32;
                best_free_blocks = (*gd).free_blocks_count;
            }
        }
    }
    best_group
}

fn find_group_hashalloc(sp: &SuperBlock, parent: *mut Inode) -> i32 {
    let ngroups = sp.s_groups_count;
    let parent_group;
    unsafe {
        parent_group = ((*parent).i_num - 1) / sp.s_inodes_per_group;
    }

    // Try parent group
    let gd = get_group_desc(sp, parent_group);
    if !gd.is_null() {
        unsafe {
            if (*gd).free_inodes_count > 0 && (*gd).free_blocks_count > 0 {
                return parent_group as i32;
            }
        }
    }

    // Quadratic probing
    let mut group = (parent_group + 1) % ngroups;
    let mut i = 1u32;
    while i < ngroups {
        let gd = get_group_desc(sp, group);
        if !gd.is_null() {
            unsafe {
                if (*gd).free_inodes_count > 0 && (*gd).free_blocks_count > 0 {
                    return group as i32;
                }
            }
        }
        group = (group + i) % ngroups;
        i <<= 1;
    }

    // Linear search
    group = parent_group;
    for _ in 0..ngroups {
        if group >= ngroups {
            group = 0;
        }
        let gd = get_group_desc(sp, group);
        if !gd.is_null() {
            unsafe {
                if (*gd).free_inodes_count > 0 {
                    return group as i32;
                }
            }
        }
        group += 1;
    }
    -1
}

fn find_group_orlov(sp: &SuperBlock, parent: *mut Inode) -> i32 {
    let avefreei = sp.s_free_inodes_count / sp.s_groups_count;
    let avefreeb = sp.s_free_blocks_count / sp.s_groups_count;

    unsafe {
        let is_root_or_topdir =
            (*parent).i_num == ROOT_INODE || ((*parent).i_flags & EXT2_TOPDIR_FL) != 0;

        if is_root_or_topdir {
            let mut best_group = -1i32;
            let mut best_avefree_group = -1i32;
            let mut best_ndir = sp.s_inodes_per_group;
            let mut fallback_group = -1i32;

            let mut group = 0i32;
            for _ in 0..sp.s_groups_count {
                if group as u32 >= sp.s_groups_count {
                    group = 0;
                }
                let gd = get_group_desc(sp, group as u32);
                if gd.is_null() {
                    group += 1;
                    continue;
                }
                if (*gd).free_inodes_count == 0 {
                    group += 1;
                    continue;
                }
                fallback_group = group;

                if (*gd).free_inodes_count >= avefreei as u16
                    && (*gd).free_blocks_count >= avefreeb as u16
                {
                    best_avefree_group = group;
                    if (*gd).used_dirs_count < best_ndir as u16 {
                        best_ndir = (*gd).used_dirs_count as u32;
                        best_group = group;
                    }
                }
                group += 1;
            }
            if best_group >= 0 {
                return best_group;
            }
            if best_avefree_group >= 0 {
                return best_avefree_group;
            }
            return fallback_group;
        } else {
            let parent_group = ((*parent).i_num - 1) / sp.s_inodes_per_group;
            let min_blocks = avefreeb / 2;
            let min_inodes = avefreei / 2;
            let mut fallback_group = -1i32;

            let mut group = parent_group as i32;
            for _ in 0..sp.s_groups_count {
                if group as u32 >= sp.s_groups_count {
                    group = 0;
                }
                let gd = get_group_desc(sp, group as u32);
                if gd.is_null() {
                    group += 1;
                    continue;
                }
                if (*gd).free_inodes_count == 0 {
                    group += 1;
                    continue;
                }
                fallback_group = group;

                if (*gd).free_inodes_count >= min_inodes as u16
                    && (*gd).free_blocks_count >= min_blocks as u16
                {
                    return group;
                }
                group += 1;
            }
            return fallback_group;
        }
    }
}

unsafe fn wipe_inode(rip: *mut Inode) {
    unsafe {
        (*rip).i_size = 0;
        (*rip).i_update = ATIME | CTIME | MTIME;
        (*rip).i_blocks = 0;
        (*rip).i_flags = 0;
        (*rip).i_generation = 0;
        (*rip).i_file_acl = 0;
        (*rip).i_dir_acl = 0;
        (*rip).i_faddr = 0;
        for i in 0..EXT2_N_BLOCKS {
            (*rip).i_block[i] = NO_BLOCK;
        }
        (*rip).i_block[0] = NO_BLOCK;
        (*rip).i_dirt = IN_DIRTY;
    }
}
