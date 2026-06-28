//! Filesystem crates (MFS, ext2, procfs, iso9660fs, vbfs, pfs).

#![no_std]

pub mod mfs;
pub mod procfs;
pub mod vbfs;

#[cfg(test)]
mod tests {
    use crate::mfs::consts::*;

    #[test]
    fn ok_is_zero() {
        assert_eq!(OK, 0);
    }

    #[test]
    fn errno_values_are_distinct() {
        let errnos = [
            EINVAL,
            EPERM,
            ENOSPC,
            EROFS,
            ENOENT,
            EFBIG,
            EACCES,
            ENOTDIR,
            EBUSY,
            EIO,
            EENTERMOUNT,
            ELEAVEMOUNT,
        ];
        for i in 0..errnos.len() {
            for j in (i + 1)..errnos.len() {
                assert_ne!(
                    errnos[i], errnos[j],
                    "duplicate errno {} at positions {} and {}",
                    errnos[i], i, j,
                );
            }
        }
    }

    #[test]
    fn super_magic_values_are_distinct() {
        assert_ne!(SUPER_MAGIC, SUPER_V2);
        assert_ne!(SUPER_MAGIC, SUPER_V3);
        assert_ne!(SUPER_V2, SUPER_V3);
    }

    #[test]
    fn root_inode_is_one() {
        assert_eq!(ROOT_INODE, 1);
    }

    #[test]
    fn zone_constants_are_sentinels() {
        assert_eq!(NO_ZONE, 0xFFFFFFFF);
        assert_eq!(NO_BLOCK, 0xFFFFFFFF);
        assert_eq!(NO_BIT, 0);
    }
}
