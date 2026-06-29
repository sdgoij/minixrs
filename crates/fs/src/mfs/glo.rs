//! MFS global state — adapted from `minix/fs/mfs/glo.h`
//!
//! All global state is accessed through raw pointers to satisfy
//! Rust 2024's `deny(static_mut_refs)`. No mutable references to
//! `static mut` are ever created — only `addr_of_mut!` and pointer
//! dereference are used.

use crate::mfs::consts::*;
use crate::mfs::types::*;
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;

/// Global MFS state.
#[repr(C)]
pub struct MfsGlobal {
    pub err_code: i32,
    pub cch: [i32; NR_INODES],
    pub caller_uid: u16,
    pub caller_gid: u16,
    pub req_nr: i32,
    pub user_path: [u8; PATH_MAX],
    pub fs_dev: u32,
    pub fs_dev_label: [u8; 16],
    pub unmountdone: i32,
    pub exitsignaled: i32,
    pub inode_table: [Inode; NR_INODES],
    pub super_blocks: [SuperBlock; 8],
    pub inode_cache_hit: u32,
    pub inode_cache_miss: u32,
}

/// Raw storage — only accessed via `addr_of_mut!` / raw pointers.
static mut MFS_STORAGE: MaybeUninit<MfsGlobal> = MaybeUninit::uninit();

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

/// Wrapper for `Option<u16>` — the unused inodes list head.
pub(crate) struct UnusedInodesHeadCell(UnsafeCell<Option<u16>>);
unsafe impl Sync for UnusedInodesHeadCell {}
impl UnusedInodesHeadCell {
    pub const fn new(val: Option<u16>) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut Option<u16> {
        self.0.get()
    }
}

/// Hash table heads for inode lookup.
pub(crate) static HASH_INODES: HashInodesCell = HashInodesCell::new([None; INODE_HASH_SIZE]);

/// Head of unused/free inode list.
pub(crate) static UNUSED_INODES_HEAD: UnusedInodesHeadCell = UnusedInodesHeadCell::new(None);

/// Initialize globals. Must be called once before any access.
pub unsafe fn mfs_init_globals() {
    // SAFETY: MFS_STORAGE is only accessed once here before any other code runs.
    let p: *mut MfsGlobal = core::ptr::addr_of_mut!(MFS_STORAGE).cast();
    // SAFETY: we have exclusive access at init time.
    p.write(MfsGlobal {
        err_code: 0,
        cch: [0; NR_INODES],
        caller_uid: INVAL_UID,
        caller_gid: INVAL_GID,
        req_nr: 0,
        user_path: [0; PATH_MAX],
        fs_dev: NO_DEV,
        fs_dev_label: [0; 16],
        unmountdone: 0,
        exitsignaled: 0,
        inode_table: core::array::from_fn(|_| Inode::default()),
        super_blocks: core::array::from_fn(|_| SuperBlock::default()),
        inode_cache_hit: 0,
        inode_cache_miss: 0,
    });
}

/// Get a raw pointer to MFS global state.
pub unsafe fn mfs_ptr() -> *mut MfsGlobal {
    core::ptr::addr_of_mut!(MFS_STORAGE).cast()
}

/// Get a raw pointer to the i-th inode.
pub unsafe fn get_inode_ptr(idx: usize) -> *mut Inode {
    let mfs = core::ptr::addr_of_mut!(MFS_STORAGE).cast::<MfsGlobal>();
    // SAFETY: we take the address of the first element via addr_of_mut!,
    // which does NOT create a reference to the static.
    let base = core::ptr::addr_of_mut!((*mfs).inode_table[0]);
    base.add(idx)
}

/// Get a raw pointer to the i-th super block.
pub unsafe fn get_super_ptr(idx: usize) -> *mut SuperBlock {
    let mfs = core::ptr::addr_of_mut!(MFS_STORAGE).cast::<MfsGlobal>();
    let base = core::ptr::addr_of_mut!((*mfs).super_blocks[0]);
    base.add(idx)
}
