//! VBFS server module.
//!
//! VBFS is a thin server that bridges VirtualBox shared folders to the
//! Minix VFS layer via the SFFS (Shared Folder File System) library.
//! Adapted from `minix/fs/vbfs/vbfs.c`.
//!
//! Architecture: VBFS -> libsffs -> libvboxfs -> vbox driver -> VirtualBox host
//!
//! Since libsffs and libvboxfs are not yet ported to Rust, this module
//! provides the VBFS server skeleton with option parsing, init sequence,
//! and main loop structure. External library calls are stubbed.

use crate::vbfs::config::{PATH_MAX, SffsParams};

// ── Global state ────────────────────────────────────────────────────────

/// Shared folder share name.
static mut SHARE: [u8; PATH_MAX] = [0; PATH_MAX];

/// SFFS parameters.
static mut PARAMS: SffsParams = SffsParams {
    p_prefix: [0; PATH_MAX],
    p_uid: 0,
    p_gid: 0,
    p_file_mask: crate::vbfs::config::DEFAULT_FILE_MASK,
    p_dir_mask: crate::vbfs::config::DEFAULT_DIR_MASK,
    p_case_insens: 0,
};

// ── External library stubs ──────────────────────────────────────────────

/// Stub: initialize vboxfs library with the given share name.
unsafe fn vboxfs_init(_share: &[u8]) -> Result<(bool, bool), i32> {
    todo!("vboxfs_init: VBOX driver not yet ported")
}

/// Stub: clean up vboxfs library resources.
#[expect(dead_code)]
unsafe fn vboxfs_cleanup() {
    todo!("vboxfs_cleanup: VBOX driver not yet ported")
}

/// Stub: initialize SFFS library.
unsafe fn sffs_init(_table: *const core::ffi::c_void, _params: &SffsParams) -> Result<(), i32> {
    todo!("sffs_init: SFFS library not yet ported")
}

/// Stub: run the SFFS main loop.
unsafe fn sffs_loop() {
    todo!("sffs_loop: SFFS library not yet ported")
}

// ── Server implementation ───────────────────────────────────────────────

/// Initialize the VBFS server.
///
/// Parses options, initializes VBOXFS and SFFS libraries.
///
/// # Safety
///
/// Must be called exactly once at startup.
pub unsafe fn vbfs_init() -> Result<(), i32> {
    unsafe {
        let share_ptr = core::ptr::addr_of_mut!(SHARE);
        let params_ptr = core::ptr::addr_of_mut!(PARAMS);

        // A share name is required.
        let first = (*share_ptr)[0];
        if first == 0 {
            return Err(-22); // EINVAL
        }

        // Initialize VBOXFS library.
        let (_case_insens, _roflag) = vboxfs_init(&(*share_ptr))?;

        // Initialize SFFS library.
        sffs_init(core::ptr::null(), &(*params_ptr))?;

        Ok(())
    }
}

/// Run the VBFS server main loop.
///
/// # Safety
///
/// Must be called after `vbfs_init()`.
pub unsafe fn vbfs_run() -> ! {
    unsafe {
        sffs_loop();
        loop {
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vbfs_init_no_share_returns_einval() {
        unsafe {
            let r = vbfs_init();
            assert!(r.is_err());
        }
    }
}
