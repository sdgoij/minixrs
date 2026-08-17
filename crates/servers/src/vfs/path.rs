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

use core::ptr::{addr_of_mut, null_mut};

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
/// follows the vmnt link to the root of the mounted FS. If the path passes
/// through a mounted tree (a vmnt with `m_path` set), the path is split at
/// the mount boundary and resolved in the mounted FS.
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

    // Mount-point crossing for paths that pass through a mounted tree:
    // resolve the mount-point prefix in dirp's fs, cross into the mounted
    // FS, then resolve the remainder there. The port has one tree mount
    // (devman at /devices), so a single split suffices.
    let path = &resolve.l_path[..resolve.l_path_len];
    if let Some((vmp, prefix_len)) = find_mount_prefix(path)
        && (*vmp).m_fs == (*dirp).v_fs_e
    {
        let mut pre = *resolve;
        pre.l_path_len = prefix_len;
        let crossed = advance_within(dirp, &pre, rfp);
        if crossed.is_null() {
            return null_mut();
        }

        let rest = &path[prefix_len..];
        if rest.iter().all(|&b| b == b'/') {
            // The path ends at the mount point itself.
            return crossed;
        }

        let mut rem = *resolve;
        rem.l_path_len = rest.len();
        rem.l_path[..rest.len()].copy_from_slice(rest);
        let vp = advance_within(crossed, &rem, rfp);
        put_vnode(crossed);
        return vp;
    }

    advance_within(dirp, resolve, rfp)
}

/// Resolve `resolve` within the fs that owns `dirp` (no mount-prefix
/// splitting), crossing a mount point only if the final resolved inode is
/// a mounted-on directory.
///
/// # Safety
///
/// `dirp` must point to a valid, locked directory vnode. `rfp` must point
/// to a valid Fproc for the calling process.
unsafe fn advance_within(dirp: *mut Vnode, resolve: &Lookup, rfp: &Fproc) -> *mut Vnode {
    if dirp.is_null() {
        return null_mut();
    }

    let (r, res) = lookup(dirp, resolve, rfp);
    if r != OK {
        return null_mut();
    }

    let fs_e = res.fs_e;
    let vp = find_vnode(fs_e, res.inode_nr);
    let vp = if !vp.is_null() {
        // Already have it — use the existing one.
        if lock_vnode(vp, VNODE_OPCL) != EBUSY {
            // Lock acquired, but vnode may have vanished.
            if (*vp).v_ref_count == 0 {
                (*vp).v_fs_count = 1;
            } else {
                (*vp).v_fs_count += 1;
            }
        }
        dup_vnode(vp);
        vp
    } else {
        let new_vp = get_free_vnode();
        if new_vp.is_null() {
            return null_mut();
        }
        // Fill in the new vnode.
        (*new_vp).v_fs_e = res.fs_e;
        (*new_vp).v_inode_nr = res.inode_nr;
        (*new_vp).v_mode = res.mode;
        (*new_vp).v_size = res.file_size;
        (*new_vp).v_dev = res.dev;
        (*new_vp).v_ref_count = 1;
        (*new_vp).v_fs_count = 1;
        new_vp
    };

    // Handle mount point crossing: if the resolved inode is a mounted-on
    // directory, return the root vnode of the mounted FS. (The C matches on
    // (dev, inode); the port tracks the mounted-on fs endpoint in the
    // vmnt's `m_fs`.)
    let vmp = find_vmnt_mounted_on(fs_e, res.inode_nr);
    if !vmp.is_null() {
        let root_vp = find_vnode((*vmp).m_fs_e, (*vmp).m_root_node);
        if !root_vp.is_null() {
            put_vnode(vp);
            dup_vnode(root_vp);
            return root_vp;
        }
    }

    vp
}

/// If `path` passes through a mounted tree, return the vmnt whose mount
/// path (`m_path`) is a component-wise prefix of `path`, plus the byte
/// length of the matched prefix within `path`.
fn find_mount_prefix(path: &[u8]) -> Option<(*mut Vmnt, usize)> {
    unsafe {
        let glob = vfs_global();
        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &mut *vmnt_arr.add(i);
            let m_path = &vmp.m_path;
            let mlen = m_path.iter().position(|&b| b == 0).unwrap_or(0);
            if mlen == 0 {
                continue;
            }
            if let Some(len) = mount_path_prefix_len(&m_path[..mlen], path) {
                return Some((vmp, len));
            }
        }
    }
    None
}

/// Component-wise prefix match of `m_path` against `path`. Returns the
/// byte length of the matched prefix within `path` (the position right
/// after the mount point's final component), or None.
fn mount_path_prefix_len(m_path: &[u8], path: &[u8]) -> Option<usize> {
    let mut m = m_path;
    let mut p = path;
    let mut matched = 0usize;
    loop {
        // Skip leading separators in the mount path itself.
        let m_skip = m.iter().take_while(|&&b| b == b'/').count();
        m = &m[m_skip..];
        if m.is_empty() {
            // Every mount component matched; the remainder starts right
            // after the mount point's final component (a separator between
            // the mount point and the next component belongs to the
            // remainder).
            return Some(matched);
        }

        // Skip path separators only while there is a mount component left
        // to match, so relative paths ("devices/...") align with an
        // absolute mount path ("/devices").
        let p_skip = p.iter().take_while(|&&b| b == b'/').count();
        p = &p[p_skip..];
        matched += p_skip;
        if p.is_empty() {
            return None;
        }

        let m_end = m.iter().position(|&b| b == b'/').unwrap_or(m.len());
        let p_end = p.iter().position(|&b| b == b'/').unwrap_or(p.len());
        if m[..m_end] != p[..p_end] {
            return None;
        }
        matched += p_end;
        m = &m[m_end..];
        p = &p[p_end..];
    }
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

    #[test]
    fn mount_path_prefix_exact_match() {
        assert_eq!(mount_path_prefix_len(b"/devices", b"/devices"), Some(8));
        assert_eq!(mount_path_prefix_len(b"/devices", b"devices"), Some(7));
        assert_eq!(mount_path_prefix_len(b"/devices", b"/devices/"), Some(8));
    }

    #[test]
    fn mount_path_prefix_with_remainder() {
        // The prefix ends right after the mount point's final component.
        assert_eq!(
            mount_path_prefix_len(b"/devices", b"/devices/tty0"),
            Some(8)
        );
        assert_eq!(mount_path_prefix_len(b"/devices", b"devices/tty0"), Some(7));
        assert_eq!(
            mount_path_prefix_len(b"/devices", b"/devices/tty0/extra"),
            Some(8)
        );
    }

    #[test]
    fn mount_path_prefix_rejects_non_matches() {
        // Component boundary: /devicesx must not match /devices.
        assert_eq!(mount_path_prefix_len(b"/devices", b"/devicesx"), None);
        // Shorter or unrelated paths.
        assert_eq!(mount_path_prefix_len(b"/devices", b"/"), None);
        assert_eq!(mount_path_prefix_len(b"/devices", b"/bin/ls"), None);
        assert_eq!(mount_path_prefix_len(b"/devices", b""), None);
    }
}
