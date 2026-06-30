//! Inode cache management — adapted from `minix/fs/mfs/inode.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::types::*;

fn hash_inum(numb: u32) -> usize {
    (numb as usize) & INODE_HASH_MASK
}

fn ino_ref(idx: u16) -> *const Inode {
    unsafe { glo::get_inode_ptr(idx as usize) as *const Inode }
}

fn ino_mut(idx: u16) -> *mut Inode {
    unsafe { glo::get_inode_ptr(idx as usize) }
}

// Reference: inode.c addhash_inode()
fn addhash_inode(node_idx: u16) {
    let num = unsafe { (*ino_ref(node_idx)).i_num };
    let hashi = hash_inum(num);
    unsafe {
        let head: *mut Option<u16> = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
        let old_head = *head;
        *head = Some(node_idx);
        (*ino_mut(node_idx)).i_hash_next = old_head;
        (*ino_mut(node_idx)).i_hash_prev = None;
        if let Some(next) = old_head {
            (*ino_mut(next)).i_hash_prev = Some(node_idx);
        }
    }
}

// Reference: inode.c unhash_inode()
fn unhash_inode(node_idx: u16) {
    unsafe {
        let node = ino_mut(node_idx);
        let prev = (*node).i_hash_prev;
        let next = (*node).i_hash_next;
        let num = (*node).i_num;

        if let Some(p) = prev {
            (*ino_mut(p)).i_hash_next = next;
        } else {
            let head: *mut Option<u16> =
                core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hash_inum(num)]);
            *head = next;
        }
        if let Some(n) = next {
            (*ino_mut(n)).i_hash_prev = prev;
        }

        let node = ino_mut(node_idx);
        (*node).i_hash_next = None;
        (*node).i_hash_prev = None;
    }
}

// Reference: inode.c init_inode_cache()
pub fn init_inode_cache() {
    unsafe {
        let mfs = glo::mfs_ptr();
        (*mfs).inode_cache_hit = 0;
        (*mfs).inode_cache_miss = 0;

        for h in 0..INODE_HASH_SIZE {
            *core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[h]) = None;
        }

        let mut prev: Option<u16> = None;
        for i in 0..NR_INODES {
            let idx = i as u16;
            let inode = glo::get_inode_ptr(i);
            (*inode).i_num = NO_ENTRY;
            (*inode).i_count = 0;
            (*inode).i_unused_next = None;
            (*inode).i_unused_prev = prev;
            if let Some(p) = prev {
                (*ino_mut(p)).i_unused_next = Some(idx);
            }
            prev = Some(idx);
        }
        *glo::UNUSED_INODES_HEAD.get() = Some(0);
    }
}

// Reference: inode.c get_inode()
pub fn get_inode(dev: u32, numb: u32) -> Option<u16> {
    let hashi = hash_inum(numb);
    unsafe {
        let head = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
        let mut idx = *head;
        while let Some(i) = idx {
            let inode = ino_ref(i);
            if (*inode).i_num == numb && (*inode).i_dev == dev {
                if (*inode).i_count == 0 {
                    (*glo::mfs_ptr()).inode_cache_hit += 1;
                    remove_from_unused(i);
                }
                (*ino_mut(i)).i_count += 1;
                return Some(i);
            }
            idx = (*inode).i_hash_next;
        }

        (*glo::mfs_ptr()).inode_cache_miss += 1;

        let free_idx = get_free_inode()?;

        if (*ino_ref(free_idx)).i_num != NO_ENTRY {
            unhash_inode(free_idx);
        }
        remove_from_unused(free_idx);

        {
            let inode = ino_mut(free_idx);
            (*inode).i_dev = dev;
            (*inode).i_num = numb;
            (*inode).i_count = 1;
            (*inode).i_update = 0;
            (*inode).i_zsearch = NO_ZONE;
            (*inode).i_mountpoint = FALSE;
            (*inode).i_last_dpos = 0;
        }

        addhash_inode(free_idx);
        Some(free_idx)
    }
}

// Reference: inode.c find_inode()
pub fn find_inode(dev: u32, numb: u32) -> Option<u16> {
    let hashi = hash_inum(numb);
    unsafe {
        let head = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
        let mut idx = *head;
        while let Some(i) = idx {
            let inode = ino_ref(i);
            if (*inode).i_count > 0 && (*inode).i_num == numb && (*inode).i_dev == dev {
                return Some(i);
            }
            idx = (*inode).i_hash_next;
        }
    }
    None
}

// Reference: inode.c put_inode()
pub fn put_inode(inode_idx: Option<u16>) {
    let idx = match inode_idx {
        Some(i) => i,
        None => return,
    };
    unsafe {
        let inode = ino_mut(idx);
        if (*inode).i_count < 1 {
            return;
        }
        (*inode).i_count -= 1;
        if (*inode).i_count == 0 {
            if (*inode).i_nlinks == NO_LINK {
                (*inode).i_mode = I_NOT_ALLOC;
            }
            (*inode).i_mountpoint = FALSE;
            if (*inode).i_dirt == IN_DIRTY {
                // rw_inode
            }
            if (*inode).i_nlinks == NO_LINK {
                unhash_inode(idx);
                (*ino_mut(idx)).i_num = NO_ENTRY;
                add_to_unused_front(idx);
            } else {
                add_to_unused_back(idx);
            }
        }
    }
}

// Reference: inode.c alloc_inode()
pub fn alloc_inode(dev: u32, bits: u16) -> Option<u16> {
    unsafe {
        let sp = super_block::get_super(dev);
        if sp.is_null() || (*sp).s_rd_only != 0 {
            (*glo::mfs_ptr()).err_code = if sp.is_null() { EINVAL } else { EROFS };
            return None;
        }
        let b = super_block::alloc_bit(&mut *sp, IMAP, (*sp).s_isearch);
        if b == NO_BIT {
            (*glo::mfs_ptr()).err_code = ENOSPC;
            return None;
        }
        (*sp).s_isearch = b;
        let rip = get_inode(NO_DEV, b)?;
        let inode = ino_mut(rip);
        (*inode).i_mode = bits;
        (*inode).i_nlinks = NO_LINK;
        (*inode).i_uid = (*glo::mfs_ptr()).caller_uid;
        (*inode).i_gid = (*glo::mfs_ptr()).caller_gid;
        (*inode).i_dev = dev;
        (*inode).i_ndzones = (*sp).s_ndzones as u32;
        (*inode).i_nindirs = (*sp).s_nindirs as u32;
        (*inode).i_sp = Some(&mut *sp);
        (*inode).i_size = 0;
        (*inode).i_update = ATIME | CTIME | MTIME;
        (*inode).i_dirt = IN_DIRTY;
        for z in &mut (*inode).i_zone {
            *z = NO_ZONE;
        }
        Some(rip)
    }
}

// Reference: inode.c dup_inode()
pub fn dup_inode(idx: u16) {
    unsafe {
        (*ino_mut(idx)).i_count += 1;
    }
}

use libs::libminixfs::cache::{lmfs_get_block, lmfs_markdirty, lmfs_put_block};

// Reference: inode.c rw_inode()
pub fn rw_inode(rip_idx: u16, rw_flag: i32) -> i32 {
    unsafe {
        let rip = glo::get_inode_ptr(rip_idx as usize);
        let sp = super_block::get_super((*rip).i_dev);
        if sp.is_null() {
            return EINVAL;
        }
        (*rip).i_sp = Some(&mut *sp);

        let offset = START_BLOCK + (*sp).s_imap_blocks as u32 + (*sp).s_zmap_blocks as u32;
        let b = ((*rip).i_num - 1) / (*sp).s_inodes_per_block + offset;
        let bp = lmfs_get_block((*rip).i_dev, b as u64);
        if bp.is_null() {
            return EIO;
        }

        let dip2_offset = (((*rip).i_num - 1) % (*sp).s_inodes_per_block) as usize * V2_INODE_SIZE;
        let dip2 = (*bp).data_ptr.add(dip2_offset) as *mut D2Inode;

        if rw_flag == WRITING {
            if (*rip).i_update != 0 {
                update_times(rip_idx);
            }
            if (*sp).s_rd_only == FALSE {
                lmfs_markdirty(bp);
            }
        }

        let norm = (*sp).s_native;
        if rw_flag == READING {
            // Copy on-disk inode to in-memory inode, swapping bytes if needed.
            (*rip).i_mode = utility::conv2(norm, (*dip2).d2_mode as i32) as u16;
            (*rip).i_uid = utility::conv2(norm, (*dip2).d2_uid as i32) as u16;
            (*rip).i_nlinks = utility::conv2(norm, (*dip2).d2_nlinks as i32) as u16;
            (*rip).i_gid = utility::conv2(norm, (*dip2).d2_gid as i32) as u16;
            (*rip).i_size = utility::conv4(norm, (*dip2).d2_size as i64) as i32;
            (*rip).i_atime = utility::conv4(norm, (*dip2).d2_atime as i64) as u32;
            (*rip).i_ctime = utility::conv4(norm, (*dip2).d2_ctime as i64) as u32;
            (*rip).i_mtime = utility::conv4(norm, (*dip2).d2_mtime as i64) as u32;
            (*rip).i_ndzones = V2_NR_DZONES as u32;
            (*rip).i_nindirs = v2_indirects((*sp).s_block_size as usize) as u32;
            for i in 0..V2_NR_TZONES {
                (*rip).i_zone[i] = utility::conv4(norm, (*dip2).d2_zone[i] as i64) as u32;
            }
        } else {
            // Copy in-memory inode to on-disk inode, swapping bytes if needed.
            (*dip2).d2_mode = utility::conv2(norm, (*rip).i_mode as i32) as u16;
            (*dip2).d2_uid = utility::conv2(norm, (*rip).i_uid as i32) as i16;
            (*dip2).d2_nlinks = utility::conv2(norm, (*rip).i_nlinks as i32) as u16;
            (*dip2).d2_gid = utility::conv2(norm, (*rip).i_gid as i32) as u16;
            (*dip2).d2_size = utility::conv4(norm, (*rip).i_size as i64) as i32;
            (*dip2).d2_atime = utility::conv4(norm, (*rip).i_atime as i64) as i32;
            (*dip2).d2_ctime = utility::conv4(norm, (*rip).i_ctime as i64) as i32;
            (*dip2).d2_mtime = utility::conv4(norm, (*rip).i_mtime as i64) as i32;
            for i in 0..V2_NR_TZONES {
                (*dip2).d2_zone[i] = utility::conv4(norm, (*rip).i_zone[i] as i64) as u32;
            }
        }

        lmfs_put_block(bp, INODE_BLOCK);
        (*rip).i_dirt = IN_CLEAN;

        OK
    }
}

// Reference: inode.c update_times()
pub fn update_times(rip_idx: u16) {
    unsafe {
        let inode = ino_mut(rip_idx);
        let sp = match (*inode).i_sp.as_mut() {
            Some(s) => s,
            None => return,
        };
        if sp.s_rd_only != 0 {
            return;
        }
        let cur_time = utility::clock_time() as u32;
        if (*inode).i_update & ATIME != 0 {
            (*inode).i_atime = cur_time;
        }
        if (*inode).i_update & CTIME != 0 {
            (*inode).i_ctime = cur_time;
        }
        if (*inode).i_update & MTIME != 0 {
            (*inode).i_mtime = cur_time;
        }
        (*inode).i_update = 0;
    }
}

// Reference: inode.c fs_putnode()
pub fn fs_putnode() -> i32 {
    unsafe {
        let ino = (*glo::mfs_ptr()).cch[0] as u32;
        let count = (*glo::mfs_ptr()).cch[1];
        let dev = (*glo::mfs_ptr()).fs_dev;

        let rip = find_inode(dev, ino);
        let rip = match rip {
            Some(i) => i,
            None => return EINVAL,
        };

        if count <= 0 || count > (*glo::get_inode_ptr(rip as usize)).i_count {
            return EINVAL;
        }

        // Decrease refcount by (count - 1); put_inode consumes the last one.
        {
            let inode = &mut *glo::get_inode_ptr(rip as usize);
            (*inode).i_count -= count - 1;
        }
        put_inode(Some(rip));

        OK
    }
}

// ── Private helpers ──

unsafe fn remove_from_unused(idx: u16) {
    let n = ino_mut(idx);
    let prev = (*n).i_unused_prev;
    let next = (*n).i_unused_next;
    if let Some(p) = prev {
        (*ino_mut(p)).i_unused_next = next;
    } else {
        *glo::UNUSED_INODES_HEAD.get() = next;
    }
    if let Some(n) = next {
        (*ino_mut(n)).i_unused_prev = prev;
    }
    (*n).i_unused_next = None;
    (*n).i_unused_prev = None;
}

unsafe fn get_free_inode() -> Option<u16> {
    let head = *glo::UNUSED_INODES_HEAD.get();
    if head.is_none() {
        (*glo::mfs_ptr()).err_code = ENFILE;
    }
    head
}

unsafe fn add_to_unused_front(idx: u16) {
    let old_head = *glo::UNUSED_INODES_HEAD.get();
    *glo::UNUSED_INODES_HEAD.get() = Some(idx);
    let inode = ino_mut(idx);
    (*inode).i_unused_next = old_head;
    (*inode).i_unused_prev = None;
    if let Some(n) = old_head {
        (*ino_mut(n)).i_unused_prev = Some(idx);
    }
}

unsafe fn add_to_unused_back(idx: u16) {
    let mut tail: Option<u16> = None;
    let mut cur = *glo::UNUSED_INODES_HEAD.get();
    while let Some(i) = cur {
        tail = Some(i);
        cur = (*ino_ref(i)).i_unused_next;
    }
    let inode = ino_mut(idx);
    (*inode).i_unused_next = None;
    (*inode).i_unused_prev = tail;
    if let Some(t) = tail {
        (*ino_mut(t)).i_unused_next = Some(idx);
    } else {
        *glo::UNUSED_INODES_HEAD.get() = Some(idx);
    }
}

use crate::mfs::super_block;
use crate::mfs::utility;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_hash_inum() {
        assert!(hash_inum(1) < INODE_HASH_SIZE);
    }
}
