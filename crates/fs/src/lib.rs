//! Filesystem crates (MFS, ext2, procfs, iso9660fs, vbfs, pfs).

#![no_std]

extern crate alloc;

pub mod block_io;
pub mod ext2;
pub mod iso9660;
pub mod mfs;
pub mod pfs;
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

    // ── PFS tests ──
    mod pfs {
        use crate::pfs::consts::*;

        #[test]
        fn pfs_ok_is_zero() {
            assert_eq!(OK, 0);
        }

        #[test]
        fn pfs_errno_values_are_distinct() {
            let errnos = [EINVAL, EPERM, ENOSPC, EFBIG, ENFILE, EIO, EBUSY, ENOSYS];
            for i in 0..errnos.len() {
                for j in (i + 1)..errnos.len() {
                    assert_ne!(
                        errnos[i], errnos[j],
                        "duplicate PFS errno {} at positions {} and {}",
                        errnos[i], i, j,
                    );
                }
            }
        }

        #[test]
        fn pfs_nr_inodes_is_512() {
            assert_eq!(PFS_NR_INODES, 512);
        }

        #[test]
        fn pfs_pipe_buf_is_4096() {
            assert_eq!(PIPE_BUF, 4096);
        }

        #[test]
        fn pfs_inode_hash_constants() {
            assert_eq!(INODE_HASH_SIZE, 128);
            assert_eq!(INODE_HASH_MASK, 127);
        }

        #[test]
        fn pfs_no_bit_is_zero() {
            assert_eq!(NO_BIT, 0);
        }

        #[test]
        fn pfs_fs_base() {
            assert_eq!(FS_BASE, 0xA00);
        }
    }
}
