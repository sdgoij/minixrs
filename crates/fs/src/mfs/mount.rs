//! Mount/unmount operations — adapted from `minix/fs/mfs/mount.c`

use core::sync::atomic::{AtomicI32, Ordering};

use crate::mfs::consts::*;
use crate::mfs::glo;
use crate::mfs::inode::*;
use crate::mfs::misc::fs_sync;
use crate::mfs::super_block::*;
use crate::mfs::types::*;

static CLEANMOUNT: AtomicI32 = AtomicI32::new(1);

/// Record a mount's flags on the superblock and decide whether it may write
/// (C `fs_readsuper`): a filesystem that was not unmounted cleanly is dropped
/// to read-only rather than written on top of, and VFS's `REQ_RDONLY`/
/// `REQ_ISROOT` are stored for the requests that read them. Returns the
/// effective read-only state.
fn apply_mount_flags(sp: &mut SuperBlock, req_flags: u32) -> bool {
    let mut readonly = (req_flags & REQ_RDONLY as u32) != 0;
    let isroot = (req_flags & REQ_ISROOT as u32) != 0;
    if sp.s_flags & MFSFLAG_CLEAN == 0 && !readonly {
        readonly = true;
    }
    sp.s_rd_only = readonly as i32;
    sp.s_is_root = isroot as u8;
    readonly
}

/// Say that a mount came out read-only because the filesystem was not unmounted cleanly.
///
/// The mount *succeeds*: reads work, the system comes up, and the first thing that reports the state
/// is a write failing somewhere much later — which reads as a broken command rather than as a disk
/// this boot may not write. Dropping to read-only is the reference's answer and it stays; saying so
/// is this port's, because nothing else does (`PORTING_PLAN.md` finding 54, and the reason the page
/// carries a control for a disk it cannot write).
///
/// The read-only state is also *not* visible to VFS, which asked for a writable mount and is not
/// told otherwise; that is the reference's protocol shape as well (the `fs_readsuper` reply carries
/// the root inode, not the effective flags) and it is why the line is the signal rather than
/// something a caller could ask for.
fn report_unclean_mount() {
    #[cfg(target_os = "minix")]
    {
        let msg = b"mfs: the filesystem was not unmounted cleanly, mounted read-only\n";
        unsafe { minix_rt::write(1, msg.as_ptr(), msg.len()) };
    }
}

pub fn fs_readsuper() -> i32 {
    unsafe {
        // Read device number from the incoming message (m1i1).
        // VFS's req_readsuper writes dev at PAYLOAD_OFF + 0 → m1.m1i1.
        let mfs = glo::mfs_ptr();
        let dev = (*mfs).m_in.m_payload.m1.m1i1 as u32;

        // Mount flags: `REQ_RDONLY` says the filesystem may not be written, and
        // `REQ_ISROOT` marks the process root (where `..` stops).
        let req_flags = u32::from_ne_bytes(
            (&(*mfs).m_in.m_payload.raw)[4..8]
                .try_into()
                .unwrap_or([0u8; 4]),
        );

        // Resolve the block driver for this device from the driver label
        // VFS grants (label_len at payload+8, label grant at payload+24,
        // granter = m_source). The resolved endpoint routes all block I/O
        // for `dev` (libbdev bdev_driver).
        #[cfg(target_os = "minix")]
        {
            let label_len = (*mfs).m_in.m_payload.m1.m1i3 as usize;
            let label_grant = i32::from_ne_bytes(
                (&(*mfs).m_in.m_payload.raw)[24..28]
                    .try_into()
                    .unwrap_or([0u8; 4]),
            );
            if label_len > 0 && label_len < crate::mfs::consts::LABEL_MAX {
                let mut label = [0u8; crate::mfs::consts::LABEL_MAX];
                let r = crate::block_io::safecopy_from(
                    (*mfs).m_in.m_source,
                    label_grant,
                    &mut label[..label_len],
                );
                if r == 0 {
                    // Prefers the granted label, but re-registers `dev` to the
                    // ramdisk driver when that driver has no device - a
                    // diskless boot's root is the embedded filesystem image.
                    let _ = crate::block_io::bdev_driver_root(dev, &label[..label_len]);
                }
            }
        }

        // fs_readsuper called
        for i in 0..8 {
            let sp = glo::get_super_ptr(i);
            if (*sp).s_dev == NO_DEV {
                (*sp).s_dev = dev;
                let r = read_super(&mut *sp);
                if r != OK {
                    (*sp).s_dev = NO_DEV;
                    return r;
                }
                if (*sp).s_flags & MFSFLAG_CLEAN != 0 {
                    CLEANMOUNT.store(1, Ordering::Relaxed);
                }
                let readonly = apply_mount_flags(&mut *sp, req_flags);
                if readonly && (req_flags & REQ_RDONLY as u32) == 0 {
                    // Read-only for the only reason `apply_mount_flags` can produce it without being
                    // asked: an unclean filesystem. Say it before the mount is handed back, so a
                    // front end's log holds it ahead of any write that will fail.
                    report_unclean_mount();
                }
                if !readonly {
                    // Mark it in use, so the next mount sees it was not unmounted
                    // cleanly. `fs_unmount` turns the flag back on.
                    (*sp).s_flags &= !MFSFLAG_CLEAN;
                    if write_super(&mut *sp) != OK {
                        (*sp).s_dev = NO_DEV;
                        return EIO;
                    }
                }
                let root_rip = match get_inode(dev, ROOT_INODE) {
                    Some(rip) => rip,
                    None => {
                        (*sp).s_dev = NO_DEV;
                        return EINVAL;
                    }
                };

                // Fill reply payload with root inode info for VFS.
                // VFS req_readsuper expects reply fields at:
                //   file_size (i64) at PAYLOAD_OFF+0 → m1.m1i1 (low) + m1i2 (high)
                //   dev       (u32) at PAYLOAD_OFF+8 → m1.m1i3
                //   inode_nr  (u32) at PAYLOAD_OFF+12 → m1.m1i4
                //   flags     (u32) at PAYLOAD_OFF+16 → m1.m1i5
                //   mode (u16) at PAYLOAD_OFF+20 → low 16 of m1i6
                let root_inode = &*glo::get_inode_ptr(root_rip as usize);
                (*mfs).fs_dev = dev;
                (*mfs).m_out.m_payload.m1.m1i1 = root_inode.i_size;
                (*mfs).m_out.m_payload.m1.m1i2 = if root_inode.i_size < 0 { -1 } else { 0 };
                (*mfs).m_out.m_payload.m1.m1i3 = dev as i32;
                (*mfs).m_out.m_payload.m1.m1i4 = ROOT_INODE as i32;
                (*mfs).m_out.m_payload.m1.m1i5 = 0; // flags: no extension flags
                (*mfs).m_out.m_payload.m1.m1i6 = root_inode.i_mode as i32;

                return OK;
            }
        }
        EINVAL
    }
}

pub fn fs_unmount() -> i32 {
    unsafe {
        let mfs = glo::mfs_ptr();
        if (*mfs).super_blocks[0].s_dev != (*mfs).fs_dev {
            return EINVAL;
        }

        // VFS's forced pass (the shutdown's) says the filesystem must come off, so
        // the tests below are skipped. The reason they exist — an unmount happening
        // under an open file — is VFS's to check and it already has (it refuses to
        // unmount a device with an open file description on it), and this port's
        // caches do not hold references that add up the way the C reference expects:
        // several paths take an FS reference VFS does not later put back
        // (`PORTING_PLAN.md` has the measurements), so a strict count here would
        // refuse the shutdown's unmount of a filesystem nothing is using.
        let forced = u32::from_ne_bytes(
            (&(*mfs).m_in.m_payload.raw)[0..4]
                .try_into()
                .unwrap_or([0u8; 4]),
        ) != 0;
        if !forced {
            let mut count = 0;
            for i in 0..NR_INODES {
                let inode = &*glo::get_inode_ptr(i);
                if (*inode).i_count > 0 && (*inode).i_dev == (*mfs).fs_dev {
                    count += (*inode).i_count;
                }
            }
            if count > 1 {
                return EBUSY;
            }
        }
        let root_ip = find_inode((*mfs).fs_dev, ROOT_INODE);
        if root_ip.is_none() {
            return EINVAL;
        }
        put_inode(root_ip);

        // Flush the inodes and cached blocks before marking the filesystem
        // clean: the clean bit is a promise about what is on the device, and a
        // promise made over a cache that still holds them is a filesystem the
        // next read-write mount would find half-written (the C reference's
        // `fs_unmount` flushes first and writes the superblock last, for this
        // reason).
        let r = fs_sync();
        if r != OK {
            return r;
        }

        let sp = &mut (*mfs).super_blocks[0];
        // Mark it clean if we're allowed to write _and_ it was clean originally.
        if CLEANMOUNT.load(Ordering::Relaxed) != 0 && sp.s_rd_only == 0 {
            sp.s_flags |= MFSFLAG_CLEAN;
            let r = write_super(sp);
            if r != OK {
                return r;
            }
            // `write_super` puts the superblock in the block cache and marks it
            // dirty; the clean bit is a promise about what is *on the device*, so it
            // goes out here rather than waiting for a flush that will never come —
            // this is the last thing the filesystem does, and the invalidate below
            // throws the cache away.
            libs::libminixfs::cache::lmfs_flushall();
        }

        // The device is no longer being written through, so nothing from this
        // filesystem may stay in the block cache: a block written back after
        // the unmount would land on a filesystem nobody is tracking any more.
        libs::libminixfs::cache::lmfs_invalidate(sp.s_dev);

        sp.s_dev = NO_DEV;
        (*mfs).unmountdone = TRUE;
        OK
    }
}

pub fn fs_mountpoint() -> i32 {
    unsafe {
        let inode_nr: u32 = 0;
        let mfs = glo::mfs_ptr();
        let rip = match get_inode((*mfs).fs_dev, inode_nr) {
            Some(idx) => idx,
            None => return EINVAL,
        };
        let inode = &*glo::get_inode_ptr(rip as usize);
        let mut r = OK;
        if (*inode).i_mountpoint != FALSE {
            r = EBUSY;
        }
        let bits = (*inode).i_mode & I_TYPE;
        if bits == I_BLOCK_SPECIAL || bits == I_CHAR_SPECIAL {
            r = ENOTDIR;
        }
        put_inode(Some(rip));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: initialize global MFS state so functions that
    /// dereference mfs_ptr / the inode table can be called.
    /// Tests in this module are NOT thread-safe (the C port is
    /// single-threaded); run with `--test-threads=1` if needed.
    fn init() {
        unsafe {
            crate::mfs::glo::mfs_init_globals();
            // Reset the inode hash table and unused list so that
            // get_inode / find_inode start from a clean slate
            // (mfs_init_globals only resets MFS_STORAGE, not these
            //  separate static mut variables).
            *crate::mfs::glo::UNUSED_INODES_HEAD.get() = None;
            let p = crate::mfs::glo::HASH_INODES.get();
            for i in 0..crate::mfs::consts::INODE_HASH_SIZE {
                let elem = core::ptr::addr_of_mut!((*p)[i]);
                elem.write(None);
            }
        }
    }

    #[test]
    fn test_fs_unmount_returns_einval_when_uninitialized() {
        // After init, no filesystem is mounted:
        //   super_blocks[0].s_dev == NO_DEV (same as fs_dev == NO_DEV),
        //   all inodes have i_count == 0,
        //   root inode is not in the hash table → EINVAL.
        init();
        assert_eq!(fs_unmount(), EINVAL);
    }

    #[test]
    fn test_fs_mountpoint_returns_einval_when_uninitialized() {
        // After init, fs_dev == NO_DEV and the inode hash table is
        // empty, so get_inode fails → EINVAL.
        init();
        assert_eq!(fs_mountpoint(), EINVAL);
    }

    #[test]
    fn apply_mount_flags_takes_the_filesystem_flags() {
        // A clean filesystem mounted read-write stays writable, and the root
        // flag is recorded for the lookup that reads it.
        let mut sp = SuperBlock {
            s_flags: MFSFLAG_CLEAN,
            ..SuperBlock::default()
        };
        assert!(!apply_mount_flags(&mut sp, REQ_ISROOT as u32));
        assert_eq!(sp.s_rd_only, 0);
        assert_eq!(sp.s_is_root, 1, "the root mount must be recorded");

        // REQ_RDONLY wins even when the filesystem is clean.
        assert!(apply_mount_flags(&mut sp, REQ_RDONLY as u32));
        assert_eq!(sp.s_rd_only, 1);

        // An unclean filesystem is mounted read-only whatever VFS asked for:
        // writing on top of whatever the crash left behind is worse than
        // refusing the writes.
        sp.s_flags = 0;
        assert!(apply_mount_flags(&mut sp, 0));
        assert_eq!(sp.s_rd_only, 1);
        assert!(apply_mount_flags(&mut sp, REQ_RDONLY as u32));
        assert_eq!(sp.s_rd_only, 1);
    }
}
