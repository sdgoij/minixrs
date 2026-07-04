//! Inode table management — adapted from `minix/fs/pfs/inode.c`

use crate::pfs::bitmap::*;
use crate::pfs::consts::*;
use crate::pfs::glo;
use crate::pfs::types::*;
use crate::pfs::utility;

fn ino_ref(idx: u16) -> *const Inode {
    unsafe { glo::get_inode_ptr(idx as usize) as *const Inode }
}

fn ino_mut(idx: u16) -> *mut Inode {
    unsafe { glo::get_inode_ptr(idx as usize) }
}

/// Compute hash index for an inode number.
fn hash_inum(numb: u32) -> usize {
    (numb as usize) & INODE_HASH_MASK
}

/// Add an inode to the hash table.
// Reference: inode.c addhash_inode()
fn addhash_inode(node_idx: u16) {
    let num = unsafe { (*ino_ref(node_idx)).i_num };
    let hashi = hash_inum(num);
    unsafe {
        let head: *mut Option<u16> = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
        let old_head = *head;
        *head = Some(node_idx);
        (*ino_mut(node_idx)).i_hash_next = old_head;
    }
}

/// Remove an inode from the hash table.
// Reference: inode.c unhash_inode()
fn unhash_inode(node_idx: u16) {
    unsafe {
        let num = (*ino_ref(node_idx)).i_num;
        let hashi = hash_inum(num);
        let head: *mut Option<u16> = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);

        let mut cur = *head;
        let mut prev: Option<u16> = None;
        while let Some(idx) = cur {
            if idx == node_idx {
                if let Some(p) = prev {
                    (*ino_mut(p)).i_hash_next = (*ino_ref(idx)).i_hash_next;
                } else {
                    *head = (*ino_ref(idx)).i_hash_next;
                }
                (*ino_mut(node_idx)).i_hash_next = None;
                return;
            }
            prev = cur;
            cur = (*ino_ref(idx)).i_hash_next;
        }
    }
}

// Reference: inode.c init_inode_cache()
pub fn init_inode_cache() {
    unsafe {
        // Initialize hash lists
        for h in 0..INODE_HASH_SIZE {
            *core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[h]) = None;
        }

        // Initialize unused/free list
        *glo::UNUSED_INODES_HEAD.get() = None;

        // Add all inodes to the unused/free list
        for i in 0..PFS_NR_INODES {
            let idx = i as u16;
            let inode = glo::get_inode_ptr(i);
            (*inode).i_num = NO_ENTRY;
            (*inode).i_count = 0;
            (*inode).i_unused_next = None;

            let head_ptr = glo::UNUSED_INODES_HEAD.get();
            (*inode).i_unused_next = *head_ptr;
            *head_ptr = Some(idx);
        }

        // Reserve inode 0 (bit 0) to prevent it from being allocated
        if alloc_bit() != NO_BIT {
            // First allocation always succeeds — this reserves bit 0
        }
    }
}

/// Find an inode in the hash table by device and number.
/// If found and currently unused (i_count == 0), removes it from the free list.
/// If not found, allocates a new slot from the free list.
// Reference: inode.c get_inode()
pub fn get_inode(dev: u32, numb: u32) -> Option<u16> {
    let hashi = hash_inum(numb);
    unsafe {
        let head: *mut Option<u16> = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
        let mut idx = *head;
        while let Some(i) = idx {
            let inode = ino_ref(i);
            if (*inode).i_num == numb && (*inode).i_dev == dev {
                // If unused, remove from free list
                if (*inode).i_count == 0 {
                    remove_from_unused(i);
                }
                (*ino_mut(i)).i_count += 1;
                return Some(i);
            }
            idx = (*inode).i_hash_next;
        }

        // Not found — get a free inode
        let free_idx = get_free_inode()?;

        // If it was previously hashed, unhash it
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
        }

        addhash_inode(free_idx);
        Some(free_idx)
    }
}

/// Find an inode by number only (ignoring device).
/// Only returns inodes with i_count > 0.
// Reference: inode.c find_inode()
pub fn find_inode(numb: u32) -> Option<u16> {
    let hashi = hash_inum(numb);
    unsafe {
        let head: *mut Option<u16> = core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[hashi]);
        let mut idx = *head;
        while let Some(i) = idx {
            let inode = ino_ref(i);
            if (*inode).i_count > 0 && (*inode).i_num == numb {
                return Some(i);
            }
            idx = (*inode).i_hash_next;
        }
    }
    None
}

/// Release an inode reference.
///
/// Decrements the reference count. If it drops to zero and the inode
/// has no links, frees the inode back to the pool.
// Reference: inode.c put_inode()
pub fn put_inode(rip_idx: Option<u16>) {
    let idx = match rip_idx {
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
                // No links: free the inode
                truncate_inode(idx, 0);
                (*inode).i_mode = I_NOT_ALLOC;
                free_inode(idx);

                // Put at front of free list
                unhash_inode(idx);
                (*inode).i_num = NO_ENTRY;
                (*inode).i_dev = NO_DEV;
                (*inode).i_rdev = NO_DEV;
                add_to_unused_front(idx);
            } else {
                // Still has links: truncate and cache at back of free list
                truncate_inode(idx, 0);
                add_to_unused_back(idx);
            }
        }
    }
}

/// Allocate a new inode on the given device.
// Reference: inode.c alloc_inode()
pub fn alloc_inode(dev: u32, bits: u16, uid: u16, gid: u16) -> Option<u16> {
    let b = alloc_bit();
    if b == NO_BIT {
        unsafe {
            (*glo::pfs_ptr()).err_code = ENOSPC;
        }
        return None;
    }
    let i_num = b as u32;

    let rip = match get_inode(dev, i_num) {
        Some(ip) => ip,
        None => {
            free_bit(b);
            return None;
        }
    };

    unsafe {
        let inode = ino_mut(rip);
        (*inode).i_mode = bits;
        (*inode).i_nlinks = NO_LINK;
        (*inode).i_uid = uid;
        (*inode).i_gid = gid;
        wipe_inode(rip);
    }

    Some(rip)
}

/// Truncate an inode to a given size.
///
/// For pipes, only truncation to 0 is supported.
// Reference: link.c truncate_inode()
pub fn truncate_inode(rip_idx: u16, newsize: i64) -> i32 {
    if newsize != 0 {
        return EINVAL;
    }
    unsafe {
        let inode = ino_mut(rip_idx);
        (*inode).i_size = newsize;
        wipe_inode(rip_idx);
    }
    OK
}

/// Erase volatile fields in an inode.
// Reference: inode.c wipe_inode()
pub fn wipe_inode(rip_idx: u16) {
    unsafe {
        let inode = ino_mut(rip_idx);
        (*inode).i_size = 0;
        (*inode).i_update = (ATIME | CTIME | MTIME) as u8;
    }
}

/// Return an inode number to the free pool.
// Reference: inode.c free_inode()
pub fn free_inode(rip_idx: u16) {
    unsafe {
        let inum = (*ino_ref(rip_idx)).i_num;
        if inum == 0 || inum >= PFS_NR_INODES as u32 {
            return;
        }
        free_bit(inum as BitT);
    }
}

/// Update atime/mtime/ctime if their respective flags are set.
// Reference: inode.c update_times()
pub fn update_times(rip_idx: u16) {
    unsafe {
        let inode = ino_mut(rip_idx);
        let cur_time = utility::clock_time();
        if (*inode).i_update as u32 & ATIME != 0 {
            (*inode).i_atime = cur_time;
        }
        if (*inode).i_update as u32 & CTIME != 0 {
            (*inode).i_ctime = cur_time;
        }
        if (*inode).i_update as u32 & MTIME != 0 {
            (*inode).i_mtime = cur_time;
        }
        (*inode).i_update = 0;
    }
}

/// Increment the reference count of an inode.
// Reference: inode.c dup_inode()
pub fn dup_inode(ip_idx: u16) {
    unsafe {
        (*ino_mut(ip_idx)).i_count += 1;
    }
}

/// VFS putnode handler — decrease inode reference count.
// Reference: inode.c fs_putnode()
pub fn fs_putnode() -> i32 {
    todo!("fs_putnode: not yet wired")
}


unsafe fn remove_from_unused(idx: u16) {
    let head_ptr = glo::UNUSED_INODES_HEAD.get();
    let inode = ino_mut(idx);
    let next = (*inode).i_unused_next;

    // Find prev by scanning from head
    let mut cur = *head_ptr;
    let mut prev: Option<u16> = None;
    while let Some(c) = cur {
        if c == idx {
            break;
        }
        prev = cur;
        cur = (*ino_ref(c)).i_unused_next;
    }

    if let Some(p) = prev {
        (*ino_mut(p)).i_unused_next = next;
    } else {
        *head_ptr = next;
    }
    (*inode).i_unused_next = None;
}

unsafe fn get_free_inode() -> Option<u16> {
    let head_ptr = glo::UNUSED_INODES_HEAD.get();
    let head = *head_ptr;
    if head.is_none() {
        (*glo::pfs_ptr()).err_code = ENFILE;
    }
    head
}

unsafe fn add_to_unused_front(idx: u16) {
    let head_ptr = glo::UNUSED_INODES_HEAD.get();
    let inode = ino_mut(idx);
    (*inode).i_unused_next = *head_ptr;
    *head_ptr = Some(idx);
}

unsafe fn add_to_unused_back(idx: u16) {
    let head_ptr = glo::UNUSED_INODES_HEAD.get();
    let inode = ino_mut(idx);
    (*inode).i_unused_next = None;

    // Find the tail
    let mut tail: Option<u16> = None;
    let mut cur = *head_ptr;
    while let Some(c) = cur {
        tail = Some(c);
        cur = (*ino_ref(c)).i_unused_next;
    }

    if let Some(t) = tail {
        (*ino_mut(t)).i_unused_next = Some(idx);
    } else {
        *head_ptr = Some(idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            glo::pfs_init_globals();
            init_inode_cache();
        }
    }

    #[test]
    fn test_init_inode_cache() {
        init();
        // Verify hash table is empty of valid entries
        for h in 0..INODE_HASH_SIZE {
            unsafe {
                assert!((*core::ptr::addr_of_mut!((*glo::HASH_INODES.get())[h])).is_none());
            }
        }
        // Verify free list has entries
        unsafe {
            let head = glo::UNUSED_INODES_HEAD.get();
            assert!((*head).is_some());
        }
    }

    #[test]
    fn test_get_inode_creates_new() {
        init();
        let ip = get_inode(1, 42);
        assert!(ip.is_some());
        let idx = ip.unwrap();
        unsafe {
            assert_eq!((*ino_ref(idx)).i_dev, 1);
            assert_eq!((*ino_ref(idx)).i_num, 42);
            assert_eq!((*ino_ref(idx)).i_count, 1);
        }
    }

    #[test]
    fn test_get_inode_finds_existing() {
        init();
        let ip1 = get_inode(1, 42).unwrap();
        let ip2 = get_inode(1, 42).unwrap();
        assert_eq!(ip1, ip2);
        unsafe {
            assert_eq!((*ino_ref(ip2)).i_count, 2);
        }
    }

    #[test]
    fn test_find_inode() {
        init();
        get_inode(1, 99);
        assert!(find_inode(99).is_some());
        assert!(find_inode(999).is_none());
    }

    #[test]
    fn test_put_inode_releases() {
        init();
        let ip = get_inode(1, 77).unwrap();
        put_inode(Some(ip));
        unsafe {
            assert_eq!((*ino_ref(ip)).i_count, 0);
        }
    }

    #[test]
    fn test_alloc_inode() {
        init();
        let ip = alloc_inode(1, I_NAMED_PIPE, 0, 0);
        assert!(ip.is_some());
        let idx = ip.unwrap();
        unsafe {
            assert_eq!((*ino_ref(idx)).i_mode, I_NAMED_PIPE);
            assert_eq!((*ino_ref(idx)).i_nlinks, NO_LINK);
        }
    }

    #[test]
    fn test_update_times() {
        init();
        let ip = get_inode(1, 50).unwrap();
        unsafe {
            (*ino_mut(ip)).i_update = ATIME as u8 | CTIME as u8 | MTIME as u8;
        }
        update_times(ip);
        unsafe {
            assert_eq!((*ino_ref(ip)).i_update, 0);
        }
    }

    #[test]
    fn test_dup_inode() {
        init();
        let ip = get_inode(1, 10).unwrap();
        unsafe {
            assert_eq!((*ino_ref(ip)).i_count, 1);
        }
        dup_inode(ip);
        unsafe {
            assert_eq!((*ino_ref(ip)).i_count, 2);
        }
    }

    #[test]
    fn test_truncate_inode() {
        init();
        let ip = get_inode(1, 30).unwrap();
        unsafe {
            (*ino_mut(ip)).i_size = 100;
        }
        assert_eq!(truncate_inode(ip, 0), OK);
        unsafe {
            assert_eq!((*ino_ref(ip)).i_size, 0);
        }
    }

    #[test]
    fn test_truncate_inode_nonzero_fails() {
        init();
        let ip = get_inode(1, 31).unwrap();
        assert_eq!(truncate_inode(ip, 100), EINVAL);
    }

    #[test]
    #[should_panic(expected = "not yet wired")]
    fn test_fs_putnode_panics() {
        fs_putnode();
    }
}
