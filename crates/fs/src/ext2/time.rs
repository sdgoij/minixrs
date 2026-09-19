//! utime — adapted from `minix/fs/ext2/time.c`

use crate::ext2::consts::*;
use crate::ext2::glo;
use crate::ext2::inode::*;
use crate::ext2::protect::read_only;
use crate::ext2::utility::*;

/// fs_utime — set the access and/or modification time of an inode.
///
/// Message layout (VFS `req_utime`): inode (u32) at payload[0], actime (i64) at
/// payload[8], modtime (i64) at payload[16], acnsec (i32) at payload[24],
/// modnsec (i32) at payload[28]. A nanosecond field of `UTIME_NOW` means "the
/// time of this call" and `UTIME_OMIT` means "leave this one alone"; any other
/// value is an explicit time. ext2 stores whole seconds, so the rounding down
/// is the caller's `actime`/`modtime`.
///
/// VFS has already checked that the caller owns the file or is the super user.
///
/// Reference: time.c fs_utime()
pub unsafe fn fs_utime() -> i32 {
    let ext2 = glo::ext2_ptr();
    let raw = (*ext2).m_in.m_payload.raw;

    // Temporarily open the file.
    let rip = get_inode((*ext2).fs_dev, payload_u32(&raw, 0));
    if rip.is_null() {
        return EINVAL;
    }

    // Not even the super user may touch an inode on a read-only volume.
    let mut r = read_only(rip);

    if r == OK {
        // Recomputing the times from scratch discards any stale ATIME/MTIME
        // flags the inode carried.
        (*rip).i_update = CTIME;

        match payload_i32(&raw, 24) {
            n if n == UTIME_NOW as i32 => (*rip).i_update |= ATIME,
            n if n == UTIME_OMIT as i32 => {}
            _ => (*rip).i_atime = payload_i64(&raw, 8) as u32,
        }

        match payload_i32(&raw, 28) {
            n if n == UTIME_NOW as i32 => (*rip).i_update |= MTIME,
            n if n == UTIME_OMIT as i32 => {}
            _ => (*rip).i_mtime = payload_i64(&raw, 16) as u32,
        }

        (*rip).i_dirt = IN_DIRTY;
        r = OK;
    }

    put_inode(rip);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utime_mode_constants_are_negative_sentinels() {
        // VFS sends these as the wire value of the nanosecond field, so they
        // have to survive the narrowing to i32.
        assert_eq!(UTIME_NOW as i32, -1);
        assert_eq!(UTIME_OMIT as i32, -2);
    }
}
