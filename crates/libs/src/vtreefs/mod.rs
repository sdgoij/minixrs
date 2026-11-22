//! VTreeFS — Virtual Tree Filesystem library and FS-server protocol loop.
//!
//! Provides a fixed-size inode table with a tree structure for virtual
//! filesystems (ProcFS, DEVMAN, ...) plus the VFS ↔ FS message loop that
//! serves those trees over the port's FS protocol (the VFS `req_*` message
//! layouts and `FS_BASE` request codes from `crates/servers/src/vfs/request.rs`).
//! Ported from `.refs/minix-3.3.0/minix/lib/libvtreefs/`.
//!
//! The inode table is a single `UnsafeCell<[INode; MAX_INODES]>` behind a
//! `Sync` impl — safe because MINIX servers are single-threaded.

#![allow(dead_code, clippy::type_complexity)]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use arch_common::ipc::Message;

/// Sentinel for "no index in parent".
pub const NO_INDEX: i32 = -1;
/// Maximum number of inodes in the table.
pub const MAX_INODES: usize = 1024;
/// Maximum path component length (`PNAME_MAX` in C).
pub const PNAME_MAX: usize = 255;
/// Maximum path length (`PATH_MAX` in C).
pub const PATH_MAX: usize = 4096;

/// Inode scheduled for deletion (`I_DELETED` in C).
pub const I_DELETED: u32 = 0x1;

/// Opaque user data stored in each inode (e.g. a file-handler pointer).
pub type CbData = usize;

/// File metadata carried by each inode.
#[derive(Debug, Clone, Copy)]
pub struct InodeStat {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub dev: u64,
}

/// A single node in the virtual directory tree.
#[derive(Debug, Clone, Copy)]
pub struct INode {
    pub id: u32,
    pub name: [u8; 64],
    pub parent_id: u32,
    pub first_child: Option<u32>,
    pub next_sibling: Option<u32>,
    pub stat: InodeStat,
    pub cbdata: CbData,
    pub count: u32,   // reference count (held by VFS between lookup and putnode)
    pub index: i32,   // index in the parent directory, or NO_INDEX
    pub indexed: u32, // number of indexed entries in this directory
    pub flags: u32,   // I_DELETED
}

/// Hook table — function pointers that VTreeFS calls at various points.
///
/// The read hook fills `buf` with data from the file at `offset` and returns
/// the number of bytes produced (it owns no storage of its own — VTreeFS
/// copies the bytes to the caller's grant).
#[derive(Clone, Copy)]
pub struct FsHooks {
    pub init_hook: Option<fn()>,
    pub cleanup_hook: Option<fn()>,
    pub lookup_hook: Option<fn(parent: u32, name: &str) -> i32>,
    pub getdents_hook: Option<fn(node: u32) -> i32>,
    pub read_hook: Option<fn(node: u32, offset: u64, buf: &mut [u8]) -> usize>,
    /// `caller` is the endpoint of the process the lookup or readlink is for,
    /// which is what lets a target depend on who is asking (`/proc/self`).
    pub rdlink_hook: Option<fn(node: u32, caller: i32, buf: &mut [u8]) -> i32>,
    pub message_hook: Option<fn(msg: &mut [u8; 64]) -> i32>,
}

impl FsHooks {
    /// A hook table with every hook unset.
    pub const fn empty() -> FsHooks {
        FsHooks {
            init_hook: None,
            cleanup_hook: None,
            lookup_hook: None,
            getdents_hook: None,
            read_hook: None,
            rdlink_hook: None,
            message_hook: None,
        }
    }
}

const ZERO_INODE: INode = INode {
    id: 0,
    name: [0u8; 64],
    parent_id: 0,
    first_child: None,
    next_sibling: None,
    stat: InodeStat {
        mode: 0,
        uid: 0,
        gid: 0,
        size: 0,
        dev: 0,
    },
    cbdata: 0,
    count: 0,
    index: NO_INDEX,
    indexed: 0,
    flags: 0,
};

/// Wrapper around `UnsafeCell<[INode; MAX_INODES]>` so we can implement `Sync`.
struct InodeTable(UnsafeCell<[INode; MAX_INODES]>);
unsafe impl Sync for InodeTable {}

/// Wrapper around `UnsafeCell<Option<FsHooks>>` so we can implement `Sync`.
struct HookStorage(UnsafeCell<Option<FsHooks>>);
unsafe impl Sync for HookStorage {}

/// Fixed-size inode table.  Accessed through raw pointer (single-threaded
/// server — no data races).
static INODE_TABLE: InodeTable = InodeTable(UnsafeCell::new([ZERO_INODE; MAX_INODES]));

/// One past the highest id ever handed out (the root is id 0, so this starts at 1).
///
/// It is a high-water mark, not the number in use: a freed slot goes on [`FREE_INODES`]
/// and is handed out again from there, the way C's `unused_inodes` list works.
static INODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Slots `delete_inode` freed, reused before [`INODE_COUNT`] grows.
///
/// Without this the table could only ever fill: a directory tree that creates and removes
/// entries (procfs's per-PID directories) would walk the id space to `MAX_INODES` and stay
/// there. A slot on this list holds no inode data, so nothing can reach it by name, index
/// or id.
struct FreeList(UnsafeCell<([u32; MAX_INODES], usize)>);
unsafe impl Sync for FreeList {}
impl FreeList {
    const fn new() -> Self {
        Self(UnsafeCell::new(([0; MAX_INODES], 0)))
    }
    fn get(&self) -> *mut ([u32; MAX_INODES], usize) {
        self.0.get()
    }
}

static FREE_INODES: FreeList = FreeList::new();

/// Registered hook table.
static HOOKS: HookStorage = HookStorage(UnsafeCell::new(None));

/// Whether the filesystem has been mounted (only REQ_READSUPER is allowed
/// before this, matching the C `fs_mounted`).
static FS_MOUNTED: AtomicBool = AtomicBool::new(false);

fn get_table() -> *mut [INode; MAX_INODES] {
    INODE_TABLE.0.get()
}

fn get_hooks_ptr() -> *mut Option<FsHooks> {
    HOOKS.0.get()
}

/// Return the currently registered hooks, or an empty set before init.
fn hooks() -> FsHooks {
    unsafe { (*get_hooks_ptr()).unwrap_or_else(FsHooks::empty) }
}

// ---------------------------------------------------------------------------
// Errno and FS protocol constants (matching the port's VFS `request.rs` and
// the C `libvtreefs/table.c`).
// ---------------------------------------------------------------------------

pub const OK: i32 = 0;
const EPERM: i32 = -1;
const ENOENT: i32 = -2;
const EIO: i32 = -5;
const E2BIG: i32 = -7;
const ENOMEM: i32 = -12;
const EACCES: i32 = -13;
const EFAULT: i32 = -14;
const EBUSY: i32 = -16;
const ENODEV: i32 = -19;
const ENOTDIR: i32 = -20;
const EINVAL: i32 = -22;
const ENAMETOOLONG: i32 = -36;
/// C `errno.h`: `_SIGN 78`. The previous `-38` was Linux's value.
const ENOSYS: i32 = -78;
const ELOOP: i32 = -40;
const ESYMLINK: i32 = -303;
const ELEAVEMOUNT: i32 = -107;

/// Mode type bits (match `crates/fs/src/mfs/consts.rs`).
const I_TYPE: u16 = 0o170000;
const I_REGULAR: u16 = 0o100000;
const I_DIRECTORY: u16 = 0o040000;
const I_SYMBOLIC_LINK: u16 = 0o120000;

/// VFS ↔ FS request codes (FS_BASE + n).
const FS_BASE: i32 = 0xA00;
const REQ_PUTNODE: i32 = FS_BASE + 2;
const REQ_INHIBREAD: i32 = FS_BASE + 7;
const REQ_STAT: i32 = FS_BASE + 8;
const REQ_STATVFS: i32 = FS_BASE + 10;
const REQ_UNMOUNT: i32 = FS_BASE + 15;
const REQ_SYNC: i32 = FS_BASE + 16;
const REQ_NEW_DRIVER: i32 = FS_BASE + 17;
const REQ_READ: i32 = FS_BASE + 19;
const REQ_LOOKUP: i32 = FS_BASE + 26;
const REQ_READSUPER: i32 = FS_BASE + 28;
const REQ_RDLINK: i32 = FS_BASE + 30;
const REQ_GETDENTS: i32 = FS_BASE + 31;

/// readsuper flags (VFS `request.rs`).
const REQ_ISROOT: u32 = 0o02;
/// lookup flag: return the symlink itself instead of resolving it. Must match
/// the value VFS sets in `l_flags` (VFS `vfs/path.rs` `PATH_RET_SYMLINK`).
const PATH_RET_SYMLINK: u32 = 4;

/// Reply `flags` for readsuper — no capabilities.
const RES_NOFLAGS: u32 = 0;

/// Maximum symlink resolution depth (`_POSIX_SYMLOOP_MAX`).
const SYMLOOP_MAX: usize = 8;

// ---------------------------------------------------------------------------
// Stat layouts — must match `crates/fs/src/mfs/types.rs` (pinned by tests).
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VfsStat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_mtime: i64,
    pub st_ctime: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VfsStatvfs {
    pub f_flags: u64,
    pub f_bsize: u32,
    pub f_frsize: u32,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_favail: u64,
    pub f_fsid: u64,
    pub f_flag: u64,
    pub f_namemax: u64,
}

// ---------------------------------------------------------------------------
// Inode table management
// ---------------------------------------------------------------------------

/// Initialise the inode table and store the hooks. The root inode is created
/// with `root_stat`. The `init_hook` is NOT called here — it fires on mount
/// (`fs_readsuper`), matching the C `vtreefs.c`.
pub fn vtreefs_init(hooks: FsHooks, nr_inodes: u32, root_stat: InodeStat) -> i32 {
    let _ = nr_inodes.min(MAX_INODES as u32);

    INODE_COUNT.store(1, Ordering::Relaxed);
    unsafe { (*FREE_INODES.get()).1 = 0 };
    FS_MOUNTED.store(false, Ordering::Relaxed);
    unsafe {
        (*get_table())[0] = INode {
            id: 0,
            name: {
                let mut n = [0u8; 64];
                n[0] = b'/';
                n
            },
            parent_id: 0,
            first_child: None,
            next_sibling: None,
            stat: root_stat,
            cbdata: 0,
            count: 0,
            index: NO_INDEX,
            indexed: 0,
            flags: 0,
        };
        for slot in (&mut *get_table())[1..].iter_mut() {
            *slot = ZERO_INODE;
        }
        *get_hooks_ptr() = Some(hooks);
    }
    OK
}

/// Allocate a new inode slot, link it under `parent_id`, and return the
/// new inode ID. `u32::MAX` means the table is full.
pub fn add_inode(parent_id: u32, name: &str, index: i32, stat: &InodeStat, cbdata: CbData) -> u32 {
    // A slot a deletion returned is used again before the high-water mark grows.
    let reused = unsafe {
        let (slots, len) = &mut *FREE_INODES.get();
        if *len > 0 {
            *len -= 1;
            Some(slots[*len])
        } else {
            None
        }
    };
    let id = match reused {
        Some(id) => id,
        None => {
            let count = INODE_COUNT.load(Ordering::Relaxed) as usize;
            if count >= MAX_INODES {
                return u32::MAX;
            }
            INODE_COUNT.store(count as u32 + 1, Ordering::Relaxed);
            count as u32
        }
    };

    unsafe {
        let table = &mut *get_table();

        let mut name_buf = [0u8; 64];
        let name_bytes = name.as_bytes();
        let copy_len = name_bytes.len().min(63);
        name_buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        name_buf[copy_len] = 0;

        table[id as usize] = INode {
            id,
            name: name_buf,
            parent_id,
            first_child: None,
            next_sibling: None,
            stat: *stat,
            cbdata,
            count: 0,
            index,
            indexed: 0,
            flags: 0,
        };

        // Link into the parent's child list (insert at head).
        table[id as usize].next_sibling = table[parent_id as usize].first_child;
        table[parent_id as usize].first_child = Some(id);

        // Track indexed entries so getdents can separate them.
        if index >= 0 {
            let want = index as u32 + 1;
            if want > table[parent_id as usize].indexed {
                table[parent_id as usize].indexed = want;
            }
        }
    }

    id
}

/// Return the root inode ID (always 0).
pub fn get_root_inode() -> u32 {
    0
}

/// Increase an inode's reference count.
pub fn ref_inode(id: u32) {
    unsafe {
        (*get_table())[id as usize].count += 1;
    }
}

/// Decrease an inode's reference count by `count`. A deleted inode whose
/// count reaches zero is freed for real, and its slot returns to the free list.
pub fn put_inode(id: u32, count: u32) {
    if id as usize >= MAX_INODES {
        return;
    }
    unsafe {
        let node = &mut (*get_table())[id as usize];
        node.count = node.count.saturating_sub(count);
        if node.count == 0 && node.flags & I_DELETED != 0 {
            free_inode_slot(id);
        }
    }
}

/// Return a deleted, unreferenced inode's slot to the free list.
///
/// # Safety
///
/// The caller must have established that no reference to `id` exists: the slot is cleared
/// and will be handed out again by the next `add_inode`.
unsafe fn free_inode_slot(id: u32) {
    unsafe {
        (*get_table())[id as usize] = ZERO_INODE;
        let (slots, len) = &mut *FREE_INODES.get();
        if *len < MAX_INODES {
            slots[*len] = id;
            *len += 1;
        }
    }
}

/// Unlink an inode (and its descendants) from the tree and mark it deleted.
///
/// The name, index and children are gone immediately, so nothing can find it; the slot is
/// zeroed only once its reference count reaches zero (`put_inode`), because VFS may still
/// be holding the vnode it resolved. C's `delete_inode` does the same with `I_DELETED` and
/// `free_inode`.
pub fn delete_inode(id: u32) -> i32 {
    if id == 0 || id as usize >= MAX_INODES {
        return -EINVAL;
    }

    unsafe {
        // Already unlinked by an earlier call; there is nothing left to do.
        if (*get_table())[id as usize].flags & I_DELETED != 0 {
            return OK;
        }

        let table = &mut *get_table();

        // Recursively delete children first.
        while let Some(child) = table[id as usize].first_child {
            delete_inode(child);
        }

        let parent_id = table[id as usize].parent_id;

        // Remove from the parent's child list.
        if table[parent_id as usize].first_child == Some(id) {
            table[parent_id as usize].first_child = table[id as usize].next_sibling;
        } else {
            let mut cur = table[parent_id as usize].first_child;
            while let Some(sib) = cur {
                if table[sib as usize].next_sibling == Some(id) {
                    table[sib as usize].next_sibling = table[id as usize].next_sibling;
                    break;
                }
                cur = table[sib as usize].next_sibling;
            }
        }

        // Mark it, and drop what would otherwise be reachable through the (now unlinked)
        // id: a deleted inode is found by neither name nor index. The slot itself waits for
        // the last reference.
        table[id as usize].flags |= I_DELETED;
        table[id as usize].first_child = None;
        table[id as usize].next_sibling = None;

        if table[id as usize].count == 0 {
            free_inode_slot(id);
        }
    }

    OK
}

/// Find a child of `parent_id` by name. Returns `None` if not found.
pub fn find_inode(parent_id: u32, name: &str) -> Option<u32> {
    let name_bytes = name.as_bytes();
    unsafe {
        let table = &*get_table();
        let mut cur = table[parent_id as usize].first_child;
        while let Some(id) = cur {
            let inode = &table[id as usize];
            if inode.flags & I_DELETED != 0 {
                cur = inode.next_sibling;
                continue;
            }
            let inode_name = &inode.name;
            let matches = {
                let mut i = 0;
                loop {
                    let a = if i < name_bytes.len() {
                        name_bytes[i]
                    } else {
                        0u8
                    };
                    let b = inode_name[i];
                    if a == 0 && b == 0 {
                        break true;
                    }
                    if a != b || i >= 63 {
                        break false;
                    }
                    i += 1;
                }
            };
            if matches {
                return Some(id);
            }
            cur = inode.next_sibling;
        }
    }
    None
}

/// Return the first child of `parent_id`, or `None`.
pub fn first_inode(parent_id: u32) -> Option<u32> {
    unsafe { (*get_table())[parent_id as usize].first_child }
}

/// Return the next sibling of `id`, or `None`.
pub fn next_sibling(id: u32) -> Option<u32> {
    unsafe { (*get_table())[id as usize].next_sibling }
}

/// Return the child of `parent_id` carrying `index`, or `None` (the C `get_inode_by_index`).
///
/// An indexed child is a slot the directory's owner fills in on demand: a PID directory in
/// `/proc`, for instance, is identified by the process slot, not by its position among the
/// directory's children. Enumeration therefore walks these by index and skips an index with
/// no inode rather than letting the gap shift the entries after it.
pub fn get_inode_by_index(parent_id: u32, index: i32) -> Option<u32> {
    if index < 0 {
        return None;
    }
    unsafe {
        let table = &*get_table();
        let mut cur = table[parent_id as usize].first_child;
        while let Some(id) = cur {
            let inode = &table[id as usize];
            if inode.flags & I_DELETED == 0 && inode.index == index {
                return Some(id);
            }
            cur = inode.next_sibling;
        }
    }
    None
}

/// Get the name of an inode as a `&str`.
pub fn get_inode_name(id: u32) -> &'static str {
    unsafe {
        let inode = &(*get_table())[id as usize];
        let name = &inode.name;
        let len = name.iter().position(|&b| b == 0).unwrap_or(64);
        core::str::from_utf8(&name[..len]).unwrap_or("")
    }
}

/// Read the opaque callback data stored in an inode.
pub fn get_inode_cbdata(id: u32) -> CbData {
    unsafe { (*get_table())[id as usize].cbdata }
}

/// Return a `&'static` reference to the given inode.
pub fn get_inode(id: u32) -> &'static INode {
    unsafe { &(*get_table())[id as usize] }
}

/// Return the metadata of an inode.
pub fn get_inode_stat(id: u32) -> &'static InodeStat {
    unsafe { &(*get_table())[id as usize].stat }
}

/// Return the parent inode ID of an inode.
pub fn get_inode_parent(id: u32) -> u32 {
    unsafe { (*get_table())[id as usize].parent_id }
}

/// Return the index of an inode in its parent directory (`NO_INDEX` if none).
pub fn get_inode_index(id: u32) -> i32 {
    unsafe { (*get_table())[id as usize].index }
}

/// Return whether an inode has been marked deleted.
pub fn is_inode_deleted(id: u32) -> bool {
    unsafe { (*get_table())[id as usize].flags & I_DELETED != 0 }
}

// ---------------------------------------------------------------------------
// Safe-copy helpers (SYS_SAFECOPYTO/FROM kernel calls, same layout as
// `crates/fs/src/block_io.rs`).
// ---------------------------------------------------------------------------

fn safecopy_to(granter: i32, grant_id: i32, src: &[u8]) -> i32 {
    if grant_id < 0 || src.is_empty() {
        return OK;
    }
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&granter.to_ne_bytes());
    kmsg[12..16].copy_from_slice(&grant_id.to_ne_bytes());
    kmsg[16..24].copy_from_slice(&0u64.to_ne_bytes());
    kmsg[24..32].copy_from_slice(&(src.as_ptr() as u64).to_ne_bytes());
    kmsg[32..40].copy_from_slice(&(src.len() as u64).to_ne_bytes());
    minix_rt::kernel_call(32, &mut kmsg) // SYS_SAFECOPYTO
}

fn safecopy_from(granter: i32, grant_id: i32, dst: &mut [u8]) -> i32 {
    if grant_id < 0 {
        return OK;
    }
    let mut kmsg = [0u8; 64];
    kmsg[8..12].copy_from_slice(&granter.to_ne_bytes());
    kmsg[12..16].copy_from_slice(&grant_id.to_ne_bytes());
    kmsg[16..24].copy_from_slice(&0u64.to_ne_bytes());
    kmsg[24..32].copy_from_slice(&(dst.as_mut_ptr() as u64).to_ne_bytes());
    kmsg[32..40].copy_from_slice(&(dst.len() as u64).to_ne_bytes());
    minix_rt::kernel_call(31, &mut kmsg) // SYS_SAFECOPYFROM
}

/// Copy a lookup's path out of the grant VFS attached to the request.
///
/// A minix target reads VFS's own buffer through the kernel. A host build has
/// no kernel to read it with — calling one there would execute a real syscall
/// instruction — so the test that drives this handler supplies the bytes the
/// grant would have pointed at.
fn fetch_path(grant_id: i32, dst: &mut [u8]) -> i32 {
    #[cfg(target_os = "minix")]
    {
        safecopy_from(arch_common::com::VFS_PROC_NR, grant_id, dst)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = grant_id;
        host_path::copy_into(dst);
        OK
    }
}

/// Copy an expanded lookup path back into VFS's grant.
///
/// A symlink whose target starts with `/` is answered with `ESYMLINK`: VFS
/// restarts the walk from the process root at the offset it is told, so the
/// expanded path has to reach its buffer, not stay in this server.
fn store_path(grant_id: i32, src: &[u8]) -> i32 {
    #[cfg(target_os = "minix")]
    {
        safecopy_to(arch_common::com::VFS_PROC_NR, grant_id, src)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = grant_id;
        host_path::store_from(src);
        OK
    }
}

/// Host-side stand-in for the bytes of a lookup grant.
#[cfg(not(target_os = "minix"))]
mod host_path {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicUsize, Ordering};

    struct Store(UnsafeCell<[u8; super::PATH_MAX]>, AtomicUsize);
    // Safety: MINIX servers are single-threaded, and so is the test using this.
    unsafe impl Sync for Store {}

    static STORE: Store = Store(UnsafeCell::new([0u8; super::PATH_MAX]), AtomicUsize::new(0));

    pub fn set(path: &[u8]) {
        let n = path.len().min(super::PATH_MAX);
        // Safety: single-threaded, as above.
        let store = unsafe { &mut *STORE.0.get() };
        store[..n].copy_from_slice(&path[..n]);
        STORE.1.store(n, Ordering::Relaxed);
    }

    pub fn copy_into(dst: &mut [u8]) {
        let n = STORE.1.load(Ordering::Relaxed).min(dst.len());
        // Safety: single-threaded, as above.
        let store = unsafe { &*STORE.0.get() };
        dst[..n].copy_from_slice(&store[..n]);
    }

    /// Record the path the handler wrote back, so a test can read the expansion.
    pub fn store_from(src: &[u8]) {
        let n = src.len().min(super::PATH_MAX);
        // Safety: single-threaded, as above.
        let store = unsafe { &mut *STORE.0.get() };
        store[..n].copy_from_slice(&src[..n]);
        STORE.1.store(n, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// FS protocol handlers
// ---------------------------------------------------------------------------

/// `m_fs_vfs_readsuper`: the root inode's properties, plus mount state.
fn fs_readsuper(msg: &mut Message) -> i32 {
    let flags = u32::from_ne_bytes(raw_of(msg)[4..8].try_into().unwrap_or([0u8; 4]));
    // VTreeFS must not be mounted as the root filesystem.
    if flags & REQ_ISROOT != 0 {
        return EINVAL;
    }

    let root = get_root_inode();
    ref_inode(root);

    // The system is now mounted — call the initialization hook.
    if let Some(init) = hooks().init_hook {
        init();
    }

    let stat = *get_inode_stat(root);
    let raw = raw_of_mut(msg);
    raw[0..8].copy_from_slice(&(stat.size as i64).to_le_bytes()); // file_size
    raw[8..12].copy_from_slice(&(stat.dev as u32).to_le_bytes()); // device
    raw[12..16].copy_from_slice(&root.to_le_bytes()); // inode
    raw[16..20].copy_from_slice(&RES_NOFLAGS.to_le_bytes()); // flags
    raw[20..22].copy_from_slice(&(stat.mode as u16).to_le_bytes()); // mode

    FS_MOUNTED.store(true, Ordering::Relaxed);
    OK
}

fn fs_unmount(msg: &mut Message) -> i32 {
    let _ = msg;
    let root = get_root_inode();
    put_inode(root, 1);

    if let Some(cleanup) = hooks().cleanup_hook {
        cleanup();
    }

    FS_MOUNTED.store(false, Ordering::Relaxed);
    OK
}

/// `m_vfs_fs_putnode`: count (u64) at payload[0], inode (u32) at payload[8].
fn fs_putnode(msg: &mut Message) -> i32 {
    let raw = raw_of(msg);
    let count = u64::from_ne_bytes(raw[0..8].try_into().unwrap_or([0u8; 8])) as u32;
    let ino = u32::from_ne_bytes(raw[8..12].try_into().unwrap_or([0u8; 4]));

    if get_inode(ino).flags & I_DELETED != 0 && get_inode(ino).count == 0 {
        return EINVAL;
    }
    put_inode(ino, count);
    OK
}

/// Resolve a symlink's target via the rdlink hook into `out`.
/// Returns the target length (excluding the NUL), or an error.
fn resolve_link(node: u32, caller: i32, out: &mut [u8]) -> Result<usize, i32> {
    let hook = match hooks().rdlink_hook {
        Some(h) => h,
        None => return Err(EINVAL),
    };
    // The hook's own code is the answer: it is the only place that knows why a
    // target is missing (`ENOENT` for a link with no recorded path), and folding
    // every failure into `EINVAL` would hide that from the caller.
    let r = hook(node, caller, out);
    if r != OK {
        return Err(r);
    }
    let len = out.iter().position(|&b| b == 0).unwrap_or(out.len());
    if len == 0 || len >= out.len() {
        return Err(ENAMETOOLONG);
    }
    Ok(len)
}

/// `m_vfs_fs_lookup`: dir_ino (u32) at payload[0], root_ino (u32) at
/// payload[4], uid (u16) at payload[8], gid (u16) at payload[10], flags (u32)
/// at payload[12], grant_ucred (i32) at payload[16], path_len (u32) at
/// payload[20] (path bytes, without the NUL), grant_path (i32) at payload[24]
/// — a read-only grant over VFS's own path buffer — and the caller's endpoint
/// (i32) at payload[28], for a name whose meaning depends on who asks.
///
/// Reply (mess_fs_vfs_lookup): file_size (i64) at payload[8], device (u32)
/// at payload[16], inode (u32) at payload[20], mode (u32) at payload[24].
fn fs_lookup(msg: &mut Message) -> i32 {
    let raw = raw_of(msg);
    let dir_ino = u32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0u8; 4]));
    let flags = u32::from_ne_bytes(raw[12..16].try_into().unwrap_or([0u8; 4]));
    let mut path_len = u32::from_ne_bytes(raw[20..24].try_into().unwrap_or([0u8; 4])) as usize;
    let grant_path = i32::from_ne_bytes(raw[24..28].try_into().unwrap_or([0u8; 4]));
    let caller = i32::from_ne_bytes(raw[28..32].try_into().unwrap_or([0u8; 4]));

    if path_len == 0 {
        return EINVAL;
    }
    // The path has to fit together with its NUL terminator. VFS's own PATH_MAX
    // is smaller than this server's, so a path that fits here is resolvable.
    if path_len >= PATH_MAX {
        return ENAMETOOLONG;
    }

    // The path stays in VFS's own buffer, reachable only through the grant;
    // copy it out into a local buffer and terminate it for the walk below.
    let mut path = [0u8; PATH_MAX];
    let r = fetch_path(grant_path, &mut path[..path_len]);
    if r != OK {
        return r;
    }
    path[path_len] = 0;

    if dir_ino as usize >= MAX_INODES {
        return EINVAL;
    }
    let mut cur = dir_ino;
    ref_inode(cur);

    let mut r = OK;
    let mut symloops = 0usize;
    let mut consumed = 0usize;

    loop {
        // Skip leading slashes.
        while consumed < path_len && path[consumed] == b'/' {
            consumed += 1;
        }
        if consumed >= path_len || path[consumed] == 0 {
            break;
        }

        let start = consumed;
        while consumed < path_len && path[consumed] != b'/' && path[consumed] != 0 {
            consumed += 1;
        }
        let name = &path[start..consumed];

        if name == b"." {
            continue;
        }
        if name == b".." {
            if cur == get_root_inode() {
                r = ELEAVEMOUNT;
                break;
            }
            let parent = get_inode_parent(cur);
            ref_inode(parent);
            put_inode(cur, 1);
            cur = parent;
            continue;
        }

        let name_str = core::str::from_utf8(name).unwrap_or("");
        if let Some(hook) = hooks().lookup_hook
            && hook(cur, name_str) != OK
        {
            r = ENOENT;
            break;
        }

        let next = match find_inode(cur, name_str) {
            Some(id) => id,
            None => {
                r = ENOENT;
                break;
            }
        };

        // Resolve symlinks unless the final component is requested as-is.
        let is_last = consumed >= path_len || path[consumed] == 0;
        let mode = get_inode_stat(next).mode;
        if (mode & I_TYPE as u32) as u16 == I_SYMBOLIC_LINK
            && (!is_last || flags & PATH_RET_SYMLINK == 0)
        {
            symloops += 1;
            if symloops > SYMLOOP_MAX {
                r = ELOOP;
                break;
            }

            let mut target = [0u8; PATH_MAX];
            let tlen = match resolve_link(next, caller, &mut target) {
                Ok(n) => n,
                Err(e) => {
                    r = e;
                    break;
                }
            };
            if target[0] == b'/' {
                // Absolute link: VFS restarts the walk from the process root. The
                // expanded path goes back through the grant with the target
                // replacing this one component, and the reply names the offset
                // VFS resumes at (the component's start) and how many links
                // were followed.
                let tail = &path[consumed..];
                let ttail = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
                if start + tlen + ttail + 1 > PATH_MAX {
                    r = ENAMETOOLONG;
                    break;
                }
                let mut expanded = [0u8; PATH_MAX];
                expanded[..start].copy_from_slice(&path[..start]);
                expanded[start..start + tlen].copy_from_slice(&target[..tlen]);
                let tail_at = start + tlen;
                expanded[tail_at..tail_at + ttail].copy_from_slice(&tail[..ttail]);
                expanded[tail_at + ttail] = 0;

                r = store_path(grant_path, &expanded[..tail_at + ttail + 1]);
                if r != OK {
                    break;
                }
                let raw = raw_of_mut(msg);
                raw[0..8].copy_from_slice(&(start as i64).to_le_bytes());
                raw[32..34].copy_from_slice(&(symloops as u16).to_le_bytes());
                r = ESYMLINK;
                break;
            }

            // Relative target: replace the remaining path with target + tail.
            let mut tail_buf = [0u8; PATH_MAX];
            let tail = &path[consumed..];
            let ttail = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
            tail_buf[..ttail].copy_from_slice(&tail[..ttail]);
            if tlen + ttail + 1 > PATH_MAX {
                r = ENAMETOOLONG;
                break;
            }
            path[..tlen].copy_from_slice(&target[..tlen]);
            path[tlen..tlen + ttail].copy_from_slice(&tail_buf[..ttail]);
            path[tlen + ttail] = 0;
            path_len = tlen + ttail + 1;
            consumed = 0;
            continue; // re-walk from `cur` with the resolved path
        }

        ref_inode(next);
        put_inode(cur, 1);
        cur = next;
    }

    if r != OK {
        put_inode(cur, 1);
        return r;
    }

    // On success, leave the resulting file open (VFS will putnode it).
    let stat = *get_inode_stat(cur);
    let raw = raw_of_mut(msg);
    raw[8..16].copy_from_slice(&(stat.size as i64).to_le_bytes()); // file_size
    raw[16..20].copy_from_slice(&(stat.dev as u32).to_le_bytes()); // device
    raw[20..24].copy_from_slice(&cur.to_le_bytes()); // inode
    raw[24..28].copy_from_slice(&stat.mode.to_le_bytes()); // mode
    OK
}

/// `m_vfs_fs_readwrite` (read): inode (u32) at payload[0], seek_pos (i64)
/// at payload[8], grant (i32) at payload[16], nbytes (u64) at payload[24].
///
/// Reply: seek_pos (i64) at payload[0], nbytes (u32) at payload[8].
fn fs_read(msg: &mut Message) -> i32 {
    let raw = raw_of(msg);
    let ino = u32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0u8; 4]));
    let pos = i64::from_ne_bytes(raw[8..16].try_into().unwrap_or([0u8; 8]));
    let grant = i32::from_ne_bytes(raw[16..20].try_into().unwrap_or([0u8; 4]));
    let nbytes = u64::from_ne_bytes(raw[24..32].try_into().unwrap_or([0u8; 8])) as usize;

    let node = if ino as usize >= MAX_INODES {
        return EINVAL;
    } else {
        ino
    };
    if (get_inode_stat(node).mode & I_TYPE as u32) as u16 != I_REGULAR {
        return EINVAL;
    }

    // Read through the hook into a local buffer, then copy out via the grant.
    let mut buf = [0u8; 4096];
    let want = nbytes.min(buf.len());
    let got = match hooks().read_hook {
        Some(hook) => hook(node, pos.max(0) as u64, &mut buf[..want]),
        None => 0,
    };
    let got = got.min(want);

    if got > 0 {
        let r = safecopy_to(msg.m_source, grant, &buf[..got]);
        if r != OK {
            return r;
        }
    }

    let raw = raw_of_mut(msg);
    raw[0..8].copy_from_slice(&(pos + got as i64).to_le_bytes()); // seek_pos
    raw[8..12].copy_from_slice(&(got as u32).to_le_bytes()); // nbytes
    // The reply m_type carries the byte count (matching MFS's
    // `fs_readwrite`, which returns cum_io): VFS's `do_read` reports the
    // reply m_type to the caller as the read() result.
    got as i32
}

/// Convert a mode to a `d_type` value (`DT_*`).
fn fs_mode_to_type(mode: u32) -> u8 {
    match (mode & I_TYPE as u32) as u16 {
        I_DIRECTORY => 4,      // DT_DIR
        I_REGULAR => 8,        // DT_REG
        I_SYMBOLIC_LINK => 10, // DT_LNK
        _ => 0,                // DT_UNKNOWN
    }
}

/// Return the `skip`-th (0-based) non-deleted, non-indexed child of `node`.
fn nth_child(node: u32, mut skip: i64) -> Option<u32> {
    let mut cur = first_inode(node);
    while let Some(c) = cur {
        if is_inode_deleted(c) || get_inode_index(c) != NO_INDEX {
            cur = next_sibling(c);
            continue;
        }
        if skip <= 0 {
            return Some(c);
        }
        skip -= 1;
        cur = next_sibling(c);
    }
    None
}

/// Build the `dirent` records for `ino` starting at position `pos`.
///
/// Returns `(next_pos, out_len, status)`. A full `out` is not an error: `next_pos` names the
/// entry the next call resumes at. `EINVAL` is returned only when not even one record fits.
///
/// Positions 0 and 1 are `.` and `..`; the next `indexed` positions are the indexed children,
/// taken in index order and skipping an index with no inode; every position after that is one
/// non-indexed child. Splitting the two kinds is the C `fs_getdents` rule. Treating the indexed
/// range as non-indexed (a negative skip into `nth_child`) makes the first non-indexed child
/// repeat once per indexed slot and hides the indexed ones entirely.
fn build_dirents(ino: u32, pos: i64, out: &mut [u8], mem_size: usize) -> (i64, usize, i32) {
    let indexed = get_inode(ino).indexed as i64;
    let mut out_len = 0usize;
    let mut entry_pos = pos.max(0);
    let mut r = OK;

    loop {
        let (child, name): (u32, &[u8]) = if entry_pos == 0 {
            (ino, b".")
        } else if entry_pos == 1 {
            (get_inode_parent(ino), b"..")
        } else if entry_pos - 2 < indexed {
            let Some(c) = get_inode_by_index(ino, (entry_pos - 2) as i32) else {
                // No inode for this index: skip the number, not the rest of the directory.
                entry_pos += 1;
                continue;
            };
            (c, get_inode_name(c).as_bytes())
        } else {
            match nth_child(ino, entry_pos - 2 - indexed) {
                Some(c) => (c, get_inode_name(c).as_bytes()),
                None => break,
            }
        };

        let name_len = name.len().min(PNAME_MAX);
        // dirent record: d_fileno(8) + d_reclen(2) + d_namlen(2) +
        // d_type(1) + d_name(namlen) + NUL, padded to 4 bytes.
        let raw_size = 13 + name_len + 1;
        let reclen = (raw_size + 3) & !3;

        if out_len + reclen > out.len() || out_len + reclen > mem_size {
            if out_len == 0 {
                r = EINVAL; // user buffer too small for even one record
            }
            break;
        }

        out[out_len..out_len + 8].copy_from_slice(&(child as u64).to_le_bytes()); // d_fileno
        out[out_len + 8..out_len + 10].copy_from_slice(&(reclen as u16).to_le_bytes()); // d_reclen
        out[out_len + 10..out_len + 12].copy_from_slice(&(name_len as u16).to_le_bytes()); // d_namlen
        out[out_len + 12] = fs_mode_to_type(get_inode_stat(child).mode); // d_type
        out[out_len + 13..out_len + 13 + name_len].copy_from_slice(&name[..name_len]);
        out_len += reclen;
        entry_pos += 1;
    }

    (entry_pos, out_len, r)
}

/// `m_vfs_fs_getdents`: inode (u32) at payload[0], seek_pos (i64) at
/// payload[8], grant (i32) at payload[16], mem_size (u64) at payload[24].
///
/// Reply: seek_pos (i64) at payload[0], nbytes (i32) at payload[8].
fn fs_getdents(msg: &mut Message) -> i32 {
    let raw = raw_of(msg);
    let ino = u32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0u8; 4]));
    let pos = i64::from_ne_bytes(raw[8..16].try_into().unwrap_or([0u8; 8]));
    let grant = i32::from_ne_bytes(raw[16..20].try_into().unwrap_or([0u8; 4]));
    let mem_size = u64::from_ne_bytes(raw[24..32].try_into().unwrap_or([0u8; 8])) as usize;

    if ino as usize >= MAX_INODES {
        return EINVAL;
    }
    if (get_inode_stat(ino).mode & I_TYPE as u32) as u16 != I_DIRECTORY {
        return EINVAL;
    }

    // Refresh the directory before enumerating; the hook is what gives it its indexed entries.
    if let Some(hook) = hooks().getdents_hook
        && hook(ino) != OK
    {
        return EINVAL;
    }

    let mut out = [0u8; 4096];
    let (entry_pos, out_len, r) = build_dirents(ino, pos, &mut out, mem_size);

    if out_len > 0 {
        let cr = safecopy_to(msg.m_source, grant, &out[..out_len]);
        if cr != OK {
            return cr;
        }
    }

    let raw = raw_of_mut(msg);
    raw[0..8].copy_from_slice(&entry_pos.to_le_bytes()); // seek_pos
    raw[8..12].copy_from_slice(&(out_len as i32).to_le_bytes()); // nbytes
    r
}

/// `m_vfs_fs_rdlink`: inode (u32) at payload[0], grant (i32) at payload[8],
/// mem_size (u64) at payload[16].
///
/// Reply: nbytes (u64) at payload[0].
fn fs_rdlink(msg: &mut Message) -> i32 {
    let raw = raw_of(msg);
    let ino = u32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0u8; 4]));
    let grant = i32::from_ne_bytes(raw[8..12].try_into().unwrap_or([0u8; 4]));
    let mem_size = u64::from_ne_bytes(raw[16..24].try_into().unwrap_or([0u8; 8])) as usize;
    let caller = i32::from_ne_bytes(raw[24..28].try_into().unwrap_or([0u8; 4]));

    if ino as usize >= MAX_INODES {
        return EINVAL;
    }

    let mut path = [0u8; PATH_MAX];
    let len = match resolve_link(ino, caller, &mut path) {
        Ok(n) => n,
        Err(e) => return e,
    };
    let copy = len.min(mem_size);

    let r = safecopy_to(msg.m_source, grant, &path[..copy]);
    if r != OK {
        return r;
    }

    let raw = raw_of_mut(msg);
    raw[0..8].copy_from_slice(&(copy as u64).to_le_bytes()); // nbytes
    OK
}

/// `m_vfs_fs_stat`: inode (u32) at payload[0], grant (i32) at payload[8].
/// The `VfsStat` is copied out through the grant.
fn fs_stat(msg: &mut Message) -> i32 {
    let raw = raw_of(msg);
    let ino = u32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0u8; 4]));
    let grant = i32::from_ne_bytes(raw[8..12].try_into().unwrap_or([0u8; 4]));

    if ino as usize >= MAX_INODES {
        return EINVAL;
    }

    let mut st = VfsStat::default();
    let stat = *get_inode_stat(ino);
    st.st_dev = stat.dev;
    st.st_ino = ino as u64;
    st.st_mode = stat.mode;
    st.st_nlink = if is_inode_deleted(ino) { 0 } else { 1 };
    st.st_uid = stat.uid;
    st.st_gid = stat.gid;
    st.st_rdev = stat.dev;
    st.st_size = stat.size as i64;
    st.st_blksize = 4096;

    let st_bytes = unsafe {
        core::slice::from_raw_parts(
            &st as *const VfsStat as *const u8,
            core::mem::size_of::<VfsStat>(),
        )
    };
    safecopy_to(msg.m_source, grant, st_bytes)
}

/// `m_vfs_fs_statvfs`: grant (i32) at payload[0]. A minimal `VfsStatvfs` is
/// copied out through the grant.
fn fs_statvfs(msg: &mut Message) -> i32 {
    let raw = raw_of(msg);
    let grant = i32::from_ne_bytes(raw[0..4].try_into().unwrap_or([0u8; 4]));

    let st = VfsStatvfs {
        f_bsize: 4096,
        f_frsize: 4096,
        f_namemax: PNAME_MAX as u64,
        ..Default::default()
    };

    let st_bytes = unsafe {
        core::slice::from_raw_parts(
            &st as *const VfsStatvfs as *const u8,
            core::mem::size_of::<VfsStatvfs>(),
        )
    };
    safecopy_to(msg.m_source, grant, st_bytes)
}

/// Dispatch an FS request (already known to be `FS_BASE`-relative). `msg`
/// carries the request; on success or a handled error the handler fills the
/// reply payload in place.
fn dispatch(idx: usize, msg: &mut Message) -> i32 {
    match idx {
        2 => fs_putnode(msg),
        7 => OK, // inhibread — no read-ahead to cancel
        8 => fs_stat(msg),
        10 => fs_statvfs(msg),
        15 => fs_unmount(msg),
        16 => OK, // sync — nothing to flush
        17 => OK, // new_driver — no block devices
        19 => fs_read(msg),
        26 => fs_lookup(msg),
        28 => fs_readsuper(msg),
        30 => fs_rdlink(msg),
        31 => fs_getdents(msg),
        // Unrecognized request: let the message hook try (C `no_sys`).
        _ => match hooks().message_hook {
            Some(hook) => hook(message_as_bytes(msg)),
            None => ENOSYS,
        },
    }
}

/// Handle one received message: non-VFS messages go to the `message_hook`
/// (e.g. devman), VFS messages are dispatched through the FS table.
///
/// Returns the status code to send back as `m_type` for VFS messages. For
/// message-hook messages the hook itself sets the reply `m_type`.
pub fn handle_fs_message(msg: &mut Message) -> i32 {
    let from_vfs = msg.m_source == arch_common::com::VFS_PROC_NR;
    if !from_vfs {
        return match hooks().message_hook {
            Some(hook) => hook(message_as_bytes(msg)),
            None => ENOSYS,
        };
    }

    let call_nr = msg.m_type;
    if FS_MOUNTED.load(Ordering::Relaxed) || call_nr == REQ_READSUPER {
        let idx = (call_nr - FS_BASE) as usize;
        dispatch(idx, msg)
    } else {
        EINVAL
    }
}

/// Borrow a `Message` as a raw 64-byte buffer (the `message_hook` ABI).
fn message_as_bytes(msg: &mut Message) -> &mut [u8; 64] {
    // The port's `Message` is 64 bytes (m_source + m_type + 56-byte payload).
    const _: () = assert!(core::mem::size_of::<Message>() == 64);
    unsafe { &mut *(msg as *mut Message as *mut [u8; 64]) }
}

/// Borrow the message payload as raw bytes.
fn raw_of(msg: &Message) -> &[u8; 56] {
    unsafe { &msg.m_payload.raw }
}

/// Borrow the message payload as mutable raw bytes.
fn raw_of_mut(msg: &mut Message) -> &mut [u8; 56] {
    unsafe { &mut msg.m_payload.raw }
}

/// Enter the VTreeFS receive-dispatch-reply loop. Never returns.
///
/// Mirrors the C `start_vtreefs()`: `vtreefs_init` first, then receive →
/// dispatch → SEND. Non-VFS messages are handled by the message hook (the
/// hook sets the reply in the message; we send it back).
pub fn start_vtreefs(hooks: FsHooks, nr_inodes: u32, root_stat: InodeStat, nr_indexed: u32) -> ! {
    let _ = nr_indexed;
    vtreefs_init(hooks, nr_inodes, root_stat);

    #[cfg(target_os = "minix")]
    {
        const RECEIVE_CALL: u64 = 47;
        const SEND_CALL: u64 = 46;
        const ANY: i32 = 0x0000ffff;
        let has_hook = hooks.message_hook.is_some();

        loop {
            let mut msg = Message {
                m_source: 0,
                m_type: 0,
                m_payload: unsafe { core::mem::zeroed() },
            };
            let src = unsafe {
                minix_rt::syscall2(RECEIVE_CALL, ANY as u64, &mut msg as *mut Message as u64)
            };
            if src < 0 {
                continue;
            }

            // RS asks each service it starts to initialise, and this loop is where a
            // VTreeFS server's messages arrive, so the answer belongs here rather than
            // in a server's message hook — otherwise every VTreeFS server that RS ever
            // asks would have to remember to handle it, and devman is the one that
            // does. The shape (m_type `RS_INIT`, result in `m2i1`, sent to RS) is
            // shared through `answer_rs_init`, because RS's init loop blocks for it and
            // panics on anything else.
            if minix_util::rs::answer_rs_init(msg.m_type, msg.m_source) {
                continue;
            }

            let sender = msg.m_source;
            let from_vfs = sender == arch_common::com::VFS_PROC_NR;

            let status = handle_fs_message(&mut msg);
            if from_vfs {
                msg.m_type = status;
            }

            // Reply to VFS always; to other senders only if a message hook
            // produced a reply (devman sets DEVMAN_REPLY).
            if from_vfs || has_hook {
                let _ = unsafe {
                    minix_rt::syscall2(SEND_CALL, sender as u64, &mut msg as *mut Message as u64)
                };
            }
        }
    }
    #[cfg(not(target_os = "minix"))]
    {
        loop {
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned by value, not by name: C compares an errno numerically, and Linux's
    /// `ENOSYS` is 38 — which is the value this constant had.
    #[test]
    fn test_enosys_matches_c() {
        assert_eq!(ENOSYS, -78);
    }

    /// VFS decides the value of this flag (`vfs/path.rs`); the server only
    /// tests it against `l_flags`. Keep the two in step.
    #[test]
    fn test_path_ret_symlink_matches_vfs() {
        assert_eq!(PATH_RET_SYMLINK, 4);
    }

    /// A link hook's own error reaches the caller.
    ///
    /// The hook is the one component that knows why a target is missing (procfs
    /// answers `ENOENT` for a link whose process has no recorded path), so
    /// `resolve_link` must not rewrite every failure into `EINVAL`.
    #[test]
    fn a_link_hooks_error_is_not_rewritten_to_einval() {
        fn link(_node: u32, caller: i32, out: &mut [u8]) -> i32 {
            assert_eq!(caller, 0x1002B, "the caller reaches the hook");
            assert_eq!(out.len(), PATH_MAX, "the hook gets the full target buffer");
            ENOENT
        }
        vtreefs_init(
            FsHooks {
                rdlink_hook: Some(link),
                ..FsHooks::empty()
            },
            64,
            test_root_stat(),
        );
        let linkstat = InodeStat {
            mode: I_SYMBOLIC_LINK as u32 | 0o777,
            ..test_root_stat()
        };
        let id = add_inode(0, "self", NO_INDEX, &linkstat, 0);

        // Only `REQ_READSUPER` is served before mounting.
        let mut mnt = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        assert_eq!(handle_fs_message(&mut mnt), OK);

        let mut msg = new_message(REQ_RDLINK, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, id);
            w_i32(raw, 8, -1); // grant: no copy on host
            w_i64(raw, 16, PATH_MAX as i64); // mem_size
            w_i32(raw, 24, 0x1002B); // caller endpoint
        }
        assert_eq!(handle_fs_message(&mut msg), ENOENT);
    }

    /// An absolute link target goes back through the grant with the rest of the
    /// path behind it, and the reply names where VFS resumes.
    ///
    /// `/proc/self/exe` is the case: `self` is mid-path, its target is
    /// `/proc/<pid>`, and VFS can only continue if the filesystem hands back
    /// both the expanded path and the offset to restart at.
    #[test]
    fn an_absolute_link_expands_into_the_grant_for_vfs_to_restart() {
        fn link(_node: u32, _caller: i32, out: &mut [u8]) -> i32 {
            let target = b"/proc/43";
            out[..target.len()].copy_from_slice(target);
            out[target.len()] = 0;
            OK
        }
        vtreefs_init(
            FsHooks {
                rdlink_hook: Some(link),
                ..FsHooks::empty()
            },
            64,
            test_root_stat(),
        );
        let linkstat = InodeStat {
            mode: I_SYMBOLIC_LINK as u32 | 0o777,
            ..test_root_stat()
        };
        add_inode(0, "self", NO_INDEX, &linkstat, 0);

        let mut mnt = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        assert_eq!(handle_fs_message(&mut mnt), OK);

        let mut msg = new_message(REQ_LOOKUP, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 0); // dir_ino = root
            w_u32(raw, 4, 0); // root_ino
            w_u32(raw, 12, 0); // flags: mid-path, so the link is followed
            w_u32(raw, 20, 8); // "self/exe"
            w_i32(raw, 24, -1); // grant; the host store stands in
            host_path::set(b"self/exe");
        }
        assert_eq!(handle_fs_message(&mut msg), ESYMLINK);

        let raw = raw_of(&msg);
        assert_eq!(r_i64(raw, 0), 0, "VFS resumes at the link component");
        assert_eq!(
            u16::from_le_bytes(raw[32..34].try_into().unwrap()),
            1,
            "one link was followed"
        );
        let mut expanded = [0u8; PATH_MAX];
        host_path::copy_into(&mut expanded);
        let end = expanded
            .iter()
            .position(|&b| b == 0)
            .expect("the expansion is NUL-terminated");
        assert_eq!(&expanded[..end], b"/proc/43/exe");
    }

    /// A link that is the last component is returned unresolved when VFS asks
    /// for it (`PATH_RET_SYMLINK`), which is what makes `readlink` read the
    /// link instead of following it.
    #[test]
    fn a_last_component_link_is_returned_unresolved_when_asked() {
        fn link(_node: u32, _caller: i32, out: &mut [u8]) -> i32 {
            let target = b"/proc/43";
            out[..target.len()].copy_from_slice(target);
            out[target.len()] = 0;
            OK
        }
        vtreefs_init(
            FsHooks {
                rdlink_hook: Some(link),
                ..FsHooks::empty()
            },
            64,
            test_root_stat(),
        );
        let linkstat = InodeStat {
            mode: I_SYMBOLIC_LINK as u32 | 0o777,
            ..test_root_stat()
        };
        let id = add_inode(0, "self", NO_INDEX, &linkstat, 0);

        let mut mnt = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        assert_eq!(handle_fs_message(&mut mnt), OK);

        let mut msg = new_message(REQ_LOOKUP, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 0);
            w_u32(raw, 4, 0);
            w_u32(raw, 12, PATH_RET_SYMLINK);
            w_u32(raw, 20, 4); // "self"
            w_i32(raw, 24, -1);
            host_path::set(b"self");
        }
        assert_eq!(handle_fs_message(&mut msg), OK);
        let raw = raw_of(&msg);
        assert_eq!(r_u32(raw, 20), id, "the link's own inode comes back");
        assert_eq!(r_u32(raw, 24), linkstat.mode, "with the link's mode");
    }

    fn test_root_stat() -> InodeStat {
        InodeStat {
            mode: I_DIRECTORY as u32 | 0o555,
            uid: 0,
            gid: 0,
            size: 0,
            dev: 0,
        }
    }

    fn null_hooks() -> FsHooks {
        FsHooks::empty()
    }

    fn new_message(m_type: i32, source: i32) -> Message {
        Message {
            m_source: source,
            m_type,
            m_payload: unsafe { core::mem::zeroed() },
        }
    }

    fn w_u32(raw: &mut [u8], off: usize, val: u32) {
        raw[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn w_i32(raw: &mut [u8], off: usize, val: i32) {
        raw[off..off + 4].copy_from_slice(&val.to_le_bytes());
    }

    fn w_i64(raw: &mut [u8], off: usize, val: i64) {
        raw[off..off + 8].copy_from_slice(&val.to_le_bytes());
    }

    fn r_u32(raw: &[u8], off: usize) -> u32 {
        u32::from_le_bytes(raw[off..off + 4].try_into().unwrap())
    }

    fn r_i64(raw: &[u8], off: usize) -> i64 {
        i64::from_le_bytes(raw[off..off + 8].try_into().unwrap())
    }

    /// The names in a packed `dirent` buffer, in the order `build_dirents` wrote them.
    fn dirent_names<'a>(buf: &'a [u8], names: &mut [&'a str]) -> usize {
        let mut n = 0usize;
        let mut off = 0usize;
        while off + 13 <= buf.len() && n < names.len() {
            let reclen = u16::from_le_bytes(buf[off + 8..off + 10].try_into().unwrap()) as usize;
            let namlen = u16::from_le_bytes(buf[off + 10..off + 12].try_into().unwrap()) as usize;
            if reclen == 0 || off + reclen > buf.len() {
                break;
            }
            names[n] = core::str::from_utf8(&buf[off + 13..off + 13 + namlen]).unwrap();
            n += 1;
            off += reclen;
        }
        n
    }

    /// `delete_inode` unlinks at once, but a slot VFS still holds must not be handed to a
    /// new inode: the name is gone and the inode is flagged, and only the last `put_inode`
    /// returns the slot to the free list.
    #[test]
    fn deletion_unlinks_at_once_and_frees_after_the_last_reference() {
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let file = InodeStat {
            mode: I_REGULAR as u32 | 0o444,
            ..test_root_stat()
        };
        let id = add_inode(0, "doomed", NO_INDEX, &file, 0);
        ref_inode(id); // VFS resolved it and has not put it back yet

        assert_eq!(delete_inode(id), OK);
        assert!(find_inode(0, "doomed").is_none(), "unfindable while held");
        assert_eq!(nth_child(0, 0), None, "unenumerable while held");
        assert!(get_inode(id).flags & I_DELETED != 0);

        // A new inode cannot get the held slot.
        let other = add_inode(0, "other", NO_INDEX, &file, 0);
        assert_ne!(other, id);

        // The holder releases it, and the slot is reused.
        put_inode(id, 1);
        let again = add_inode(0, "again", NO_INDEX, &file, 0);
        assert_eq!(again, id, "the freed slot is handed out again");
        assert!(find_inode(0, "again").is_some());
    }

    /// An unreferenced inode is freed by `delete_inode` itself.
    #[test]
    fn an_unreferenced_deletion_frees_its_slot_at_once() {
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let file = InodeStat {
            mode: I_REGULAR as u32 | 0o444,
            ..test_root_stat()
        };
        let id = add_inode(0, "gone", NO_INDEX, &file, 0);
        assert_eq!(delete_inode(id), OK);
        let next = add_inode(0, "next", NO_INDEX, &file, 0);
        assert_eq!(next, id);
    }

    /// A directory's indexed children are enumerated by index, once each, with an empty index
    /// skipped. The previous code had no branch for them: the negative skip it passed to
    /// `nth_child` made the first ordinary child repeat once per indexed slot, which is what
    /// `ls /proc` printed as a row of `uptime` entries.
    #[test]
    fn getdents_names_indexed_children_once_each() {
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let file = InodeStat {
            mode: I_REGULAR as u32 | 0o444,
            ..test_root_stat()
        };
        add_inode(0, "static-a", NO_INDEX, &file, 0);
        add_inode(0, "static-b", NO_INDEX, &file, 0);
        add_inode(0, "pid-0", 0, &file, 0);
        add_inode(0, "pid-2", 2, &file, 0); // index 1 is deliberately empty
        assert_eq!(get_inode(0).indexed, 3, "the high-water mark is index + 1");

        let mut out = [0u8; 4096];
        let (next, len, status) = build_dirents(0, 0, &mut out, 4096);
        assert_eq!(status, OK);
        {
            let mut names = [""; 16];
            let n = dirent_names(&out[..len], &mut names);
            assert_eq!(&names[..2], &[".", ".."], "the first two positions are fixed");
            assert_eq!(n, 6, "two indexed and two ordinary children: {names:?}");
            for want in ["pid-0", "pid-2", "static-a", "static-b"] {
                assert_eq!(
                    names[2..n].iter().filter(|got| **got == want).count(),
                    1,
                    "{want} is listed exactly once: {names:?}"
                );
            }
        }
        // Position 3 named a hole and was skipped, so 7 positions cover 6 records.
        assert_eq!(next, 7, "the reply's seek_pos skips the empty index");

        // Resuming at an ordinary child lists the remainder and then stops.
        let (next, len, status) = build_dirents(0, 5, &mut out, 4096);
        assert_eq!(status, OK);
        {
            let mut names = [""; 16];
            let n = dirent_names(&out[..len], &mut names);
            assert_eq!(n, 2);
            for want in ["static-a", "static-b"] {
                assert_eq!(names[..n].iter().filter(|got| **got == want).count(), 1);
            }
        }
        assert_eq!(next, 7);

        let (_, len, status) = build_dirents(0, 7, &mut out, 4096);
        assert_eq!(status, OK);
        assert_eq!(len, 0, "past the end is an empty read, not an error");
    }

    /// All tests run in a single function because VTreeFS uses global static
    /// state (UnsafeCell) and cannot tolerate parallel test execution.
    #[test]
    fn vtreefs_all() {
        // --- layout pins -------------------------------------------------
        assert_eq!(core::mem::size_of::<Message>(), 64);
        assert_eq!(core::mem::size_of::<VfsStat>(), 88);
        assert_eq!(core::mem::offset_of!(VfsStat, st_dev), 0);
        assert_eq!(core::mem::offset_of!(VfsStat, st_ino), 8);
        assert_eq!(core::mem::offset_of!(VfsStat, st_mode), 16);
        assert_eq!(core::mem::offset_of!(VfsStat, st_rdev), 32);
        assert_eq!(core::mem::offset_of!(VfsStat, st_size), 40);
        assert_eq!(core::mem::offset_of!(VfsStat, st_blocks), 56);
        assert_eq!(core::mem::size_of::<VfsStatvfs>(), 88);

        // --- init + table -------------------------------------------------
        let r = vtreefs_init(null_hooks(), 64, test_root_stat());
        assert_eq!(r, OK);
        assert_eq!(get_root_inode(), 0);
        assert_eq!(INODE_COUNT.load(Ordering::Relaxed), 1);

        let stat = InodeStat {
            mode: I_DIRECTORY as u32 | 0o555,
            uid: 0,
            gid: 0,
            size: 0,
            dev: 0,
        };
        let id = add_inode(0, "testfile", NO_INDEX, &stat, 42);
        assert_eq!(id, 1);
        assert_eq!(find_inode(0, "testfile"), Some(1));
        assert_eq!(get_inode_name(1), "testfile");
        assert_eq!(get_inode_cbdata(1), 42);

        // --- mount (readsuper) fires init_hook + refs root ---------------
        static INIT_CALLED: AtomicBool = AtomicBool::new(false);
        fn init_hook() {
            INIT_CALLED.store(true, Ordering::Relaxed);
        }
        vtreefs_init(
            FsHooks {
                init_hook: Some(init_hook),
                ..FsHooks::empty()
            },
            64,
            test_root_stat(),
        );
        INIT_CALLED.store(false, Ordering::Relaxed);

        // readsuper before mount: reply has root inode properties.
        let mut msg = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        let raw = raw_of_mut(&mut msg);
        w_u32(raw, 4, 0); // flags: not root
        let status = handle_fs_message(&mut msg);
        assert_eq!(status, OK);
        assert!(
            INIT_CALLED.load(Ordering::Relaxed),
            "init_hook fires on mount"
        );
        assert_eq!(r_u32(raw_of(&msg), 12), 0); // inode = root
        assert_eq!(r_i64(raw_of(&msg), 0), 0); // file_size
        assert_eq!(r_u32(raw_of(&msg), 16), RES_NOFLAGS);

        // readsuper with REQ_ISROOT is rejected.
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let mut msg = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        let raw = raw_of_mut(&mut msg);
        w_u32(raw, 4, REQ_ISROOT);
        assert_eq!(handle_fs_message(&mut msg), EINVAL);

        // --- lookup -------------------------------------------------------
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let dir = InodeStat {
            mode: I_DIRECTORY as u32 | 0o555,
            ..test_root_stat()
        };
        let file = InodeStat {
            mode: I_REGULAR as u32 | 0o444,
            size: 5,
            ..test_root_stat()
        };
        add_inode(0, "devices", NO_INDEX, &dir, 0);
        add_inode(1, "tty0", NO_INDEX, &file, 7);

        // Mount first — only REQ_READSUPER is served before mounting.
        let mut mnt = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        assert_eq!(handle_fs_message(&mut mnt), OK);

        // Resolve /devices/tty0 from the root.
        let mut msg = new_message(REQ_LOOKUP, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 0); // dir_ino = root
            w_u32(raw, 4, 0); // root_ino
            w_u32(raw, 12, 0); // flags
            w_u32(raw, 20, 13); // path_len, bytes without the terminator
            raw[24..28].copy_from_slice(&(-1i32).to_le_bytes()); // grant_path
            host_path::set(b"/devices/tty0");
        }
        let status = handle_fs_message(&mut msg);
        assert_eq!(status, OK);
        assert_eq!(r_u32(raw_of(&msg), 20), 2, "resolves to tty0 inode");
        assert_eq!(r_u32(raw_of(&msg), 24), file.mode, "mode");
        assert_eq!(r_i64(raw_of(&msg), 8), 5, "file_size");
        // The result inode holds one reference for VFS.
        assert_eq!(get_inode(2).count, 1);

        // Nonexistent component → ENOENT.
        let mut msg = new_message(REQ_LOOKUP, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 0);
            w_u32(raw, 4, 0);
            w_u32(raw, 20, 10); // "/nope/nope", without the terminator
            raw[24..28].copy_from_slice(&(-1i32).to_le_bytes());
            host_path::set(b"/nope/nope");
        }
        assert_eq!(handle_fs_message(&mut msg), ENOENT);

        // ".." from a child directory resolves to the parent.
        let mut msg = new_message(REQ_LOOKUP, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 1); // dir_ino = devices
            w_u32(raw, 4, 0);
            w_u32(raw, 20, 2); // ".." without the terminator
            raw[24..28].copy_from_slice(&(-1i32).to_le_bytes());
            host_path::set(b"..");
        }
        let status = handle_fs_message(&mut msg);
        assert_eq!(status, OK);
        assert_eq!(r_u32(raw_of(&msg), 20), 0, ".. from devices → root");

        // --- putnode ------------------------------------------------------
        let mut msg = new_message(REQ_PUTNODE, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_i64(raw, 0, 1); // count
            w_u32(raw, 8, 2); // inode
        }
        assert_eq!(handle_fs_message(&mut msg), OK);
        assert_eq!(get_inode(2).count, 0);

        // --- getdents -----------------------------------------------------
        let mut msg = new_message(REQ_GETDENTS, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 1); // inode = devices
            w_i64(raw, 8, 0); // seek_pos
            w_i32(raw, 16, -1); // grant — no copy on host
            w_i64(raw, 24, 4096); // mem_size
        }
        let status = handle_fs_message(&mut msg);
        assert_eq!(status, OK);
        // Entries: ".", "..", "tty0" → 3 records, 2 children skipped.
        assert_eq!(r_i64(raw_of(&msg), 0), 3, "next seek_pos");
        let nbytes = r_u32(raw_of(&msg), 8) as usize;
        assert!(nbytes > 0, "getdents produced bytes");

        // --- read ---------------------------------------------------------
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let file = InodeStat {
            mode: I_REGULAR as u32 | 0o444,
            ..test_root_stat()
        };
        add_inode(0, "events", NO_INDEX, &file, 99);

        static READ_DATA: &[u8] = b"ADD /devices/tty0 0x00000001";
        fn read_hook(_node: u32, offset: u64, buf: &mut [u8]) -> usize {
            let data = READ_DATA;
            let start = (offset as usize).min(data.len());
            let n = (data.len() - start).min(buf.len());
            buf[..n].copy_from_slice(&data[start..start + n]);
            n
        }
        vtreefs_init(
            FsHooks {
                read_hook: Some(read_hook),
                ..FsHooks::empty()
            },
            64,
            test_root_stat(),
        );
        add_inode(0, "events", NO_INDEX, &file, 0);

        // Mount again (the re-init above cleared the mounted state).
        let mut mnt = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        assert_eq!(handle_fs_message(&mut mnt), OK);

        let mut msg = new_message(REQ_READ, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 1); // inode
            w_i64(raw, 8, 0); // pos
            w_i32(raw, 16, -1); // grant — no copy on host
            w_i64(raw, 24, 4096); // nbytes
        }
        let status = handle_fs_message(&mut msg);
        // The read reply m_type carries the byte count (MFS protocol:
        // VFS's do_read reports it as the read() result).
        assert_eq!(status, READ_DATA.len() as i32, "status = byte count");
        assert_eq!(
            r_i64(raw_of(&msg), 0),
            READ_DATA.len() as i64,
            "seek_pos advanced"
        );

        // --- unmount ------------------------------------------------------
        let mut msg = new_message(REQ_UNMOUNT, arch_common::com::VFS_PROC_NR);
        assert_eq!(handle_fs_message(&mut msg), OK);
        assert!(!FS_MOUNTED.load(Ordering::Relaxed));

        // --- message hook for non-VFS sources ----------------------------
        static HOOK_CALLED: AtomicBool = AtomicBool::new(false);
        fn msg_hook(_m: &mut [u8; 64]) -> i32 {
            HOOK_CALLED.store(true, Ordering::Relaxed);
            0
        }
        vtreefs_init(
            FsHooks {
                message_hook: Some(msg_hook),
                ..FsHooks::empty()
            },
            64,
            test_root_stat(),
        );
        HOOK_CALLED.store(false, Ordering::Relaxed);
        let mut msg = new_message(0x1200, 42); // DEVMAN_ADD_DEV from a driver
        let status = handle_fs_message(&mut msg);
        assert_eq!(status, 0);
        assert!(
            HOOK_CALLED.load(Ordering::Relaxed),
            "message hook called for non-VFS source"
        );

        // --- stat / statvfs with grant < 0 (no copy, still OK) ------------
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let file = InodeStat {
            mode: I_REGULAR as u32 | 0o444,
            size: 7,
            ..test_root_stat()
        };
        add_inode(0, "devman_id", NO_INDEX, &file, 0);

        // Mount again.
        let mut mnt = new_message(REQ_READSUPER, arch_common::com::VFS_PROC_NR);
        assert_eq!(handle_fs_message(&mut mnt), OK);

        let mut msg = new_message(REQ_STAT, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_u32(raw, 0, 1);
            w_i32(raw, 8, -1);
        }
        assert_eq!(handle_fs_message(&mut msg), OK);

        let mut msg = new_message(REQ_STATVFS, arch_common::com::VFS_PROC_NR);
        {
            let raw = raw_of_mut(&mut msg);
            w_i32(raw, 0, -1);
        }
        assert_eq!(handle_fs_message(&mut msg), OK);

        // --- fill the table -------------------------------------------------
        vtreefs_init(null_hooks(), 64, test_root_stat());
        for _ in 1..MAX_INODES {
            let id = add_inode(0, "x", NO_INDEX, &file, 0);
            assert_ne!(id, u32::MAX, "unexpected table-full");
        }
        assert_eq!(add_inode(0, "y", NO_INDEX, &file, 0), u32::MAX);
    }

    #[test]
    fn stat_layout_matches_mfs() {
        // Must match `crates/fs/src/mfs/types.rs::Stat` exactly.
        assert_eq!(core::mem::size_of::<VfsStat>(), 88);
        assert_eq!(core::mem::offset_of!(VfsStat, st_dev), 0);
        assert_eq!(core::mem::offset_of!(VfsStat, st_ino), 8);
        assert_eq!(core::mem::offset_of!(VfsStat, st_mode), 16);
        assert_eq!(core::mem::offset_of!(VfsStat, st_nlink), 20);
        assert_eq!(core::mem::offset_of!(VfsStat, st_uid), 24);
        assert_eq!(core::mem::offset_of!(VfsStat, st_gid), 28);
        assert_eq!(core::mem::offset_of!(VfsStat, st_rdev), 32);
        assert_eq!(core::mem::offset_of!(VfsStat, st_size), 40);
        assert_eq!(core::mem::offset_of!(VfsStat, st_blksize), 48);
        assert_eq!(core::mem::offset_of!(VfsStat, st_blocks), 56);
        assert_eq!(core::mem::offset_of!(VfsStat, st_atime), 64);
        assert_eq!(core::mem::offset_of!(VfsStat, st_mtime), 72);
        assert_eq!(core::mem::offset_of!(VfsStat, st_ctime), 80);
    }
}
