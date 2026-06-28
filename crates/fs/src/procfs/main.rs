//! ProcFS entry point — adapted from `minix/fs/procfs/main.c`

use crate::procfs::consts::*;
use crate::procfs::types::File;

// ── VTreeFS stub functions ────────────────────────────────────────────

/// Stub: add an inode to the virtual tree.
///
/// `parent` is the parent node index, `name` is the entry name,
/// `index` is the optional index (NO_INDEX for static files),
/// `mode` holds the file type and permissions.
///
/// TODO: call the real `add_inode()` from VTreeFS.
fn add_inode(_parent: u16, _name: &str, _index: i32, _mode: u32) -> u16 {
    0
}

/// Stub: return the root inode index.
///
/// TODO: call the real `get_root_inode()`.
#[allow(dead_code)]
fn get_root_inode() -> u16 {
    1
}

/// Stub: start the VTreeFS event loop (does not return).
///
/// TODO: call the real `start_vtreefs()`.
/// Stub: start the VTreeFS event loop (does not return).
///
/// TODO: call the real `start_vtreefs()`.
fn start_vtreefs() {
    todo!("VTreeFS event loop — not yet implemented")
}

// ── Hook table ────────────────────────────────────────────────────────

/// Initialization hook called by VTreeFS at startup.
///
/// Constructs the static portion of the ProcFS tree.
#[allow(dead_code)]
fn init_hook() {
    let root = get_root_inode();
    construct_tree(root, crate::procfs::root::ROOT_FILES);
}

/// Recursively construct the static portion of the ProcFS tree.
///
/// For each non-sentinel entry in `files`:
/// - If the file mode has `S_IFDIR` set, the entry describes a subdirectory
///   to be created recursively.
/// - Otherwise, a regular inode is created.
///
/// TODO: handle directory entries by passing the child file array.
pub fn construct_tree(dir: u16, files: &[File]) {
    for file in files {
        // Sentinel check.
        if file.name.is_empty() {
            break;
        }

        let _node = add_inode(dir, file.name, NO_INDEX, file.mode);

        if file.mode & crate::procfs::consts::S_IFDIR != 0 {
            // Directory entry: recurse.
            // The child file list must be stored inside FileData — currently
            // FileData does not have a directory variant.  This will need to
            // be added when subdirectories (e.g. /proc/net/) are implemented.
            // For now, skip recursion.
        }
    }
}

/// Initialize the ProcFS tree state.
///
/// This updates the process tables and counts PID directory entries.
/// Returns `OK` on success.
///
/// TODO: call `update_tables()`, count PID_FILES entries.
pub fn init_tree() -> i32 {
    OK
}

/// ProcFS entry point.
///
/// Initializes the tree, sets up root directory properties, and starts
/// the VTreeFS event loop (which does not return).
///
/// TODO: in a real build this is called from `main()`.
pub fn procfs_main() {
    let r = init_tree();
    if r != OK {
        return;
    }

    // Start VTreeFS (does not return).
    start_vtreefs();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_tree_returns_ok() {
        assert_eq!(init_tree(), OK);
    }

    #[test]
    fn init_hook_no_panic() {
        init_hook();
        init_hook(); // second call should also be fine
    }

    #[test]
    fn construct_tree_empty() {
        construct_tree(0, &[]);
    }

    #[test]
    fn construct_tree_with_sentinel() {
        let files = [File {
            name: "",
            mode: 0,
            data: crate::procfs::types::FileData::None,
        }];
        construct_tree(0, &files);
    }
}
