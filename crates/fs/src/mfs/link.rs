//! Link, unlink, rename, readlink — adapted from `minix/fs/mfs/link.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::path::*;
use crate::mfs::read::*;
use libs::libminixfs::cache::lmfs_put_block;

/* Args to unlink_file */
const SAME: i32 = 1000;

/// Remove a directory entry from `dirp` and decrement the link count on `rip`.
unsafe fn unlink_file(dirp_idx: u16, rip: Option<u16>, fname: &[u8]) -> i32 {
    let mut numb: u32 = 0;
    let rip = match rip {
        Some(idx) => {
            dup_inode(idx);
            Some(idx)
        }
        None => {
            let ec = search_dir(dirp_idx, fname, Some(&mut numb), LOOK_UP, IGN_PERM);
            if ec != OK {
                return ec;
            }
            let dev = (*glo::get_inode_ptr(dirp_idx as usize)).i_dev;
            get_inode(dev, numb)
        }
    };

    let rip = match rip {
        Some(i) => i,
        None => return EINVAL,
    };

    let r = search_dir(dirp_idx, fname, None, DELETE, IGN_PERM);
    if r == OK {
        let rp = &mut *glo::get_inode_ptr(rip as usize);
        (*rp).i_nlinks = (*rp).i_nlinks.saturating_sub(1);
        (*rp).i_update |= CTIME;
        (*rp).i_dirt = IN_DIRTY;
    }

    put_inode(Some(rip));
    r
}

/// Remove a directory: must be empty, not "." or "..", not root.
unsafe fn remove_dir(rldirp_idx: u16, rip_idx: u16, dir_name: &[u8]) -> i32 {
    let r = search_dir(rip_idx, &[], None, IS_EMPTY, IGN_PERM);
    if r != OK {
        return r;
    }
    if dir_name == b"." || dir_name == b".." {
        return EINVAL;
    }
    let rip = &*glo::get_inode_ptr(rip_idx as usize);
    if (*rip).i_num == ROOT_INODE {
        return EBUSY;
    }
    let r = unlink_file(rldirp_idx, Some(rip_idx), dir_name);
    if r != OK {
        return r;
    }
    let _ = unlink_file(rip_idx, None, &DOT1);
    let _ = unlink_file(rip_idx, None, &DOT2);
    OK
}

/// Read a little-endian field from a request payload.
///
/// VFS writes these fields little-endian (`crates/servers/src/vfs/request.rs`'s `w_*`), and a
/// payload shorter than the field being read yields zero rather than panicking, as this file's
/// other parsers do.
fn payload_u32(raw: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(
        raw.get(off..off + 4)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0u8; 4]),
    )
}

fn payload_i32(raw: &[u8], off: usize) -> i32 {
    payload_u32(raw, off) as i32
}

fn payload_u64(raw: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(
        raw.get(off..off + 8)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0u8; 8]),
    )
}

/// Copy a request's name into `buf` through its grant, and return how many bytes of `buf` hold
/// it.
///
/// The name is the last component of a path VFS resolved, sent NUL-terminated with that NUL
/// counted, and a name that fills `buf` is truncated rather than left unterminated — C's
/// `NUL(string, len, sizeof(string))`. `req_link` and `req_rename` parse their own payload
/// because `mfs_main` unpacks none for them (`KNOWN_ISSUES.md` item 37).
fn copy_name(buf: &mut [u8; MFS_NAME_MAX], grant: i32, len: usize) -> Result<usize, i32> {
    let len = len.min(MFS_NAME_MAX);
    #[cfg(target_os = "minix")]
    let r = safecopy_from_grant(grant, 0, buf.as_mut_ptr(), len);
    #[cfg(not(target_os = "minix"))]
    let r = {
        // No kernel to copy through on the host, so a request that names a file arrives empty
        // and is refused rather than acted on with a name it never carried.
        let _ = grant;
        if len > 0 { EIO } else { OK }
    };
    if r != OK {
        return Err(r);
    }
    if len >= MFS_NAME_MAX {
        buf[MFS_NAME_MAX - 1] = 0;
    }
    Ok(buf.iter().position(|&c| c == 0).unwrap_or(MFS_NAME_MAX))
}

/// Create a hard link.
///
/// Message layout (VFS `req_link`): inode at payload[0], directory inode at payload[4], name
/// grant at payload[8], name length at payload[16]. The name arrives through a grant, which is
/// the only way VFS's own bytes reach a filesystem server, and it is the last component of the
/// path VFS resolved.
///
/// Reference: `minix/fs/mfs/link.c` `fs_link()`
pub fn fs_link() -> i32 {
    unsafe {
        let raw = (*glo::mfs_ptr()).m_in.m_payload.raw;
        let ino = payload_u32(&raw, 0);
        let dir_ino = payload_u32(&raw, 4);
        let grant_path = payload_i32(&raw, 8);
        let path_len = payload_u64(&raw, 16) as usize;
        let dev = (*glo::mfs_ptr()).fs_dev;

        let mut name_buf = [0u8; MFS_NAME_MAX];
        let name_len = match copy_name(&mut name_buf, grant_path, path_len) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let string = &name_buf[..name_len];

        let rip = match get_inode(dev, ino) {
            Some(r) => r,
            None => return EINVAL,
        };

        let mut r = OK;
        if (*glo::get_inode_ptr(rip as usize)).i_nlinks >= LINK_MAX {
            r = EMLINK;
        }
        if r == OK {
            let mode = (*glo::get_inode_ptr(rip as usize)).i_mode;
            if (mode & I_TYPE) == I_DIRECTORY && (*glo::mfs_ptr()).caller_uid != SU_UID as u16 {
                r = EPERM;
            }
        }
        if r != OK {
            put_inode(Some(rip));
            return r;
        }

        let ip = match get_inode(dev, dir_ino) {
            Some(i) => i,
            None => {
                put_inode(Some(rip));
                return EINVAL;
            }
        };
        if (*glo::get_inode_ptr(ip as usize)).i_nlinks == NO_LINK {
            put_inode(Some(rip));
            put_inode(Some(ip));
            return ENOENT;
        }

        let new_ip = advance(ip, string, IGN_PERM);
        if new_ip.is_none() {
            let ec = (*glo::mfs_ptr()).err_code;
            if ec == ENOENT {
                r = OK;
            } else {
                r = ec;
            }
        } else {
            put_inode(new_ip);
            r = EEXIST;
        }

        if r == OK {
            let mut inum = (*glo::get_inode_ptr(rip as usize)).i_num;
            r = search_dir(ip, string, Some(&mut inum), ENTER, IGN_PERM);
        }
        if r == OK {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_add(1);
            (*rp).i_update |= CTIME;
            (*rp).i_dirt = IN_DIRTY;
        }

        put_inode(Some(rip));
        put_inode(Some(ip));
        r
    }
}

pub fn fs_unlink() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let req_nr = (*glo::mfs_ptr()).req_nr;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        let rldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return EINVAL,
        };

        let rip = advance(rldirp, string, IGN_PERM);
        let mut r = (*glo::mfs_ptr()).err_code;
        if r != OK {
            if r == EENTERMOUNT || r == ELEAVEMOUNT {
                if let Some(ip) = rip {
                    put_inode(Some(ip));
                }
                r = EBUSY;
            }
            put_inode(Some(rldirp));
            return r;
        }

        let rip = match rip {
            Some(i) => i,
            None => {
                put_inode(Some(rldirp));
                return ENOENT;
            }
        };

        let rip_ptr = glo::get_inode_ptr(rip as usize);
        if (*rip_ptr).i_sp.map_or(true, |sp| (*sp).s_rd_only != 0) {
            r = EROFS;
        } else if req_nr == (REQ_UNLINK - FS_BASE) {
            let mode = (*glo::get_inode_ptr(rip as usize)).i_mode;
            if (mode & I_TYPE) == I_DIRECTORY {
                r = EPERM;
            }
            if r == OK {
                r = unlink_file(rldirp, Some(rip), string);
            }
        } else {
            r = remove_dir(rldirp, rip, string);
        }

        put_inode(Some(rip));
        put_inode(Some(rldirp));
        r
    }
}

/// Read a symlink's target into the caller's buffer.
///
/// Message layout (VFS `req_rdlink`): inode at payload[0], buffer grant at payload[8],
/// buffer size at payload[16]. The reply carries the number of bytes copied at payload[0]
/// (u64), which is what `req_rdlink` hands back as the call's result.
///
/// The target lives in block 0 of the link's inode — `fs_slink` puts it there and `i_size`
/// is its length without the terminating NUL (`.refs/minix-3.3.0/minix/fs/mfs/link.c:174`).
///
/// Reference: `minix/fs/mfs/link.c` `fs_rdlink()`
/// Reference: `crates/fs/src/ext2/link.rs` `fs_rdlink()` — the same function for ext2, which
/// was ported complete and is the pattern this follows.
pub fn fs_rdlink() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        let raw = (*mfs).m_in.m_payload.raw;
        let rd_u32 = |off: usize| {
            u32::from_le_bytes(
                raw.get(off..off + 4)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or([0u8; 4]),
            )
        };
        let rd_i32 = |off: usize| {
            i32::from_le_bytes(
                raw.get(off..off + 4)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or([0u8; 4]),
            )
        };
        let rd_u64 = |off: usize| {
            u64::from_le_bytes(
                raw.get(off..off + 8)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or([0u8; 8]),
            )
        };

        let ino = rd_u32(0);
        let grant = rd_i32(8);
        let mem_size = rd_u64(16) as usize;
        let dev = (*mfs).fs_dev;

        let rip = match get_inode(dev, ino) {
            Some(r) => r,
            None => return EINVAL,
        };

        let mode = (*glo::get_inode_ptr(rip as usize)).i_mode;
        let r = if (mode & I_TYPE) != I_SYMBOLIC_LINK {
            EACCES
        } else {
            let bp = get_block_map(rip, 0);
            if bp.is_null() {
                EIO
            } else {
                let size = (*glo::get_inode_ptr(rip as usize)).i_size as usize;
                let copied = core::cmp::min(mem_size, size);
                #[cfg(target_os = "minix")]
                let r = safecopy_to_grant(grant, 0, (*bp).data_ptr, copied);
                #[cfg(not(target_os = "minix"))]
                let r = {
                    // No kernel to copy through on the host, so the walk still runs and the
                    // length it resolved is what is replied — the same split ext2's tests use.
                    let _ = (grant, (*bp).data_ptr);
                    OK
                };
                lmfs_put_block(bp, DIRECTORY_BLOCK);
                if r == OK {
                    let reply = &mut (*mfs).m_out.m_payload.raw;
                    reply[0..8].copy_from_slice(&(copied as u64).to_le_bytes());
                }
                r
            }
        };

        put_inode(Some(rip));
        r
    }
}

/// Rename a file or directory.
///
/// Message layout (VFS `req_rename`): old directory inode at payload[0], new directory inode
/// at payload[4], old name length at payload[8], new name length at payload[16], old name grant
/// at payload[24], new name grant at payload[28]. Both names are the last component of the path
/// VFS resolved and both arrive through grants.
///
/// Reference: `minix/fs/mfs/link.c` `fs_rename()`
pub fn fs_rename() -> i32 {
    unsafe {
        let raw = (*glo::mfs_ptr()).m_in.m_payload.raw;
        let dir_old = payload_u32(&raw, 0);
        let dir_new = payload_u32(&raw, 4);
        let len_old = payload_u64(&raw, 8) as usize;
        let len_new = payload_u64(&raw, 16) as usize;
        let grant_old = payload_i32(&raw, 24);
        let grant_new = payload_i32(&raw, 28);
        let dev = (*glo::mfs_ptr()).fs_dev;

        let mut old_buf = [0u8; MFS_NAME_MAX];
        let old_len = match copy_name(&mut old_buf, grant_old, len_old) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let mut new_buf = [0u8; MFS_NAME_MAX];
        let new_len = match copy_name(&mut new_buf, grant_new, len_new) {
            Ok(n) => n,
            Err(e) => return e,
        };
        let old_name = &old_buf[..old_len];
        let new_name = &new_buf[..new_len];

        let old_dirp = match get_inode(dev, dir_old) {
            Some(d) => d,
            None => return EINVAL,
        };
        let old_ip = advance(old_dirp, old_name, IGN_PERM);
        let mut r = (*glo::mfs_ptr()).err_code;

        if r == EENTERMOUNT || r == ELEAVEMOUNT {
            if let Some(ip) = old_ip {
                put_inode(Some(ip));
            }
            r = if r == EENTERMOUNT { EXDEV } else { EINVAL };
        }
        if r != OK || old_ip.is_none() {
            put_inode(Some(old_dirp));
            return r;
        }
        let old_ip = old_ip.unwrap();

        let new_dirp = match get_inode(dev, dir_new) {
            Some(d) => d,
            None => {
                put_inode(Some(old_ip));
                put_inode(Some(old_dirp));
                return EINVAL;
            }
        };
        if (*glo::get_inode_ptr(new_dirp as usize)).i_nlinks == NO_LINK {
            put_inode(Some(old_ip));
            put_inode(Some(old_dirp));
            put_inode(Some(new_dirp));
            return ENOENT;
        }

        let new_ip = advance(new_dirp, new_name, IGN_PERM);
        if (*glo::mfs_ptr()).err_code == EENTERMOUNT {
            if let Some(ip) = new_ip {
                put_inode(Some(ip));
            }
            r = EBUSY;
        }

        let odir = ((*glo::get_inode_ptr(old_ip as usize)).i_mode & I_TYPE) == I_DIRECTORY;
        let same_pdir = old_dirp == new_dirp;

        if r == OK {
            if old_name == b"." || old_name == b".." || new_name == b"." || new_name == b".." {
                r = EINVAL;
            }
            if let Some(new_ip_val) = new_ip {
                if old_ip == new_ip_val {
                    r = SAME;
                }
                let ndir =
                    ((*glo::get_inode_ptr(new_ip_val as usize)).i_mode & I_TYPE) == I_DIRECTORY;
                if odir && !ndir {
                    r = ENOTDIR;
                }
                if !odir && ndir {
                    r = EISDIR;
                }
            } else if odir
                && !same_pdir
                && (*glo::get_inode_ptr(new_dirp as usize)).i_nlinks >= LINK_MAX
            {
                r = EMLINK;
            }
        }

        if r == OK {
            if let Some(new_ip_val) = new_ip {
                if odir {
                    r = remove_dir(new_dirp, new_ip_val, new_name);
                } else {
                    r = unlink_file(new_dirp, Some(new_ip_val), new_name);
                }
            }
        }

        if r == OK {
            let mut numb = (*glo::get_inode_ptr(old_ip as usize)).i_num;
            if same_pdir {
                r = search_dir(old_dirp, old_name, None, DELETE, IGN_PERM);
                if r == OK {
                    r = search_dir(old_dirp, new_name, Some(&mut numb), ENTER, IGN_PERM);
                }
            } else {
                r = search_dir(new_dirp, new_name, Some(&mut numb), ENTER, IGN_PERM);
                if r == OK {
                    r = search_dir(old_dirp, old_name, None, DELETE, IGN_PERM);
                }
            }
        }

        if r == OK && odir && !same_pdir {
            let mut new_inum = (*glo::get_inode_ptr(new_dirp as usize)).i_num;
            let _ = unlink_file(old_ip, None, &DOT2);
            if search_dir(old_ip, &DOT2, Some(&mut new_inum), ENTER, IGN_PERM) == OK {
                let ndp = &mut *glo::get_inode_ptr(new_dirp as usize);
                (*ndp).i_nlinks = (*ndp).i_nlinks.saturating_add(1);
                (*ndp).i_dirt = IN_DIRTY;
            }
        }

        put_inode(Some(old_dirp));
        put_inode(Some(old_ip));
        put_inode(Some(new_dirp));
        if let Some(nip) = new_ip {
            put_inode(Some(nip));
        }

        if r == SAME { OK } else { r }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
        }
    }

    #[test]
    fn test_fs_link_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_link(), EINVAL);
    }

    #[test]
    fn test_fs_unlink_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_unlink(), EINVAL);
    }

    #[test]
    fn test_fs_rdlink_returns_einval_when_no_inode() {
        init();
        assert_eq!(fs_rdlink(), EINVAL);
    }

    #[test]
    fn test_fs_rename_returns_einval_when_no_inode() {
        init();
        // No inodes loaded, so rename returns EINVAL
        assert_eq!(fs_rename(), EINVAL);
    }
}
