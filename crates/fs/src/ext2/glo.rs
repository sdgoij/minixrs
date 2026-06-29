//! Ext2 global state — adapted from `minix/fs/ext2/glo.h`
//!
//! All global state is accessed through raw pointers to satisfy
//! Rust 2024's `deny(static_mut_refs)`. No mutable references to
//! `static mut` are ever created — only `addr_of_mut!` and pointer
//! dereference are used.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicI32, AtomicPtr, Ordering};

use crate::ext2::consts::*;
use crate::ext2::types::*;
use core::mem::MaybeUninit;

/// Global ext2 state.
#[repr(C)]
pub struct Ext2Global {
    pub err_code: i32,
    pub rdwt_err: i32,
    pub cch: [i32; NR_INODES],
    pub fs_m_in_type: i32,
    pub fs_m_in_source: u16,
    pub fs_m_in_m_type: i32,
    pub caller_uid: u16,
    pub caller_gid: u16,
    pub req_nr: i32,
    pub user_path: [u8; PATH_MAX],
    pub fs_dev: u32,
    pub fs_dev_label: [u8; 16],
    pub unmountdone: i32,
    pub exitsignaled: i32,
    pub group_descriptors_dirty: i32,
    pub le_CPU: i32,
    pub inode_table: [Inode; NR_INODES],
    pub inode_cache_hit: u32,
    pub inode_cache_miss: u32,
}

/// Raw storage — only accessed via `addr_of_mut!` / raw pointers.
// Use ext2_ptr() helper instead of accessing this directly.
pub(crate) static mut EXT2_STORAGE: MaybeUninit<Ext2Global> = MaybeUninit::uninit();

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

/// Wrapper for `Opt` — global options.
pub(crate) struct OptCell(UnsafeCell<Opt>);
unsafe impl Sync for OptCell {}
impl OptCell {
    pub const fn new(val: Opt) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut Opt {
        self.0.get()
    }
}

/// Hash table heads for inode lookup.
pub(crate) static HASH_INODES: HashInodesCell = HashInodesCell::new([None; INODE_HASH_SIZE]);

/// Head of unused/free inode list.
pub(crate) static UNUSED_INODES_HEAD: UnusedInodesHeadCell = UnusedInodesHeadCell::new(None);

/// Global options.
pub(crate) static OPT: OptCell = OptCell::new(Opt {
    use_orlov: TRUE,
    mfsalloc: FALSE,
    use_reserved_blocks: FALSE,
    block_with_super: 0,
    use_prealloc: FALSE,
});

/// Group descriptors dirty flag.
pub(crate) static GROUP_DESCRIPTORS_DIRTY: AtomicI32 = AtomicI32::new(0);

/// Super block pointer (single superblock for ext2).
pub(crate) static SUPERBLOCK: AtomicPtr<SuperBlock> = AtomicPtr::new(core::ptr::null_mut());

/// Initialize globals. Must be called once before any access.
pub unsafe fn ext2_init_globals() {
    let p: *mut Ext2Global = core::ptr::addr_of_mut!(EXT2_STORAGE).cast();
    p.write(Ext2Global {
        err_code: 0,
        rdwt_err: 0,
        cch: [0; NR_INODES],
        fs_m_in_type: 0,
        fs_m_in_source: 0,
        fs_m_in_m_type: 0,
        caller_uid: INVAL_UID,
        caller_gid: INVAL_GID,
        req_nr: 0,
        user_path: [0; PATH_MAX],
        fs_dev: NO_DEV,
        fs_dev_label: [0; 16],
        unmountdone: 0,
        exitsignaled: 0,
        group_descriptors_dirty: 0,
        le_CPU: 1,
        inode_table: core::array::from_fn(|_| Inode::default()),
        inode_cache_hit: 0,
        inode_cache_miss: 0,
    });
}

/// Get a raw pointer to ext2 global state.
pub unsafe fn ext2_ptr() -> *mut Ext2Global {
    core::ptr::addr_of_mut!(EXT2_STORAGE).cast()
}

/// Get a raw pointer to the i-th inode.
pub unsafe fn get_inode_ptr(idx: usize) -> *mut Inode {
    let ext2 = core::ptr::addr_of_mut!(EXT2_STORAGE).cast::<Ext2Global>();
    let base = core::ptr::addr_of_mut!((*ext2).inode_table[0]);
    base.add(idx)
}
