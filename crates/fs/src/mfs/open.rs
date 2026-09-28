//! File creation ops — adapted from `minix/fs/mfs/open.c`

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::path::*;
#[cfg(target_os = "minix")]
use crate::mfs::read::safecopy_from_grant;
use crate::mfs::super_block::get_block_size;
use crate::mfs::write::*;
use libs::libminixfs::cache::lmfs_put_block;

/// Internal: create a new inode under `ldirp_idx` with name `string` and mode `bits`.
unsafe fn new_node(ldirp_idx: u16, string: &[u8], bits: u16, z0: u32) -> Option<u16> {
    let ldirp = &*glo::get_inode_ptr(ldirp_idx as usize);

    if (*ldirp).i_nlinks == NO_LINK {
        (*glo::mfs_ptr()).err_code = ENOENT;
        return None;
    }

    let rip = advance(ldirp_idx, string, IGN_PERM);

    if bits & I_TYPE == I_DIRECTORY && (*ldirp).i_nlinks >= LINK_MAX {
        if let Some(r) = rip {
            put_inode(Some(r));
        }
        (*glo::mfs_ptr()).err_code = EMLINK;
        return None;
    }

    if rip.is_none() && (*glo::mfs_ptr()).err_code == ENOENT {
        let new_rip = alloc_inode((*ldirp).i_dev, bits)?;

        {
            let rp = &mut *glo::get_inode_ptr(new_rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_add(1);
            (*rp).i_zone[0] = z0;
        }
        rw_inode(new_rip, WRITING);

        let mut inum = (*glo::get_inode_ptr(new_rip as usize)).i_num;
        let r = search_dir(ldirp_idx, string, Some(&mut inum), ENTER, IGN_PERM);
        if r != OK {
            let rp = &mut *glo::get_inode_ptr(new_rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_sub(1);
            (*rp).i_dirt = IN_DIRTY;
            put_inode(Some(new_rip));
            (*glo::mfs_ptr()).err_code = r;
            return None;
        }

        (*glo::mfs_ptr()).err_code = OK;
        return Some(new_rip);
    }

    let ec = (*glo::mfs_ptr()).err_code;
    if ec == EENTERMOUNT || ec == ELEAVEMOUNT {
        (*glo::mfs_ptr()).err_code = EEXIST;
    } else {
        (*glo::mfs_ptr()).err_code = if rip.is_some() { EEXIST } else { ec };
    }
    rip
}

pub fn fs_create() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let mode = (*glo::mfs_ptr()).cch[1] as u16;
        let uid = (*glo::mfs_ptr()).cch[2] as u16;
        let gid = (*glo::mfs_ptr()).cch[3] as u16;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        (*glo::mfs_ptr()).caller_uid = uid;
        (*glo::mfs_ptr()).caller_gid = gid;

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return ENOENT,
        };

        let rip = new_node(ldirp, string, mode, NO_ZONE);
        let r = (*glo::mfs_ptr()).err_code;

        if r != OK {
            put_inode(Some(ldirp));
            if let Some(rp) = rip {
                put_inode(Some(rp));
            }
            return r;
        }

        if let Some(rp) = rip {
            let rip_ref = &*glo::get_inode_ptr(rp as usize);
            (*glo::mfs_ptr()).cch[0] = (*rip_ref).i_num as i32;
            (*glo::mfs_ptr()).cch[1] = (*rip_ref).i_mode as i32;
            (*glo::mfs_ptr()).cch[2] = (*rip_ref).i_size;
            (*glo::mfs_ptr()).cch[3] = (*rip_ref).i_uid as i32;
            (*glo::mfs_ptr()).cch[4] = (*rip_ref).i_gid as i32;
            put_inode(Some(rp));
        }

        put_inode(Some(ldirp));
        if rip.is_some() {
            OK
        } else {
            (*glo::mfs_ptr()).err_code
        }
    }
}

pub fn fs_mkdir() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let mode = (*glo::mfs_ptr()).cch[1] as u16;
        let uid = (*glo::mfs_ptr()).cch[2] as u16;
        let gid = (*glo::mfs_ptr()).cch[3] as u16;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        (*glo::mfs_ptr()).caller_uid = uid;
        (*glo::mfs_ptr()).caller_gid = gid;

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return ENOENT,
        };

        let rip = new_node(ldirp, string, mode | I_DIRECTORY, NO_ZONE);

        if rip.is_none() || (*glo::mfs_ptr()).err_code == EEXIST {
            if let Some(r) = rip {
                put_inode(Some(r));
            }
            let ec = (*glo::mfs_ptr()).err_code;
            put_inode(Some(ldirp));
            return ec;
        }

        let rip = rip.unwrap();
        let dotdot = (*glo::get_inode_ptr(ldirp as usize)).i_num;
        let dot = (*glo::get_inode_ptr(rip as usize)).i_num;

        {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_mode = mode | I_DIRECTORY;
        }

        let mut dot_mut = dot;
        let mut dotdot_mut = dotdot;
        let r1 = search_dir(rip, &DOT1, Some(&mut dot_mut), ENTER, IGN_PERM);
        let r2 = search_dir(rip, &DOT2, Some(&mut dotdot_mut), ENTER, IGN_PERM);

        if r1 == OK && r2 == OK {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_add(1);
            let lp = &mut *glo::get_inode_ptr(ldirp as usize);
            (*lp).i_nlinks = (*lp).i_nlinks.saturating_add(1);
            (*lp).i_dirt = IN_DIRTY;
        } else {
            let _ = search_dir(ldirp, string, None, DELETE, IGN_PERM);
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_nlinks = (*rp).i_nlinks.saturating_sub(1);
        }

        {
            let rp = &mut *glo::get_inode_ptr(rip as usize);
            (*rp).i_dirt = IN_DIRTY;
        }

        let ec = (*glo::mfs_ptr()).err_code;
        put_inode(Some(ldirp));
        put_inode(Some(rip));
        ec
    }
}

pub fn fs_mknod() -> i32 {
    unsafe {
        let dir_ino = (*glo::mfs_ptr()).cch[0] as u32;
        let mode = (*glo::mfs_ptr()).cch[1] as u16;
        let device = (*glo::mfs_ptr()).cch[2] as u32;
        let uid = (*glo::mfs_ptr()).cch[3] as u16;
        let gid = (*glo::mfs_ptr()).cch[4] as u16;
        let dev = (*glo::mfs_ptr()).fs_dev;
        let user_path = &(*glo::mfs_ptr()).user_path;
        let len = user_path
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(user_path.len());
        let string = &user_path[..len];

        (*glo::mfs_ptr()).caller_uid = uid;
        (*glo::mfs_ptr()).caller_gid = gid;

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return ENOENT,
        };

        let ip = new_node(ldirp, string, mode, device);
        let ec = (*glo::mfs_ptr()).err_code;
        if let Some(ip_val) = ip {
            put_inode(Some(ip_val));
        }
        put_inode(Some(ldirp));
        ec
    }
}

/// Create a symbolic link.
///
/// Message layout (VFS `req_slink`): inode at payload[0], name length at payload[8],
/// target length at payload[16], name grant at payload[24], target grant at payload[28],
/// uid at payload[32], gid at payload[34]. Both strings arrive through grants, which is
/// the only way a caller's bytes reach a filesystem server.
///
/// The target goes in the inode's first block and `i_size` is its length — the form
/// `fs_rdlink` reads back. The count is the caller's, so a target whose bytes hold a NUL
/// is refused rather than stored shorter than it was said to be.
///
/// Reference: open.c fs_slink()
pub fn fs_slink() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        let raw = (*mfs).m_in.m_payload.raw;
        let rd_u16 = |off: usize| {
            u16::from_le_bytes(
                raw.get(off..off + 2)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or([0u8; 2]),
            )
        };
        let rd_i32 = |off: usize| {
            i32::from_le_bytes(
                raw.get(off..off + 4)
                    .and_then(|s| s.try_into().ok())
                    .unwrap_or([0u8; 4]),
            )
        };
        let rd_u32 = |off: usize| {
            u32::from_le_bytes(
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

        let dir_ino = rd_u32(0);
        let path_len = rd_u64(8) as usize;
        let mem_size = rd_u64(16) as usize;
        let grant_path = rd_i32(24);
        let grant_target = rd_i32(28);
        let dev = (*mfs).fs_dev;

        (*mfs).caller_uid = rd_u16(32);
        (*mfs).caller_gid = rd_u16(34);

        // The name is the link's own last component: VFS resolved its directory already
        // and sends the name NUL-terminated, with that NUL counted (C reads
        // `min(path_len, sizeof(string))` and NUL-pads what is left over).
        let len = path_len.min(MFS_NAME_MAX);
        let mut string = [0u8; MFS_NAME_MAX];
        #[cfg(target_os = "minix")]
        let r_name = safecopy_from_grant(grant_path, 0, string.as_mut_ptr(), len);
        #[cfg(not(target_os = "minix"))]
        let r_name = {
            // No kernel to copy through on the host, so the name arrives empty and a
            // non-empty target is refused rather than stored as garbage — the same split
            // ext2's fs_slink takes, for the same reason.
            let _ = (grant_path, grant_target, len);
            if mem_size > 0 { EIO } else { OK }
        };
        if r_name != OK {
            return r_name;
        }
        if len >= MFS_NAME_MAX {
            // C `NUL(string, len, sizeof(string))`: a name that filled the buffer is
            // truncated rather than left unterminated. A shorter one is already
            // terminated by the zero the buffer started with.
            string[MFS_NAME_MAX - 1] = 0;
        }
        let name_len = string.iter().position(|&c| c == 0).unwrap_or(MFS_NAME_MAX);
        let name = &string[..name_len];

        let ldirp = match get_inode(dev, dir_ino) {
            Some(d) => d,
            None => return EINVAL,
        };

        let sip = new_node(ldirp, name, I_SYMBOLIC_LINK | RWX_MODES, 0);
        let mut r = (*mfs).err_code;

        if r == OK {
            if let Some(sip_idx) = sip {
                let block_size = get_block_size(dev) as usize;
                let bp = new_block(sip_idx, 0) as *mut libs::libminixfs::types::Buf;
                if bp.is_null() {
                    r = (*mfs).err_code;
                    if r == OK {
                        r = EIO;
                    }
                } else if block_size <= mem_size {
                    // The block has to hold the target *and* the NUL written after it.
                    r = ENAMETOOLONG;
                    lmfs_put_block(bp, DIRECTORY_BLOCK);
                } else {
                    #[cfg(target_os = "minix")]
                    let r_copy = safecopy_from_grant(grant_target, 0, (*bp).data_ptr, mem_size);
                    // The host cannot copy, and the length guard above already refused
                    // every non-empty target, so there is nothing left to store.
                    #[cfg(not(target_os = "minix"))]
                    let r_copy = OK;
                    r = r_copy;
                    if r == OK {
                        // C calls `strlen()` on what it copied and refuses a target that
                        // reads back shorter than the count it was given, because a NUL
                        // inside the caller's buffer would make the link quietly shorter
                        // than the count says.
                        let data = (*bp).data_ptr;
                        let full = (0..mem_size)
                            .position(|i| *data.add(i) == 0)
                            .unwrap_or(mem_size);
                        if full != mem_size {
                            r = ENAMETOOLONG;
                        } else {
                            *data.add(mem_size) = 0;
                            (*glo::get_inode_ptr(sip_idx as usize)).i_size = mem_size as i32;
                        }
                    }
                    lmfs_put_block(bp, DIRECTORY_BLOCK);
                }

                if r != OK {
                    // C `open.c` fs_slink undoes the inode and its entry together, and
                    // only here, where `new_node` made both: a name that was already
                    // there comes back as `EEXIST` with `sip` naming the *existing*
                    // inode, which this rollback must not touch.
                    (*glo::get_inode_ptr(sip_idx as usize)).i_nlinks = NO_LINK;
                    let _ = search_dir(ldirp, name, None, DELETE, IGN_PERM);
                }
            }
        }

        if let Some(sip_idx) = sip {
            put_inode(Some(sip_idx));
        }
        put_inode(Some(ldirp));
        r
    }
}

pub fn fs_inhibread() -> i32 {
    EINVAL
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
    fn test_fs_inhibread_returns_einval_when_uninitialized() {
        init();
        assert_eq!(fs_inhibread(), EINVAL);
    }

    #[test]
    fn test_fs_create_returns_enoent_when_no_dev() {
        init();
        assert_eq!(fs_create(), ENOENT);
    }

    #[test]
    fn test_fs_mkdir_returns_enoent_when_no_dev() {
        init();
        assert_eq!(fs_mkdir(), ENOENT);
    }

    #[test]
    fn test_fs_mknod_returns_enoent_when_no_dev() {
        init();
        assert_eq!(fs_mknod(), ENOENT);
    }

    #[test]
    fn test_fs_slink_returns_einval_when_no_dev() {
        init();
        assert_eq!(fs_slink(), EINVAL);
    }

    /// The host has no kernel to copy through, so it cannot store a target: a non-empty
    /// one is refused rather than written as whatever the message's grant id happens to
    /// name. Nothing else here reaches that branch — the test above only gets as far as
    /// the absent device because its target is empty.
    #[test]
    fn test_fs_slink_refuses_a_target_on_the_host() {
        init();
        unsafe {
            let raw = &mut (*glo::mfs_ptr()).m_in.m_payload.raw;
            raw[16..24].copy_from_slice(&4u64.to_le_bytes());
        }
        assert_eq!(fs_slink(), EIO);
    }
}
