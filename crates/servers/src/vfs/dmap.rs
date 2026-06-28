//! Device driver mapping table management.
//!
//! Adapted from \`minix/servers/vfs/dmap.c\` and \`minix/servers/vfs/dmap.h\`.
//!
//! The dmap table maps major device numbers to driver process endpoints.
//! It is indexed by major device number and provides the link between
//! device nodes in the filesystem and the device driver processes that
//! handle I/O for them.

use crate::vfs::consts::*;
use crate::vfs::types::*;

// =============================================================================
// Dmap entry locking
// =============================================================================

/// Lock a dmap entry.
///
/// Suspends the current worker thread and acquires the per-entry mutex
/// (\`dmap_lock\`) to synchronise access to the driver slot.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`lock_dmap()\` (line 27)
///
/// # Safety
///
/// \`dp\` must point to a valid, initialised dmap entry whose
/// \`dmap_driver\` is not \`NONE\`.
///
/// # TODO
///
/// Wire the locking infrastructure:
///   1. \`worker_suspend()\` to save the current thread context.
///   2. \`mutex_lock(&dp->dmap_lock)\`.
///   3. \`worker_resume()\` to restore the thread context.
pub fn lock_dmap(dp: *mut Dmap) {
    let _ = dp;
}

/// Unlock a dmap entry.
///
/// Releases the per-entry mutex that was acquired by \`lock_dmap()\`.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`unlock_dmap()\` (line 47)
///
/// # Safety
///
/// \`dp\` must point to a valid, locked dmap entry.
///
/// # TODO
///
/// Wire \`mutex_unlock(&dp->dmap_lock)\`.
pub fn unlock_dmap(dp: *mut Dmap) {
    let _ = dp;
}

// =============================================================================
// Initialisation
// =============================================================================

/// Initialize the device mapping table.
///
/// Zeroes the dmap table, sets every entry's \`dmap_driver\` to \`NONE\`,
/// initialises each entry's mutex, and sets up the special \`CTTY_MAJOR\`
/// entry which is handled by VFS itself.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`init_dmap()\` (line 216)
///
/// # Safety
///
/// Must be called exactly once during VFS initialisation, before any
/// driver mappings or I/O operations are attempted.
///
/// # TODO
///
/// Wire full initialisation:
///   1. Iterate over all \`NR_DEVICES\` entries.
///   2. Set \`dmap_driver = NONE\`, \`dmap_servicing = INVALID_THREAD\`.
///   3. Call \`mutex_init()\` on each \`dmap_lock\`.
///   4. Call \`map_driver("vfs", CTTY_MAJOR, VFS_PROC_NR)\` for the
///      controlling-terminal entry.
pub fn init_dmap() {
    // TODO: iterate NR_DEVICES, set dmap_driver = NONE, init mutexes,
    // then map the CTTY_MAJOR entry to VFS itself.
}

// =============================================================================
// Lookup / matching
// =============================================================================

/// Check if a driver endpoint matches a major device number.
///
/// Returns 1 if the dmap entry for \`major\` is valid (driver not \`NONE\`)
/// and its \`dmap_driver\` field equals \`proc\`.  Returns 0 otherwise.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`dmap_driver_match()\` (line 238)
///
/// # TODO
///
/// Implement the bounds check on \`major\` and compare the dmap entry's
/// \`dmap_driver\` field against \`proc\`.
pub fn dmap_driver_match(proc: i32, major: i32) -> i32 {
    let _ = (proc, major);
    ENOSYS
}

/// A driver endpoint has come up.
///
/// Called when a device driver with endpoint \`proc_nr\` has been restarted.
/// For block drivers (\`is_blk != 0\`), it initiates driver recovery via
/// \`bdev_up()\`.  For character drivers, it invalidates all open filps that
/// reference the affected major.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`dmap_endpt_up()\` (line 261)
///
/// # Safety
///
/// Requires exclusive access to the global dmap and fproc tables.
///
/// # TODO
///
/// Wire the recovery flow:
///   1. Scan the dmap table for entries matching \`proc_e\`.
///   2. For block drivers: stop any servicing worker, set
///      \`dmap_recovering\`, call \`bdev_up(major)\`, clear recovering.
///   3. For character drivers: stop servicing worker, call
///      \`invalidate_filp_by_char_major(major)\`.
pub fn dmap_endpt_up(proc_nr: i32, is_blk: i32) {
    let _ = (proc_nr, is_blk);
    // TODO: iterate dmap, handle block/char driver recovery.
}

/// Get the dmap entry for a driver endpoint.
///
/// Searches the dmap table linearly for an entry whose \`dmap_driver\`
/// equals \`proc_e\`.  Returns a pointer to the entry, or \`null_mut()\`
/// if no match is found.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`get_dmap()\` (line 303)
///
/// # TODO
///
/// Implement the linear scan over \`NR_DEVICES\` using
/// \`dmap_driver_match()\` and return a pointer to the matching entry.
pub fn get_dmap(proc_e: i32) -> *mut Dmap {
    let _ = proc_e;
    core::ptr::null_mut()
}

/// Get the dmap entry by major device number.
///
/// Returns a pointer to the dmap entry for \`major\`, or \`null_mut()\` if
/// the major is out of range or the entry has no driver (\`dmap_driver == NONE\`).
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`get_dmap_by_major()\` (line 250)
///
/// # TODO
///
/// Implement bounds check on \`major\` against \`NR_DEVICES\`, check
/// \`dmap_driver != NONE\`, and return a pointer to the entry.
pub fn get_dmap_by_major(major: i32) -> *mut Dmap {
    let _ = major;
    core::ptr::null_mut()
}

// =============================================================================
// Unmapping
// =============================================================================

/// Unmap all dmap entries for a given endpoint.
///
/// Scans the dmap table and unmaps every entry whose \`dmap_driver\`
/// matches \`proc_nr\`.  Used when a driver process exits.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`dmap_unmap_by_endpt()\` (line 166)
///
/// # Safety
///
/// Requires exclusive access to the global dmap table.
///
/// # TODO
///
/// Wire the unmap flow:
///   1. Iterate over \`0..NR_DEVICES\`.
///   2. For each matching entry, call \`map_driver(label, major, NONE)\`
///      to invalidate the slot.
///   3. \`invalidate_filp_by_char_major(major)\` is called inside
///      \`map_driver\` when unmapping.
pub fn dmap_unmap_by_endpt(proc_nr: i32) {
    let _ = proc_nr;
    // TODO: iterate dmap table and unmap matching entries.
}

// =============================================================================
// Service/driver registration
// =============================================================================

/// Map a service to a device (called by RS).
///
/// Called from the Reincarnation Server (RS) when a new system service
/// starts up.  If the service publishes a device number (\`dev_nr\`),
/// the service is registered as the driver for that major device.
///
/// C source: \`minix/servers/vfs/dmap.c\` — \`map_service()\` (line 186)
///
/// # Parameters
///
/// \`rpub\` — pointer to the RS public entry for the service.
///
/// # Safety
///
/// \`rpub\` must point to a valid, fully-initialised RS public entry.
/// Requires exclusive access to the global fproc and dmap tables.
///
/// # TODO
///
/// Wire the registration flow:
///   1. If \`IS_RPUB_BOOT_USR\`, return \`OK\` (boot-time user processes
///      are not remapped).
///   2. Look up the endpoint in the fproc table, set \`FP_SRV_PROC\`.
///   3. If \`dev_nr == NO_DEV\`, it's not a driver — return \`OK\`.
///   4. Otherwise call \`map_driver(rpub.label, rpub.dev_nr, rpub.endpoint)\`.
pub fn map_service(rpub: *const core::ffi::c_void) -> i32 {
    let _ = rpub;
    ENOSYS
}
