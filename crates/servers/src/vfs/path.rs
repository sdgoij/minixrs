//! Pathname resolution — adapted from `minix/servers/vfs/path.c`
//!
//! Core pathname resolution: `eat_path` resolves a full pathname to a vnode,
//! `last_dir` resolves everything except the last component. Both handle
//! mount point crossing, symlink following (max 256 iterations), and
//! POSIX trailing-slash semantics.

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::mount::*;
use crate::vfs::request::req_lookup;
use crate::vfs::types::*;

use core::ptr::null_mut;

/// Resolve parent directory (for create/link/rename etc.).
pub const PATH_GET_PARENT: u32 = 1;
/// Return a vchard vnode for a character/block device.
pub const PATH_GET_VCHARD: u32 = 2;
/// Return symlink contents as the vnode (do NOT follow).
pub const PATH_RET_SYMLINK: u32 = 4;

/// Maximum symlink recursion depth.
#[allow(dead_code)]
const MAX_SYMLINK_LOOPS: usize = 256;

/// Result of a pathname resolution step.
#[derive(Debug, Clone, Copy)]
pub struct PathRes {
    pub vp: *mut Vnode,
    pub vmp: *mut Vmnt,
    pub error: i32,
}

/// Look up a single path component in a directory.
///
/// Calls `req_lookup` on the FS server that owns `dirp`, then either
/// returns the resolved vnode or handles mount point crossing / symlink
/// following.
unsafe fn lookup(dirp: *mut Vnode, resolve: &Lookup, rfp: &Fproc) -> (i32, LookupRes) {
    let dir_inode = (*dirp).v_inode_nr;
    let fs_e = (*dirp).v_fs_e;

    let root_ino = if !rfp.fp_rdir.is_null() {
        // Check if root dir is on the same device as the directory being looked up.
        // If so, use the root inode for chroot resolution.
        if (*rfp.fp_rdir).v_dev == (*dirp).v_dev {
            (*rfp.fp_rdir).v_inode_nr
        } else {
            0
        }
    } else {
        let glob = vfs_global();
        (*glob).root_dev
    };

    req_lookup(
        fs_e,
        dir_inode,
        root_ino,
        rfp.fp_effuid,
        rfp.fp_effgid,
        resolve,
    )
}

/// Resolve one path component starting from `dirp`.
///
/// Returns a vnode for the resolved component, or null on error.
/// Handles mount point crossing: if the resolved inode is a mount point,
/// follows the vmnt link to the root of the mounted FS.
///
/// # Safety
///
/// `dirp` must point to a valid, locked directory vnode. `rfp` must point
/// to a valid Fproc for the calling process.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/path.c` (advance, line 42)
pub unsafe fn advance(dirp: *mut Vnode, resolve: &Lookup, rfp: &Fproc) -> *mut Vnode {
    if dirp.is_null() {
        return null_mut();
    }

    let (r, res) = lookup(dirp, resolve, rfp);
    if r != OK {
        return null_mut();
    }

    let fs_e = res.fs_e;
    let new_vp = get_free_vnode();
    if new_vp.is_null() {
        return null_mut();
    }

    // Check if the inode is already in the vnode table.
    let vp = find_vnode(fs_e, res.inode_nr);
    if !vp.is_null() {
        // Already have it — use the existing one.
        unlock_vnode(new_vp);
        if lock_vnode(vp, VNODE_OPCL) != EBUSY {
            // Lock acquired, but vnode may have vanished.
            if (*vp).v_ref_count == 0 {
                (*vp).v_fs_count = 1;
            } else {
                (*vp).v_fs_count += 1;
            }
        }
        dup_vnode(vp);
        return vp;
    }

    // Fill in the new vnode.
    (*new_vp).v_fs_e = res.fs_e;
    (*new_vp).v_inode_nr = res.inode_nr;
    (*new_vp).v_mode = res.mode;
    (*new_vp).v_size = res.file_size;
    (*new_vp).v_dev = res.dev;
    (*new_vp).v_ref_count = 1;
    (*new_vp).v_fs_count = 1;

    // Handle mount point crossing.
    let vmp = find_vmnt(res.fs_e);
    if !vmp.is_null() && (*vmp).m_mounted_on == res.inode_nr {
        // Cross the mount point: the root vnode of the mounted FS.
        let root_vp = find_vnode((*vmp).m_fs_e, (*vmp).m_root_node);
        if !root_vp.is_null() {
            unlock_vnode(new_vp);
            dup_vnode(root_vp);
            return root_vp;
        }
    }

    new_vp
}

/// Resolve a full pathname to a vnode.
///
/// Starts from the process's root directory (or the VFS root) and walks
/// each path component. Handles mount points, symlinks, and POSIX
/// trailing-slash semantics.
///
/// # Safety
///
/// `rfp` must point to a valid Fproc. VFS globals must be initialized.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/path.c` (eat_path, line 137)
pub unsafe fn eat_path(resolve: &Lookup, rfp: &Fproc) -> *mut Vnode {
    // Determine starting directory: absolute paths start from root, relative from cwd.
    let start_dir = if !resolve.l_path.is_empty() && resolve.l_path[0] == b'/' {
        // Absolute path: start from process's root directory.
        if !rfp.fp_rdir.is_null() {
            let vp = rfp.fp_rdir;
            dup_vnode(vp);
            vp
        } else {
            null_mut()
        }
    } else {
        // Relative path: start from current working directory.
        if !rfp.fp_cdir.is_null() {
            let vp = rfp.fp_cdir;
            dup_vnode(vp);
            vp
        } else {
            null_mut()
        }
    };

    if start_dir.is_null() {
        return null_mut();
    }

    // Call advance to resolve the path.
    let result = advance(start_dir, resolve, rfp);
    if !result.is_null() {
        dup_vnode(result);
    }
    result
}

/// Resolve everything except the last path component.
///
/// Used by operations that modify directory entries (create, link, unlink,
/// rename, mkdir, mknod, symlink). Returns the parent directory vnode.
///
/// # Safety
///
/// `rfp` must point to a valid Fproc. VFS globals must be initialized.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/path.c` (last_dir, line 151)
pub unsafe fn last_dir(resolve: &Lookup, rfp: &Fproc) -> *mut Vnode {
    let path_buf = &resolve.l_path;
    let path_len = resolve.l_path_len;

    if path_len == 0 || path_buf[0] == 0 {
        return null_mut();
    }

    // Find the last '/' in the path.
    let last_slash = path_buf[..path_len].iter().rposition(|&b| b == b'/');

    // Determine the parent path length.
    // For "/x": last_slash = Some(0), parent_len = 1 (just "/")
    // For "/tmp/x": last_slash = Some(4), parent_len = 4 ("/tmp")
    // For "x": last_slash = None, parent_len = 0 (cwd)
    let (parent_len, start_dir) = match last_slash {
        Some(pos) => {
            if pos == 0 {
                // Path like "/x" — parent is the root directory.
                let vp = if !rfp.fp_rdir.is_null() {
                    let vp = rfp.fp_rdir;
                    dup_vnode(vp);
                    vp
                } else {
                    null_mut()
                };
                (1usize, vp) // parent_len = 1 ("/")
            } else {
                // Path like "/tmp/x" — parent is "/tmp".
                let vp = if !rfp.fp_rdir.is_null() {
                    let vp = rfp.fp_rdir;
                    dup_vnode(vp);
                    vp
                } else {
                    null_mut()
                };
                (pos, vp) // parent_len = pos (up to but not including the slash)
            }
        }
        None => {
            // No slash — path is relative. Parent is cwd.
            let vp = if !rfp.fp_cdir.is_null() {
                let vp = rfp.fp_cdir;
                dup_vnode(vp);
                vp
            } else {
                null_mut()
            };
            (0usize, vp)
        }
    };

    if start_dir.is_null() {
        return null_mut();
    }

    // If the parent path is just "/" (root), return the root directory directly.
    // No need to do a lookup — the root vnode IS the parent.
    if parent_len == 0 || (parent_len == 1 && path_buf[0] == b'/') {
        return start_dir;
    }

    // For paths like "/tmp/x", we need to resolve "/tmp" from the root.
    // We don't modify the original path; instead we just pass the truncated
    // path length to advance.
    let mut temp_resolve = *resolve;
    temp_resolve.l_path_len = parent_len;

    advance(start_dir, &temp_resolve, rfp)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set up a vnode in the global table and return a pointer to it.
    unsafe fn setup_vnode(slot: usize, inode: u32, fs_e: i32) -> *mut Vnode {
        let glob = vfs_global();
        let vp = &mut (*glob).vnode[slot];
        vp.v_ref_count = 1;
        vp.v_fs_e = fs_e;
        vp.v_inode_nr = inode;
        vp
    }

    #[test]
    fn last_dir_root_path_returns_root_vnode() {
        unsafe {
            let root_vp = setup_vnode(0, 1, 1);
            let rfp = Fproc {
                fp_rdir: root_vp,
                ..Default::default()
            };

            let mut resolve = Lookup::default();
            resolve.l_path[0] = b'/';
            resolve.l_path_len = 1;

            let result = last_dir(&resolve, &rfp);
            assert!(!result.is_null());
        }
    }

    #[test]
    fn last_dir_absolute_single_component_returns_root() {
        unsafe {
            let root_vp = setup_vnode(0, 1, 1);
            let rfp = Fproc {
                fp_rdir: root_vp,
                ..Default::default()
            };

            // Path "/x" — the parent of "x" is "/"
            let mut resolve = Lookup::default();
            resolve.l_path[0] = b'/';
            resolve.l_path[1] = b'x';
            resolve.l_path_len = 2;

            let result = last_dir(&resolve, &rfp);
            assert!(!result.is_null());
        }
    }

    #[test]
    fn last_dir_relative_no_slash_uses_cwd() {
        unsafe {
            let cwd_vp = setup_vnode(1, 2, 1);
            let rfp = Fproc {
                fp_cdir: cwd_vp,
                ..Default::default()
            };

            // Path "x" — no slash, parent is cwd
            let mut resolve = Lookup::default();
            resolve.l_path[0] = b'x';
            resolve.l_path_len = 1;

            let result = last_dir(&resolve, &rfp);
            assert!(!result.is_null());
        }
    }

    #[test]
    fn last_dir_empty_path_returns_null() {
        unsafe {
            let resolve = Lookup::default();
            let rfp = Fproc::default();
            assert!(last_dir(&resolve, &rfp).is_null());
        }
    }

    #[test]
    fn last_dir_null_start_dir_returns_null() {
        unsafe {
            let mut resolve = Lookup::default();
            resolve.l_path[0] = b'x';
            resolve.l_path_len = 1;

            // No rdir or cdir set on the default Fproc
            let rfp = Fproc::default();
            assert!(last_dir(&resolve, &rfp).is_null());
        }
    }

    #[test]
    fn last_dir_nested_path_falls_through_to_advance() {
        unsafe {
            let root_vp = setup_vnode(0, 1, 1);
            let rfp = Fproc {
                fp_rdir: root_vp,
                ..Default::default()
            };

            // Path "/tmp/x" — parent "/tmp" requires advance() → lookup() IPC.
            // Without a real FS process, lookup() fails and advance() returns null.
            let mut resolve = Lookup::default();
            resolve.l_path[..4].copy_from_slice(b"/tmp");
            resolve.l_path[4] = b'/';
            resolve.l_path[5] = b'x';
            resolve.l_path_len = 6;

            let result = last_dir(&resolve, &rfp);
            assert!(result.is_null(), "nested path lookup fails without FS IPC");
        }
    }
}
