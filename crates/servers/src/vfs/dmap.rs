//! Device driver mapping table management.
//!
//! Adapted from `minix/servers/vfs/dmap.c` and `minix/servers/vfs/dmap.h`.
//!
//! The dmap table maps major device numbers to driver process endpoints.
//! It is indexed by major device number and provides the link between
//! device nodes in the filesystem and the device driver processes that
//! handle I/O for them.

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::types::*;

use core::ptr::addr_of_mut;

// Dmap entry locking

/// Lock a dmap entry.
///
/// NOTE: Unused in current code — all dmap operations use the caller's
/// worker thread context and don't need explicit locking yet.
/// When wired: needs to suspend the current worker thread and acquire
/// the per-entry `dmap_lock` mutex, then resume on unlock.
/// See `minix/servers/vfs/dmap.c` lines 27-45.
pub fn lock_dmap(_dp: *mut Dmap) {
    todo!("lock_dmap: needs worker_suspend + mutex_lock; unused in current code")
}

/// Unlock a dmap entry.
pub fn unlock_dmap(_dp: *mut Dmap) {
    todo!("unlock_dmap: needs mutex_unlock + worker_resume; unused in current code")
}

// Initialisation

/// Initialize the device mapping table.
///
/// Sets every entry's `dmap_driver` to `NONE` and `dmap_ep` to `NONE`.
///
/// # Safety
///
/// Must be called exactly once during VFS initialisation.
pub unsafe fn init_dmap() {
    let glob = vfs_global();
    let dmap_arr = addr_of_mut!((*glob).dmap) as *mut Dmap;
    for i in 0..NR_DEVICES {
        let dp = &mut *dmap_arr.add(i);
        dp.dmap_driver = -1; // NONE
        dp.dmap_ep = -1; // NONE
        dp.dmap_style = 0;
        dp.dmap_label = [0u8; LABEL_MAX];
    }
}

/// Map a driver to a device slot.
///
/// Sets the dmap entry for `major` to the given endpoint and label.
/// Returns OK on success, or EINVAL if `major` is out of range.
///
/// # Safety
///
/// Requires exclusive access to the dmap table.
pub unsafe fn map_driver(label: &[u8], major: i32, endpoint: i32) -> i32 {
    if major < 0 || major as usize >= NR_DEVICES {
        return EINVAL;
    }
    let glob = vfs_global();
    let dmap_arr = addr_of_mut!((*glob).dmap) as *mut Dmap;
    let dp = &mut *dmap_arr.add(major as usize);
    dp.dmap_driver = endpoint;
    dp.dmap_ep = endpoint;
    // Copy label (up to LABEL_MAX - 1, null-terminated)
    let copy_len = label.len().min(LABEL_MAX - 1);
    dp.dmap_label[..copy_len].copy_from_slice(&label[..copy_len]);
    dp.dmap_label[copy_len] = 0;
    OK
}

// Lookup / matching

/// Check if a driver endpoint matches a major device number.
pub fn dmap_driver_match(proc_e: i32, major: i32) -> i32 {
    if major < 0 || major as usize >= NR_DEVICES {
        return 0;
    }
    unsafe {
        let glob = vfs_global();
        let dmap_arr = addr_of_mut!((*glob).dmap) as *mut Dmap;
        let dp = &*dmap_arr.add(major as usize);
        if dp.dmap_driver == proc_e && dp.dmap_driver != -1 {
            1
        } else {
            0
        }
    }
}

/// A driver endpoint has come up.
///
/// Called when a device driver restarts. Iterates dmap entries
/// matching `proc_nr` and initiates recovery:
/// - Block drivers: call `bdev_up(major)` to re-open devices
/// - Char drivers: call `invalidate_filp_by_char_major(major)`
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/dmap.c` (dmap_endpt_up)
pub fn dmap_endpt_up(proc_nr: i32, is_blk: i32) {
    let _ = (proc_nr, is_blk);
    todo!("dmap_endpt_up: needs bdev_up / invalidate_filp; see PORTING_PLAN.md 10.4b")
}

/// Get the dmap entry for a driver endpoint.
///
/// Searches the dmap table linearly for an entry whose `dmap_ep`
/// equals `proc_e`. Returns a pointer to the entry, or null.
pub fn get_dmap(proc_e: i32) -> *mut Dmap {
    unsafe {
        let glob = vfs_global();
        let dmap_arr = addr_of_mut!((*glob).dmap) as *mut Dmap;
        for i in 0..NR_DEVICES {
            let dp = &*dmap_arr.add(i);
            if dp.dmap_ep == proc_e {
                return dmap_arr.add(i);
            }
        }
    }
    core::ptr::null_mut()
}

/// Get the dmap entry by major device number.
///
/// Returns a pointer to the dmap entry for `major`, or null if
/// out of range or the entry has no driver.
pub fn get_dmap_by_major(major: i32) -> *mut Dmap {
    if major < 0 || major as usize >= NR_DEVICES {
        return core::ptr::null_mut();
    }
    unsafe {
        let glob = vfs_global();
        let dmap_arr = addr_of_mut!((*glob).dmap) as *mut Dmap;
        let dp = &*dmap_arr.add(major as usize);
        if dp.dmap_driver == -1 {
            return core::ptr::null_mut();
        }
        dmap_arr.add(major as usize)
    }
}

/// Find a dmap entry by device label.
///
/// Scans the dmap table for an entry whose label matches `label`.
/// Returns the major device number, or -1 if not found.
pub fn find_dmap_by_label(label: &[u8]) -> i32 {
    unsafe {
        let glob = vfs_global();
        let dmap_arr = addr_of_mut!((*glob).dmap) as *mut Dmap;
        for i in 0..NR_DEVICES {
            let dp = &*dmap_arr.add(i);
            if dp.dmap_driver == -1 {
                continue;
            }
            // Compare labels (up to LABEL_MAX)
            let dlabel_len = dp
                .dmap_label
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(LABEL_MAX);
            let dlabel = &dp.dmap_label[..dlabel_len];
            if dlabel == label {
                return i as i32;
            }
        }
    }
    -1
}

// Unmapping

/// Unmap all dmap entries for a given endpoint.
///
/// Iterates the dmap table and resets every entry whose `dmap_driver`
/// matches `proc_nr`. Used when a driver process exits.
pub fn dmap_unmap_by_endpt(proc_nr: i32) {
    unsafe {
        let glob = vfs_global();
        let dmap_arr = core::ptr::addr_of_mut!((*glob).dmap) as *mut Dmap;
        for i in 0..NR_DEVICES {
            let dp = &mut *dmap_arr.add(i);
            if dp.dmap_driver == proc_nr {
                dp.dmap_driver = -1; // NONE
                dp.dmap_ep = -1;
                dp.dmap_style = 0;
                dp.dmap_label = [0u8; LABEL_MAX];
            }
        }
    }
}

// Service/driver registration

/// Map a service to a device (called by RS).
///
/// When a new system service starts, RS calls this to register its
/// device mapping. Reads the RS public entry (`RprocPub`) and calls
/// `map_driver` if the service publishes a device number.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/dmap.c` (map_service)
pub fn map_service(rpub: *const core::ffi::c_void) -> i32 {
    if rpub.is_null() {
        return EINVAL;
    }
    let rpub = rpub as *const crate::rs::RprocPub;
    unsafe {
        let dev_nr = (*rpub).dev_nr;
        if dev_nr < 0 {
            // No device number — not a driver, nothing to map.
            return OK;
        }
        let endpoint = (*rpub).endpoint;
        // Extract null-terminated label.
        let label_len = (*rpub)
            .label
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(LABEL_MAX - 1);
        let label = core::slice::from_raw_parts((*rpub).label.as_ptr(), label_len);
        map_driver(label, dev_nr, endpoint)
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn init() {
        init_dmap();
    }

    #[test]
    fn test_init_dmap_clears_all_entries() {
        unsafe {
            init();
            let glob = vfs_global();
            let dmap_arr = addr_of_mut!((*glob).dmap) as *mut Dmap;
            for i in 0..NR_DEVICES {
                assert_eq!((*dmap_arr.add(i)).dmap_driver, -1);
                assert_eq!((*dmap_arr.add(i)).dmap_ep, -1);
            }
        }
    }

    #[test]
    fn test_map_driver_sets_entry() {
        unsafe {
            init();
            let label = b"ext2";
            assert_eq!(map_driver(label, 2, 100), OK);
            let dp = get_dmap_by_major(2);
            assert!(!dp.is_null());
            assert_eq!((*dp).dmap_ep, 100);
            assert_eq!((*dp).dmap_driver, 100);
        }
    }

    #[test]
    fn test_map_driver_out_of_range() {
        unsafe {
            init();
            assert_eq!(map_driver(b"test", 999, 1), EINVAL);
            assert_eq!(map_driver(b"test", -1, 1), EINVAL);
        }
    }

    #[test]
    fn test_get_dmap_by_major_returns_null_for_unmapped() {
        unsafe {
            init();
            assert!(get_dmap_by_major(5).is_null());
        }
    }

    #[test]
    fn test_get_dmap_finds_by_endpoint() {
        unsafe {
            init();
            assert!(get_dmap(42).is_null());
            map_driver(b"mfs", 3, 42);
            let dp = get_dmap(42);
            assert!(!dp.is_null());
            assert_eq!((*dp).dmap_ep, 42);
        }
    }

    #[test]
    fn test_dmap_driver_match_checks_major() {
        unsafe {
            init();
            map_driver(b"pfs", 7, 77);
            assert_eq!(dmap_driver_match(77, 7), 1);
            assert_eq!(dmap_driver_match(77, 8), 0);
            assert_eq!(dmap_driver_match(99, 7), 0);
        }
    }

    #[test]
    fn test_find_dmap_by_label() {
        unsafe {
            init();
            assert_eq!(find_dmap_by_label(b"ext2"), -1);
            map_driver(b"ext2", 2, 100);
            assert_eq!(find_dmap_by_label(b"ext2"), 2);
            assert_eq!(find_dmap_by_label(b"mfs"), -1);
        }
    }

    #[test]
    fn test_dmap_unmap_by_endpt_clears_all_matching() {
        unsafe {
            init();
            map_driver(b"ext2", 2, 100);
            map_driver(b"mfs", 3, 100);
            map_driver(b"pfs", 4, 200);
            assert!(!get_dmap_by_major(2).is_null());
            assert!(!get_dmap_by_major(3).is_null());
            assert!(!get_dmap_by_major(4).is_null());

            dmap_unmap_by_endpt(100);

            // ext2 and mfs were mapped to endpoint 100 — should be cleared
            assert!(get_dmap_by_major(2).is_null(), "ext2 should be unmapped");
            assert!(get_dmap_by_major(3).is_null(), "mfs should be unmapped");
            // pfs was mapped to 200 — should remain
            assert!(!get_dmap_by_major(4).is_null(), "pfs should remain");
        }
    }
}
