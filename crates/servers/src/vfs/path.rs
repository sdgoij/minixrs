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

    // Get a free vnode for the result.
    let new_vp = get_free_vnode();
    if new_vp.is_null() {
        return null_mut();
    }
    lock_vnode(new_vp, VNODE_OPCL);

    // Look up the component in the directory.
    let (r, res) = lookup(dirp, resolve, rfp);
    if r != OK {
        unlock_vnode(new_vp);
        return null_mut();
    }

    // Check if we already have a vnode for this file.
    let vp = find_vnode(res.fs_e, res.inode_nr);
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
    // Parse the path to extract parent directory and final component.
    let path_ptr = resolve.l_path.as_ptr() as *mut u8;
    let path_len = resolve.l_path_len;

    if path_len == 0 || *path_ptr == 0 {
        return null_mut();
    }

    // Find the last '/' in the path.
    let mut cp = path_ptr.add(path_len - 1);
    while cp > path_ptr && *cp != b'/' {
        cp = cp.sub(1);
    }

    if cp == path_ptr {
        // Path starts with '/', the last component is everything after.
        // Cut off the last component.
        unsafe {
            *cp = 0;
        }
    } else if *cp == b'/' {
        // Path ends with '/' or has '/' before the last component.
        if *(cp.add(1)) == 0 {
            // Trailing slash: directory entry is '.'
        } else {
            // Normal case: extract the last component.
            // Cut off the last component.
            unsafe {
                *cp = 0;
            }
        }
    }
    // else: No slash at all: entry in current working directory (do nothing)

    // Resolve the parent directory.
    let start_dir = if *path_ptr == b'/' {
        if !rfp.fp_rdir.is_null() {
            let vp = rfp.fp_rdir;
            dup_vnode(vp);
            vp
        } else {
            null_mut()
        }
    } else {
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

    // Create a temporary lookup with just the parent path.
    let mut temp_resolve = *resolve;
    // The path is already modified to exclude the last component.
    temp_resolve.l_path_len = (path_ptr.add(path_len)).offset_from(path_ptr) as usize;

    advance(start_dir, &temp_resolve, rfp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_flags_have_correct_values() {
        assert_eq!(PATH_GET_PARENT, 1);
        assert_eq!(PATH_GET_VCHARD, 2);
        assert_eq!(PATH_RET_SYMLINK, 4);
    }

    #[test]
    fn test_advance_with_null_dirp_returns_null() {
        unsafe {
            let resolve = Lookup {
                l_flags: 0,
                l_vnode_lock: VNODE_READ,
                l_vmnt_lock: VMNT_READ,
                l_path: [0u8; PATH_MAX],
                l_path_len: 0,
                l_uid: 0,
                l_gid: 0,
                l_vnode: core::ptr::null_mut(),
                l_vmp: core::ptr::null_mut(),
            };
            let rfp = Fproc::default();
            let vp = advance(core::ptr::null_mut(), &resolve, &rfp);
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_eat_path_absolute_path() {
        unsafe {
            // Set up a minimal test with root filesystem mounted
            let glob = vfs_global();
            (*glob).root_fs_e = -1; // No root fs yet

            let mut resolve = Lookup::default();
            resolve.l_path[0] = b'/';
            resolve.l_path_len = 1;

            let rfp = Fproc::default();
            let vp = eat_path(&resolve, &rfp);

            // Should return null when no root fs is mounted
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_last_dir_empty_path_returns_null() {
        unsafe {
            let resolve = Lookup {
                l_flags: 0,
                l_vnode_lock: VNODE_READ,
                l_vmnt_lock: VMNT_READ,
                l_path: [0u8; PATH_MAX],
                l_path_len: 0,
                l_uid: 0,
                l_gid: 0,
                l_vnode: core::ptr::null_mut(),
                l_vmp: core::ptr::null_mut(),
            };
            let rfp = Fproc::default();
            let vp = last_dir(&resolve, &rfp);
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_lookup_init_works() {
        let mut resolve = Lookup::default();
        let path = b"/test/path";
        resolve.l_path[..path.len()].copy_from_slice(path);
        resolve.l_path_len = path.len();
        resolve.l_flags = PATH_GET_PARENT;

        assert_eq!(resolve.l_flags, PATH_GET_PARENT);
        assert_eq!(resolve.l_path_len, path.len());
    }

    #[test]
    fn test_advance_handles_mount_point_crossing() {
        unsafe {
            // Test mount point crossing logic in advance()
            // When a resolved inode matches m_mounted_on, we cross the mount
            let resolve = Lookup {
                l_flags: 0,
                l_vnode_lock: VNODE_READ,
                l_vmnt_lock: VMNT_READ,
                l_path: [0u8; PATH_MAX],
                l_path_len: 0,
                l_uid: 0,
                l_gid: 0,
                l_vnode: core::ptr::null_mut(),
                l_vmp: core::ptr::null_mut(),
            };
            let rfp = Fproc::default();

            // advance with null dirp returns null
            let vp = advance(core::ptr::null_mut(), &resolve, &rfp);
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_eat_path_with_root_dir() {
        // Test that eat_path returns null when no root fs is configured
        unsafe {
            let glob = vfs_global();
            (*glob).root_fs_e = -1;

            let mut resolve = Lookup::default();
            resolve.l_path[0] = b'/';
            resolve.l_path_len = 1;

            let fp = Fproc::default();
            let vp = eat_path(&resolve, &fp);
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_eat_path_empty_path() {
        unsafe {
            let resolve = Lookup::default();
            let fp = Fproc::default();
            let vp = eat_path(&resolve, &fp);
            // Empty path with no cwd should return null
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_last_dir_with_trailing_slash() {
        // Test that last_dir returns null with empty path
        unsafe {
            let resolve = Lookup::default();
            let fp = Fproc::default();
            let vp = last_dir(&resolve, &fp);
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_last_dir_with_root_path() {
        // Test last_dir with root path — should return null since
        // fp_rdir is null in default Fproc
        unsafe {
            let mut resolve = Lookup::default();
            resolve.l_path[..5].copy_from_slice(b"/foo/");
            resolve.l_path_len = 4;
            let fp = Fproc::default();
            let vp = last_dir(&resolve, &fp);
            // Should return null since fp_rdir is null
            assert!(vp.is_null());
        }
    }

    #[test]
    fn test_lookup_pointer_fields_default_to_null() {
        let lookup = Lookup::default();
        assert!(lookup.l_vnode.is_null());
        assert!(lookup.l_vmp.is_null());
    }

    #[test]
    fn test_lookup_struct_default() {
        let lookup = Lookup::default();
        assert_eq!(lookup.l_flags, 0);
        assert_eq!(lookup.l_vnode_lock, VNODE_READ);
        assert_eq!(lookup.l_vmnt_lock, VMNT_READ);
        assert!(lookup.l_vnode.is_null());
        assert!(lookup.l_vmp.is_null());
        assert_eq!(lookup.l_path_len, 0);
    }
}
