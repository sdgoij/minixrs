//! PFS global state — adapted from `minix/fs/pfs/glo.h` and `buf.h`
//!
//! All global state is accessed through raw pointers to satisfy
//! Rust 2024's `deny(static_mut_refs)`. No mutable references to
//! `static mut` are ever created — only `addr_of_mut!` and pointer
//! dereference are used.

use crate::pfs::consts::*;
use crate::pfs::types::*;
use core::cell::UnsafeCell;
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

/// Wrapper for `[Option<u16>; INODE_HASH_SIZE]`.
pub(crate) struct HashInodesCell(UnsafeCell<[Option<u16>; INODE_HASH_SIZE]>);
unsafe impl Sync for HashInodesCell {}
impl HashInodesCell {
    pub const fn new(val: [Option<u16>; INODE_HASH_SIZE]) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut [Option<u16>; INODE_HASH_SIZE] {
        self.0.get()
    }
}

/// Wrapper for `Option<u16>`.
pub(crate) struct OptionU16Cell(UnsafeCell<Option<u16>>);
unsafe impl Sync for OptionU16Cell {}
impl OptionU16Cell {
    pub const fn new(val: Option<u16>) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut Option<u16> {
        self.0.get()
    }
}

/// Hash table heads for inode lookup (index into inode_table).
pub(crate) static HASH_INODES: HashInodesCell = HashInodesCell::new([None; INODE_HASH_SIZE]);

/// Head of unused/free inode list.
pub(crate) static UNUSED_INODES_HEAD: OptionU16Cell = OptionU16Cell::new(None);

/// Buf free list: points to least recently used free block.
pub(crate) static BUF_FRONT: OptionU16Cell = OptionU16Cell::new(None);

/// Buf free list: points to most recently used free block.
pub(crate) static BUF_REAR: OptionU16Cell = OptionU16Cell::new(None);

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
