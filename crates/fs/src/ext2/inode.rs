//! Inode cache and I/O — adapted from `minix/fs/ext2/inode.c`

use crate::ext2::balloc::*;
use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::glo::Ext2Global;
use crate::ext2::super_::*;
use crate::ext2::types::*;
use crate::ext2::utility::*;
use libs::libminixfs::cache::{lmfs_get_block, lmfs_markdirty, lmfs_put_block};
use libs::libminixfs::constants::INODE_BLOCK;
use libs::libminixfs::types::Buf;

/// Initialize inode cache.
pub unsafe fn init_inode_cache() {
    let ext2 = glo::ext2_ptr();
    (*ext2).inode_cache_hit = 0;
    (*ext2).inode_cache_miss = 0;

    // Initialize hash lists (None = empty list)
    for i in 0..INODE_HASH_SIZE {
        core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[i]).write(None);
    }

    // Add inodes to unused list
    for i in 0..NR_INODES {
        let rip = glo::get_inode_ptr(i);
        (*rip).i_num = NO_ENTRY;
    }
    *glo::UNUSED_INODES_HEAD.get() = Some(0);
    // Link unused list sequentially
    for i in 0..(NR_INODES - 1) {
        let rip = glo::get_inode_ptr(i);
        (*rip).i_unused_next = Some(i as u16 + 1);
        let next = glo::get_inode_ptr(i + 1);
        (*next).i_unused_prev = Some(i as u16);
    }
    let last = glo::get_inode_ptr(NR_INODES - 1);
    (*last).i_unused_next = None;
}

pub unsafe fn addhash_inode(node: *mut Inode) {
    let hashi = ((*node).i_num as usize) & INODE_HASH_MASK;
    let head = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
    let idx = (node as *const Inode as usize - glo::get_inode_ptr(0) as *const Inode as usize)
        / core::mem::size_of::<Inode>();
    (*node).i_hash_next = *head;
    (*node).i_hash_prev = None;
    if let Some(next) = *head {
        let next_ptr = glo::get_inode_ptr(next as usize);
        (*next_ptr).i_hash_prev = Some(idx as u16);
    }
    *head = Some(idx as u16);
}

pub unsafe fn unhash_inode(node: *mut Inode) {
    let hashi = ((*node).i_num as usize) & INODE_HASH_MASK;
    let head = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
    let node_idx = (node as *const Inode as usize - glo::get_inode_ptr(0) as *const Inode as usize)
        / core::mem::size_of::<Inode>();

    if let Some(prev) = (*node).i_hash_prev {
        let prev_ptr = glo::get_inode_ptr(prev as usize);
        (*prev_ptr).i_hash_next = (*node).i_hash_next;
    } else {
        // Node is head of list
        *head = (*node).i_hash_next;
    }
    if let Some(next) = (*node).i_hash_next {
        let next_ptr = glo::get_inode_ptr(next as usize);
        (*next_ptr).i_hash_prev = (*node).i_hash_prev;
    }
    (*node).i_hash_next = None;
    (*node).i_hash_prev = None;
    let _ = node_idx;
}

/// Find inode in hash table by device and number.
pub unsafe fn find_inode(dev: u32, numb: u32) -> *mut Inode {
    let hashi = (numb as usize) & INODE_HASH_MASK;
    let mut idx = (*glo::HASH_INODES.get())[hashi];
    while let Some(i) = idx {
        let rip = glo::get_inode_ptr(i as usize);
        if (*rip).i_count > 0 && (*rip).i_num == numb && (*rip).i_dev == dev {
            return rip;
        }
        idx = (*rip).i_hash_next;
    }
    core::ptr::null_mut()
}

/// Get an inode from the cache (load from disk if needed).
pub unsafe fn get_inode(dev: u32, numb: u32) -> *mut Inode {
    let hashi = (numb as usize) & INODE_HASH_MASK;

    // Search hash table
    let mut idx = (*glo::HASH_INODES.get())[hashi];
    while let Some(i) = idx {
        let rip = glo::get_inode_ptr(i as usize);
        if (*rip).i_num == numb && (*rip).i_dev == dev {
            if (*rip).i_count == 0 {
                let ext2 = glo::ext2_ptr();
                (*ext2).inode_cache_hit += 1;
                // Remove from unused list
                remove_from_unused(rip);
            }
            (*rip).i_count += 1;
            return rip;
        }
        idx = (*rip).i_hash_next;
    }

    // Cache miss
    let ext2 = glo::ext2_ptr();
    (*ext2).inode_cache_miss += 1;

    // Get a free inode slot
    let rip = get_free_inode();
    if rip.is_null() {
        return core::ptr::null_mut();
    }

    // If not free, unhash it
    if (*rip).i_num != NO_ENTRY {
        unhash_inode(rip);
    }

    // Remove from unused list
    remove_from_unused(rip);

    // Load the inode
    (*rip).i_dev = dev;
    (*rip).i_num = numb;
    (*rip).i_count = 1;
    if dev != NO_DEV {
        rw_inode(rip, READING);
    }
    (*rip).i_update = 0;
    (*rip).i_last_dpos = 0;
    (*rip).i_bsearch = NO_BLOCK;
    (*rip).i_last_pos_bl_alloc = 0;
    (*rip).i_last_dentry_size = 0;
    (*rip).i_mountpoint = FALSE;

    let opt = glo::OPT.get();
    (*rip).i_preallocation = (*opt).use_prealloc;
    (*rip).i_prealloc_count = 0;
    (*rip).i_prealloc_index = 0;

    for i in 0..EXT2_PREALLOC_BLOCKS {
        if (*rip).i_prealloc_blocks[i] != NO_BLOCK {
            if let Some(ref mut sp) = (*rip).i_sp {
                free_block(sp as &mut SuperBlock, (*rip).i_prealloc_blocks[i]);
            }
            (*rip).i_prealloc_blocks[i] = NO_BLOCK;
        }
    }

    addhash_inode(rip);
    rip
}

/// Put an inode (decrease reference count).
pub unsafe fn put_inode(rip: *mut Inode) {
    if rip.is_null() {
        return;
    }

    if (*rip).i_count < 1 {
        return;
    }

    (*rip).i_count -= 1;
    if (*rip).i_count == 0 {
        if (*rip).i_links_count == NO_LINK {
            // Free the inode
            truncate_inode(rip, 0);
            (*rip).i_dirt = IN_DIRTY;
            free_inode_(rip);
        }

        (*rip).i_mountpoint = FALSE;
        if (*rip).i_dirt == IN_DIRTY {
            rw_inode(rip, WRITING);
        }

        discard_preallocated_blocks(Some(&mut *rip));

        if (*rip).i_links_count == NO_LINK {
            unhash_inode(rip);
            (*rip).i_num = NO_ENTRY;
            add_to_unused_head(rip);
        } else {
            add_to_unused_tail(rip);
        }
    }
}

/// Read or write an inode from/to disk.
pub unsafe fn rw_inode(rip: *mut Inode, rw_flag: i32) {
    let sp = get_super((*rip).i_dev);
    if sp.is_null() {
        return;
    }
    (*rip).i_sp = Some(&mut *sp);

    let block_group_number = ((*rip).i_num - 1) / (*sp).s_inodes_per_group;
    let gd = get_group_desc(&mut *sp, block_group_number);
    if gd.is_null() {
        return;
    }

    let inode_size = ext2_inode_size(&*sp);
    let offset = (((*rip).i_num - 1) % (*sp).s_inodes_per_group) * inode_size;
    let b = (*gd).inode_table + (offset >> (*sp).s_blocksize_bits as u32);

    // Load the block containing this inode
    let bp = lmfs_get_block((*rip).i_dev, b as u64);
    if bp.is_null() {
        return;
    }

    let offset_in_block = offset & ((*sp).s_block_size as u32 - 1);

    if rw_flag == WRITING {
        if (*rip).i_update != 0 {
            update_times(rip);
        }
        if (*sp).s_rd_only == 0 {
            lmfs_markdirty(bp);
        }
    }

    // Copy between in-memory and on-disk inode via pointer into buffer
    let dip = ((*bp).data_ptr as usize + offset_in_block as usize) as *mut DInode;
    icopy(rip, dip, rw_flag);

    lmfs_put_block(bp, INODE_BLOCK);
    (*rip).i_dirt = IN_CLEAN;
}

/// Update times for an inode.
pub unsafe fn update_times(rip: *mut Inode) {
    let sp = (*rip).i_sp.as_mut().unwrap();
    if sp.s_rd_only != 0 {
        return;
    }

    let cur_time = clock_time() as u32;
    if ((*rip).i_update & ATIME) != 0 {
        (*rip).i_atime = cur_time;
    }
    if ((*rip).i_update & CTIME) != 0 {
        (*rip).i_ctime = cur_time;
    }
    if ((*rip).i_update & MTIME) != 0 {
        (*rip).i_mtime = cur_time;
    }
    (*rip).i_update = 0;
}

/// Duplicate an inode reference.
pub unsafe fn dup_inode(ip: *mut Inode) {
    (*ip).i_count += 1;
}

/// fs_putnode handler.
pub unsafe fn fs_putnode() -> i32 {
    let ext2 = glo::ext2_ptr();
    let rip = find_inode((*ext2).fs_dev, (*ext2).fs_m_in_type as u32);
    if rip.is_null() {
        return EINVAL;
    }

    let count = (*ext2).cch[0] as i32; // FIXME: proper message parsing
    if count <= 0 {
        return EINVAL;
    }
    if count > (*rip).i_count {
        return EINVAL;
    }

    (*rip).i_count -= count - 1;
    put_inode(rip);
    OK
}

// ── Internal helpers ──

unsafe fn remove_from_unused(rip: *mut Inode) {
    let idx = (rip as *const Inode as usize - glo::get_inode_ptr(0) as *const Inode as usize)
        / core::mem::size_of::<Inode>();

    let prev = (*rip).i_unused_prev;
    let next = (*rip).i_unused_next;

    if let Some(p) = prev {
        let p_ptr = glo::get_inode_ptr(p as usize);
        (*p_ptr).i_unused_next = next;
    } else {
        // rip is head
        *glo::UNUSED_INODES_HEAD.get() = next;
    }
    if let Some(n) = next {
        let n_ptr = glo::get_inode_ptr(n as usize);
        (*n_ptr).i_unused_prev = prev;
        *glo::UNUSED_INODES_HEAD.get() = next;
    }
}

unsafe fn add_to_unused_head(rip: *mut Inode) {
    let idx = (rip as *const Inode as usize - glo::get_inode_ptr(0) as *const Inode as usize)
        / core::mem::size_of::<Inode>();
    (*rip).i_unused_next = *glo::UNUSED_INODES_HEAD.get();
    (*rip).i_unused_prev = None;
    if let Some(next) = *glo::UNUSED_INODES_HEAD.get() {
        let next_ptr = glo::get_inode_ptr(next as usize);
        (*next_ptr).i_unused_prev = Some(idx as u16);
    }
    *glo::UNUSED_INODES_HEAD.get() = Some(idx as u16);
}

unsafe fn add_to_unused_tail(rip: *mut Inode) {
    // Find tail
    let mut tail: Option<u16> = None;
    let mut idx = *glo::UNUSED_INODES_HEAD.get();
    while let Some(i) = idx {
        let p = glo::get_inode_ptr(i as usize);
        tail = idx;
        idx = (*p).i_unused_next;
    }
    let my_idx = (rip as *const Inode as usize - glo::get_inode_ptr(0) as *const Inode as usize)
        / core::mem::size_of::<Inode>();
    (*rip).i_unused_prev = tail;
    (*rip).i_unused_next = None;
    if let Some(t) = tail {
        let t_ptr = glo::get_inode_ptr(t as usize);
        (*t_ptr).i_unused_next = Some(my_idx as u16);
    } else {
        *glo::UNUSED_INODES_HEAD.get() = Some(my_idx as u16);
    }
}

unsafe fn get_free_inode() -> *mut Inode {
    let head = *glo::UNUSED_INODES_HEAD.get();
    if let Some(idx) = head {
        let rip = glo::get_inode_ptr(idx as usize);
        rip
    } else {
        core::ptr::null_mut()
    }
}

/// Copy inode data between in-memory Inode and on-disk DInode.
unsafe fn icopy(rip: *mut Inode, dip: *mut DInode, direction: i32) {
    let norm = 1; // little-endian CPU
    if direction == READING {
        // Copy from on-disk DInode to in-memory Inode
        (*rip).i_mode = conv2(norm, (*dip).i_mode as u32) as u16;
        (*rip).i_uid = conv2(norm, (*dip).i_uid as u32) as u16;
        (*rip).i_size = conv4(norm, (*dip).i_size);
        (*rip).i_atime = conv4(norm, (*dip).i_atime);
        (*rip).i_ctime = conv4(norm, (*dip).i_ctime);
        (*rip).i_mtime = conv4(norm, (*dip).i_mtime);
        (*rip).i_dtime = conv4(norm, (*dip).i_dtime);
        (*rip).i_gid = conv2(norm, (*dip).i_gid as u32) as u16;
        (*rip).i_links_count = conv2(norm, (*dip).i_links_count as u32) as u16;
        (*rip).i_blocks = conv4(norm, (*dip).i_blocks);
        (*rip).i_flags = conv4(norm, (*dip).i_flags);
        (*rip).osd1 = (*dip).osd1;
        for i in 0..EXT2_N_BLOCKS {
            (*rip).i_block[i] = conv4(norm, (*dip).i_block[i]);
        }
        (*rip).i_generation = conv4(norm, (*dip).i_generation);
        (*rip).i_file_acl = conv4(norm, (*dip).i_file_acl);
        (*rip).i_dir_acl = conv4(norm, (*dip).i_dir_acl);
        (*rip).i_faddr = conv4(norm, (*dip).i_faddr);
        (*rip).osd2 = (*dip).osd2;
        (*rip).i_dirt = IN_CLEAN;
    } else {
        // Copy from in-memory Inode to on-disk DInode
        (*dip).i_mode = conv2(norm, (*rip).i_mode as u32) as u16;
        (*dip).i_uid = conv2(norm, (*rip).i_uid as u32) as u16;
        (*dip).i_size = conv4(norm, (*rip).i_size);
        (*dip).i_atime = conv4(norm, (*rip).i_atime);
        (*dip).i_ctime = conv4(norm, (*rip).i_ctime);
        (*dip).i_mtime = conv4(norm, (*rip).i_mtime);
        (*dip).i_dtime = conv4(norm, (*rip).i_dtime);
        (*dip).i_gid = conv2(norm, (*rip).i_gid as u32) as u16;
        (*dip).i_links_count = conv2(norm, (*rip).i_links_count as u32) as u16;
        (*dip).i_blocks = conv4(norm, (*rip).i_blocks);
        (*dip).i_flags = conv4(norm, (*rip).i_flags);
        (*dip).osd1 = (*rip).osd1;
        for i in 0..EXT2_N_BLOCKS {
            (*dip).i_block[i] = conv4(norm, (*rip).i_block[i]);
        }
        (*dip).i_generation = conv4(norm, (*rip).i_generation);
        (*dip).i_file_acl = conv4(norm, (*rip).i_file_acl);
        (*dip).i_dir_acl = conv4(norm, (*rip).i_dir_acl);
        (*dip).i_faddr = conv4(norm, (*rip).i_faddr);
        (*dip).osd2 = (*rip).osd2;
        (*rip).i_dirt = IN_CLEAN;
    }
}

/// Truncate inode (stub).
pub unsafe fn truncate_inode(rip: *mut Inode, _len: u64) -> i32 {
    // TODO: implement block deallocation
    (*rip).i_size = 0;
    for i in 0..EXT2_N_BLOCKS {
        if (*rip).i_block[i] != NO_BLOCK {
            if let Some(ref mut sp) = (*rip).i_sp {
                free_block(sp as &mut SuperBlock, (*rip).i_block[i]);
            }
            (*rip).i_block[i] = NO_BLOCK;
        }
    }
    (*rip).i_blocks = 0;
    (*rip).i_dirt = IN_DIRTY;
    OK
}

/// Free inode (stub).
pub unsafe fn free_inode_(rip: *mut Inode) {
    if let Some(sp) = (*rip).i_sp.as_mut() {
        let dev = (*rip).i_dev;
        let _b = (*rip).i_num;
        let mode = (*rip).i_mode;

        if _b <= NO_ENTRY || _b > sp.s_inodes_count {
            return;
        }
        // TODO: free_inode_bit(sp, b, (mode & I_TYPE) == I_DIRECTORY);
        (*rip).i_mode = I_NOT_ALLOC;
        let _ = dev;
        let _ = mode;
    }
}
