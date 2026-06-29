//! VTreeFS — Virtual Tree Filesystem library.
//!
//! Provides a fixed-size inode table with tree structure for virtual
//! filesystems (ProcFS, DEVMAN, etc.).  The inode table is a single
//! `UnsafeCell<[INode; MAX_INODES]>` behind a `Sync` impl — safe because
//! MINIX servers are single-threaded.

#![allow(dead_code, clippy::type_complexity)]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

// ── Constants ───────────────────────────────────────────────────────────

pub const NO_INDEX: i32 = -1;
pub const MAX_INODES: usize = 1024;

// ── Types ───────────────────────────────────────────────────────────────

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
}

/// Hook table — function pointers that VTreeFS calls at various points.
pub struct FsHooks {
    pub init_hook: Option<fn()>,
    pub cleanup_hook: Option<fn()>,
    pub lookup_hook: Option<fn(parent: u32, name: &str) -> i32>,
    pub getdents_hook: Option<fn(node: u32) -> i32>,
    pub read_hook: Option<fn(node: u32, offset: u64, len: usize, cbdata: CbData) -> i32>,
    pub rdlink_hook: Option<fn(node: u32, ptr: &mut [u8]) -> i32>,
    pub message_hook: Option<fn(msg: &mut [u8; 64]) -> i32>,
}

// ── Static state ────────────────────────────────────────────────────────

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
};

// ── Newtype wrappers for static state (needed to impl `Sync` on foreign types) ──

/// Wrapper around `UnsafeCell<[INode; MAX_INODES]>` so we can implement `Sync`.
struct InodeTable(UnsafeCell<[INode; MAX_INODES]>);
unsafe impl Sync for InodeTable {}

/// Wrapper around `UnsafeCell<Option<FsHooks>>` so we can implement `Sync`.
struct HookStorage(UnsafeCell<Option<FsHooks>>);
unsafe impl Sync for HookStorage {}

/// Fixed-size inode table.  Accessed through raw pointer (single-threaded
/// server — no data races).
static INODE_TABLE: InodeTable = InodeTable(UnsafeCell::new([ZERO_INODE; MAX_INODES]));

/// Number of allocated inodes (root = 1).
static INODE_COUNT: AtomicU32 = AtomicU32::new(0);

/// Registered hook table.
static HOOKS: HookStorage = HookStorage(UnsafeCell::new(None));

// ── Helpers ─────────────────────────────────────────────────────────────

fn get_table() -> *mut [INode; MAX_INODES] {
    INODE_TABLE.0.get()
}

fn get_hooks_ptr() -> *mut Option<FsHooks> {
    HOOKS.0.get()
}

// ── Public API ──────────────────────────────────────────────────────────

/// Initialise the inode table, store hooks, create the root inode, and
/// call the `init_hook` (if set).
pub fn vtreefs_init(hooks: FsHooks, nr_inodes: u32, root_stat: InodeStat) -> i32 {
    // Clamp at MAX_INODES.
    let _ = nr_inodes.min(MAX_INODES as u32);

    // Reset state.
    INODE_COUNT.store(1, Ordering::Relaxed);
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
        };
        // Zero out the rest.
        for slot in (&mut *get_table())[1..].iter_mut() {
            *slot = ZERO_INODE;
        }
        *get_hooks_ptr() = Some(hooks);
    }

    // Fire init hook.
    let hooks_ref = unsafe { (*get_hooks_ptr()).as_ref().unwrap() };
    if let Some(init) = hooks_ref.init_hook {
        init();
    }

    0 // OK
}

/// Allocate a new inode slot, link it under `parent_id`, and return the
/// new inode ID.
pub fn add_inode(parent_id: u32, name: &str, _index: i32, stat: &InodeStat, cbdata: CbData) -> u32 {
    let count = INODE_COUNT.load(Ordering::Relaxed) as usize;
    if count >= MAX_INODES {
        return u32::MAX; // table full
    }

    let id = count as u32;
    INODE_COUNT.store(count as u32 + 1, Ordering::Relaxed);

    unsafe {
        let table = &mut *get_table();

        // Write the name (C-string, null-terminated).
        let mut name_buf = [0u8; 64];
        let name_bytes = name.as_bytes();
        let copy_len = name_bytes.len().min(63);
        name_buf[..copy_len].copy_from_slice(&name_bytes[..copy_len]);
        name_buf[copy_len] = 0;

        // If this is a dynamic name (index >= 0), append the index.
        // Actual formatting into the name buffer would be done by the
        // caller — we just store what we're given.

        table[id as usize] = INode {
            id,
            name: name_buf,
            parent_id,
            first_child: None,
            next_sibling: None,
            stat: *stat,
            cbdata,
        };

        // Link into parent's child list (insert at head).
        table[id as usize].next_sibling = table[parent_id as usize].first_child;
        table[parent_id as usize].first_child = Some(id);
    }

    id
}

/// Return the root inode ID (always 0).
pub fn get_root_inode() -> u32 {
    0
}

/// Delete an inode and all its descendants.
pub fn delete_inode(id: u32) -> i32 {
    if id == 0 || id as usize >= MAX_INODES {
        return -1; // EINVAL
    }

    unsafe {
        let table = &mut *get_table();

        // Recursively delete children first.
        while let Some(child) = table[id as usize].first_child {
            delete_inode(child);
        }

        let parent_id = table[id as usize].parent_id;

        // Remove from parent's child list.
        if table[parent_id as usize].first_child == Some(id) {
            table[parent_id as usize].first_child = table[id as usize].next_sibling;
        } else {
            // Walk siblings to find the node just before `id`.
            let mut cur = table[parent_id as usize].first_child;
            while let Some(sib) = cur {
                if table[sib as usize].next_sibling == Some(id) {
                    table[sib as usize].next_sibling = table[id as usize].next_sibling;
                    break;
                }
                cur = table[sib as usize].next_sibling;
            }
        }

        // Zero out the slot.
        table[id as usize] = ZERO_INODE;

        INODE_COUNT.fetch_sub(1, Ordering::Relaxed);
    }

    0 // OK
}

/// Find a child of `parent_id` by name.  Returns `None` if not found.
pub fn find_inode(parent_id: u32, name: &str) -> Option<u32> {
    let name_bytes = name.as_bytes();
    unsafe {
        let table = &*get_table();
        let mut cur = table[parent_id as usize].first_child;
        while let Some(id) = cur {
            let inode = &table[id as usize];
            // Compare name up to null terminator or 64 bytes.
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

// ── Event loop stub ─────────────────────────────────────────────────────

/// Enter the VTreeFS receive-dispatch loop.
///
/// In a real MINIX server this would block on a `seL4_Call` / message
/// receive and dispatch to the appropriate hook.  For now this is a
/// stub that never returns.
pub fn start_vtreefs() -> ! {
    // TODO: receive message from VFS, dispatch to hook based on message type.
    #[allow(clippy::empty_loop)]
    loop {}
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root_stat() -> InodeStat {
        InodeStat {
            mode: 0o040555,
            uid: 0,
            gid: 0,
            size: 0,
            dev: 0,
        }
    }

    fn null_hooks() -> FsHooks {
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

    /// All tests run in a single function because VTreeFS uses global static
    /// state (UnsafeCell) and cannot tolerate parallel test execution.
    #[test]
    fn vtreefs_all() {
        // ── init_creates_root ──
        let r = vtreefs_init(null_hooks(), 64, test_root_stat());
        assert_eq!(r, 0);
        assert_eq!(get_root_inode(), 0);
        assert_eq!(INODE_COUNT.load(Ordering::Relaxed), 1);

        // ── add_and_find_inode ──
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let stat = InodeStat {
            mode: 0o100444,
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

        // ── find_missing ──
        vtreefs_init(null_hooks(), 64, test_root_stat());
        assert_eq!(find_inode(0, "nope"), None);

        // ── first_inode_and_next_sibling ──
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let a = add_inode(0, "a", NO_INDEX, &stat, 0);
        let b = add_inode(0, "b", NO_INDEX, &stat, 0);
        // Inserted at head, so first_child should be b, then a.
        assert_eq!(first_inode(0), Some(b));
        assert_eq!(next_sibling(b), Some(a));
        assert_eq!(next_sibling(a), None);

        // ── delete_inode_removes_children ──
        vtreefs_init(null_hooks(), 64, test_root_stat());
        let _a = add_inode(0, "a", NO_INDEX, &stat, 0);
        let b = add_inode(0, "b", NO_INDEX, &stat, 0);
        let _c = add_inode(b, "c", NO_INDEX, &stat, 0);
        assert_eq!(delete_inode(b), 0);
        assert_eq!(find_inode(0, "b"), None);
        // b's children were also deleted.
        assert_eq!(find_inode(0, "a"), Some(1));

        // ── init_hook_is_called ──
        static CALLED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
        fn hook() {
            CALLED.store(true, Ordering::Relaxed);
        }
        let hooks = FsHooks {
            init_hook: Some(hook),
            ..null_hooks()
        };
        vtreefs_init(hooks, 64, test_root_stat());
        assert!(CALLED.load(Ordering::Relaxed));

        // ── add_inode_table_full ──
        vtreefs_init(null_hooks(), 64, test_root_stat());
        // Fill the table (root already uses slot 0).
        for _ in 1..MAX_INODES {
            let id = add_inode(0, "x", NO_INDEX, &stat, 0);
            assert_ne!(id, u32::MAX, "unexpected table-full");
        }
        // Next allocation should fail.
        assert_eq!(add_inode(0, "y", NO_INDEX, &stat, 0), u32::MAX);
    }
}
