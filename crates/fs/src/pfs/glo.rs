//! PFS global state — adapted from `minix/fs/pfs/glo.h` and `buf.h`
//!
//! All global state is accessed through raw pointers to satisfy
//! Rust 2024's `deny(static_mut_refs)`. No mutable references to
//! `static mut` are ever created — only `addr_of_mut!` and pointer
//! dereference are used.

use crate::pfs::consts::*;
use crate::pfs::types::*;
use core::mem::MaybeUninit;

/// Global PFS state.
#[repr(C)]
pub struct PfsGlobal {
    pub err_code: i32,
    pub caller_uid: u16,
    pub caller_gid: u16,
    pub req_nr: i32,
    pub fs_dev: u32,
    pub unmountdone: i32,
    pub exitsignaled: i32,
    pub inode_table: [Inode; PFS_NR_INODES],
    pub buf_pool: [Buf; PIPE_NR_BUFS],
    /// Bitmap for inode allocation.
    pub inodemap: [BitchunkT; INODEMAP_CHUNKS],
}

/// Raw storage — only accessed via `addr_of_mut!` / raw pointers.
static mut PFS_STORAGE: MaybeUninit<PfsGlobal> = MaybeUninit::uninit();

/// Hash table heads for inode lookup (index into inode_table).
pub static mut HASH_INODES: [Option<u16>; INODE_HASH_SIZE] = [None; INODE_HASH_SIZE];

/// Head of unused/free inode list.
pub static mut UNUSED_INODES_HEAD: Option<u16> = None;

/// Buf free list: points to least recently used free block.
pub static mut BUF_FRONT: Option<u16> = None;

/// Buf free list: points to most recently used free block.
pub static mut BUF_REAR: Option<u16> = None;

/// Initialize globals. Must be called once before any access.
pub unsafe fn pfs_init_globals() {
    // SAFETY: PFS_STORAGE is only accessed once here before any other code runs.
    let p: *mut PfsGlobal = core::ptr::addr_of_mut!(PFS_STORAGE).cast();
    // SAFETY: we have exclusive access at init time.
    p.write(PfsGlobal {
        err_code: 0,
        caller_uid: 0,
        caller_gid: 0,
        req_nr: 0,
        fs_dev: NO_DEV,
        unmountdone: FALSE,
        exitsignaled: 0,
        inode_table: core::array::from_fn(|_| Inode::default()),
        buf_pool: core::array::from_fn(|_| Buf::default()),
        inodemap: [0; INODEMAP_CHUNKS],
    });
}

/// Get a raw pointer to PFS global state.
pub unsafe fn pfs_ptr() -> *mut PfsGlobal {
    core::ptr::addr_of_mut!(PFS_STORAGE).cast()
}

/// Get a raw pointer to the i-th inode in the table.
pub unsafe fn get_inode_ptr(idx: usize) -> *mut Inode {
    let pfs = core::ptr::addr_of_mut!(PFS_STORAGE).cast::<PfsGlobal>();
    let base = core::ptr::addr_of_mut!((*pfs).inode_table[0]);
    base.add(idx)
}

/// Get a raw pointer to the i-th buffer in the pool.
pub unsafe fn get_buf_ptr(idx: usize) -> *mut Buf {
    let pfs = core::ptr::addr_of_mut!(PFS_STORAGE).cast::<PfsGlobal>();
    let base = core::ptr::addr_of_mut!((*pfs).buf_pool[0]);
    base.add(idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pfs_init_globals() {
        unsafe {
            pfs_init_globals();
            let p = pfs_ptr();
            assert_eq!((*p).err_code, 0);
            assert_eq!((*p).fs_dev, NO_DEV);
            assert_eq!((*p).unmountdone, FALSE);
            assert_eq!((*p).exitsignaled, 0);
        }
    }

    #[test]
    fn test_inode_ptr_valid() {
        unsafe {
            pfs_init_globals();
            let ptr = get_inode_ptr(0);
            assert_eq!((*ptr).i_count, 0);
        }
    }

    #[test]
    fn test_buf_ptr_valid() {
        unsafe {
            pfs_init_globals();
            let ptr = get_buf_ptr(0);
            assert_eq!((*ptr).b_dev, NO_DEV);
        }
    }
}
