//! ISO 9660 miscellaneous operations — adapted from `minix/fs/iso9660fs/misc.c`
//!
//! Since ISO 9660 is read-only, most of these are no-ops.

use crate::iso9660::consts::*;
use crate::iso9660::glo;

/// `fs_sync()` — sync data to disk.
///
/// No-op since ISO 9660 is read-only.
///
/// # Safety
///
/// Requires exclusive access to globals (currently no-op anyway).
pub unsafe fn fs_sync() -> i32 {
    OK
}

/// `fs_flush()` — flush file data.
///
/// No-op since ISO 9660 is read-only.
///
/// # Safety
///
/// Requires exclusive access to globals (currently no-op anyway).
pub unsafe fn fs_flush() -> i32 {
    OK
}

/// `fs_new_driver()` — set a new driver endpoint for this device.
///
/// # Safety
///
/// Requires exclusive access to globals.
pub unsafe fn fs_new_driver() -> i32 {
    let isofs = glo::isofs_ptr();

    // In the real implementation:
    //   dev       = fs_m_in.m_vfs_fs_new_driver.device;
    //   label_gid = fs_m_in.m_vfs_fs_new_driver.grant;
    //   label_len = fs_m_in.m_vfs_fs_new_driver.path_len;
    //   sys_safecopyfrom(fs_m_in.m_source, label_gid, 0, label, label_len);
    //   bdev_driver(dev, label);
    let _ = isofs;

    OK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fs_sync() {
        unsafe {
            assert_eq!(fs_sync(), OK);
        }
    }

    #[test]
    fn test_fs_flush() {
        unsafe {
            assert_eq!(fs_flush(), OK);
        }
    }

    #[test]
    fn test_fs_new_driver_stub() {
        unsafe {
            let r = fs_new_driver();
            assert_eq!(r, OK);
        }
    }
}
