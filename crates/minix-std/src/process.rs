//! Process lifecycle — PM protocol wrappers.
//!
//! Provides `fork`, `exit`, `waitpid`, `exec`, `getpid`, `getppid` by
//! sending PM server messages via the kernel IPC syscall.
//!
//! PM message numbers (from `.refs/minix-3.3.0/minix/include/minix/callnr.h`):
//! ```text
//! PM_BASE = 0x000
//! PM_EXIT    = PM_BASE + 1   (0x001)
//! PM_FORK    = PM_BASE + 2   (0x002)
//! PM_WAITPID = PM_BASE + 3   (0x003)
//! PM_GETPID  = PM_BASE + 4   (0x004)
//! PM_EXEC    = PM_BASE + 14  (0x00E)
//! PM_EXEC_NEW = PM_BASE + 43 (0x02B)
//! ```

#![allow(dead_code)]

#[cfg(target_os = "none")]
use crate::sendrec;
use crate::{MinixErr, PM_PROC_NR};

pub const PM_BASE: u32 = 0x000;
pub const PM_EXIT: u32 = PM_BASE + 1;
pub const PM_FORK: u32 = PM_BASE + 2;
pub const PM_WAITPID: u32 = PM_BASE + 3;
pub const PM_GETPID: u32 = PM_BASE + 4;
pub const PM_EXEC: u32 = PM_BASE + 14;
pub const PM_EXEC_NEW: u32 = PM_BASE + 43;

// For PM calls, the message layout is:
//   offset 0: dest endpoint (i32) — set by sendrec
//   offset 4: source endpoint (i32) — set by kernel
//   offset 8: call_nr / m_type (i32) — PM_* constant
//   offset 12-xx: call-specific data

const OFF_TYPE: usize = 8;

// PM_GETPID result layout:
//   offset 16: pid (i32) — low 32 bits of return
//   offset 20: ppid (i32) — high 32 bits >> 32
const OFF_PID: usize = 16;
const OFF_PPID: usize = 20;

// PM_FORK result:
//   offset 16: child_pid (i32)
//   offset 20: child_endpoint (i32) — unused by user
const OFF_CHILD_PID: usize = 16;

// PM_WAITPID args:
//   offset 12: pid (i32) — child pid to wait for, or -1 for any
//   offset 16: options (i32) — WNOHANG, WUNTRACED
//   offset 20: status (i32) — returned status code
const OFF_WAIT_PID: usize = 12;
const OFF_WAIT_OPTIONS: usize = 16;
const OFF_WAIT_STATUS: usize = 20;

// PM_EXEC_NEW args:
//   offset 12: exec_endpt (i32) — calling process endpoint, set by kernel
//   offset 16: grant_id (i32) — grant for the exec data
//   offset 20: grant_size (i32) — size of exec data
const OFF_EXEC_ENDPT: usize = 12;

// Helpers

/// Read an i32 from a message buffer at the given offset.
fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

/// Write an i32 into a message buffer at the given offset.
fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

// Process lifecycle functions

/// Fork the current process.
///
/// On success, returns the child PID in the parent, and 0 in the child.
///
/// # Safety
///
/// Must be called in a context where the PM server is running and can
/// process fork requests.
pub unsafe fn fork() -> Result<i32, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (PM_FORK, PM_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_FORK as i32);
        let result = sendrec(PM_PROC_NR, &mut msg);
        match result {
            Ok(_) => {
                let mtype = msg_i32(&msg, OFF_TYPE);
                if mtype < 0 {
                    Err(MinixErr::from_i32(mtype))
                } else {
                    Ok(msg_i32(&msg, OFF_CHILD_PID))
                }
            }
            Err(e) => Err(e),
        }
    }
}

/// Exit the current process with the given status.
pub fn exit(status: i32) -> ! {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_EXIT as i32);
        msg_set_i32(&mut msg, OFF_CHILD_PID, status);
        let _ = sendrec(PM_PROC_NR, &mut msg);
    }
    let _ = status;
    loop {
        core::hint::spin_loop();
    }
}

/// Wait for a child process to change state.
///
/// `pid` is the child PID to wait for, or -1 for any child.
/// `options` can be 0 (blocking) or `WNOHANG` (non-blocking).
pub fn waitpid(pid: i32, _options: i32) -> Result<(i32, i32), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_WAITPID as i32);
        msg_set_i32(&mut msg, OFF_WAIT_PID, pid);
        msg_set_i32(&mut msg, OFF_WAIT_OPTIONS, _options);
        let result = sendrec(PM_PROC_NR, &mut msg);
        match result {
            Ok(r) => {
                let mtype = msg_i32(&msg, OFF_TYPE);
                if mtype < 0 {
                    Err(MinixErr::from_i32(mtype))
                } else {
                    Ok((r, msg_i32(&msg, OFF_WAIT_STATUS)))
                }
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = pid;
        Err(MinixErr::ENOSYS)
    }
}

/// Execute a new program.
///
/// `path` is the binary path, `argv` is the null-terminated argument list.
/// On success, does not return (the process is replaced).
pub fn exec(path: &str, _argv: &[*const u8]) -> Result<i32, MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        // Call SYS_EXEC_REPLACE (61) directly to load from initramfs.
        // This bypasses PM's incomplete grant-based exec path.
        // args[0] = path pointer, args[1] = argv pointer (unused).
        let result = minix_rt::syscall3(61, path.as_ptr() as u64, 0, 0);
        if result == 0 {
            // Should not return — process is replaced.
            loop {}
        } else {
            Err(MinixErr::from_i32(result as i32))
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (path, _argv);
        Err(MinixErr::ENOSYS)
    }
}

/// Get the current process PID and parent PID.
///
/// Returns `(pid, ppid)` on success.
pub fn getpid() -> Result<(i32, i32), MinixErr> {
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_GETPID as i32);
        let result = sendrec(PM_PROC_NR, &mut msg);
        match result {
            Ok(_) => {
                let mtype = msg_i32(&msg, OFF_TYPE);
                if mtype < 0 {
                    Err(MinixErr::from_i32(mtype))
                } else {
                    // PM returns combined pid/ppid: pid in low 32, ppid in high 32.
                    let pid = msg_i32(&msg, OFF_PID);
                    let ppid = msg_i32(&msg, OFF_PPID);
                    Ok((pid, ppid))
                }
            }
            Err(e) => Err(e),
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        Err(MinixErr::ENOSYS)
    }
}

impl MinixErr {
    /// Create a MinixErr from a raw syscall return value (non-positive).
    pub const fn from_i32(v: i32) -> Self {
        // MINIX convention: negative return = -errno
        if v < 0 { MinixErr(-v) } else { MinixErr(0) }
    }

    /// ENOSYS error constant.
    pub const ENOSYS: MinixErr = MinixErr(71);
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pm_call_numbers() {
        assert_eq!(PM_BASE, 0x000);
        assert_eq!(PM_EXIT, 0x001);
        assert_eq!(PM_FORK, 0x002);
        assert_eq!(PM_WAITPID, 0x003);
        assert_eq!(PM_GETPID, 0x004);
        assert_eq!(PM_EXEC_NEW, 0x02B);
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 8, 42);
        assert_eq!(msg_i32(&msg, 8), 42);

        msg_set_i32(&mut msg, 16, -1);
        assert_eq!(msg_i32(&msg, 16), -1);
    }

    #[test]
    fn test_fork_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_FORK as i32);
        assert_eq!(msg_i32(&msg, 8), 0x002);
        // All other fields should be zero
        assert_eq!(msg_i32(&msg, 12), 0);
        assert_eq!(msg_i32(&msg, 16), 0);
    }

    #[test]
    fn test_exit_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_EXIT as i32);
        msg_set_i32(&mut msg, OFF_CHILD_PID, 42);
        assert_eq!(msg_i32(&msg, 8), 0x001);
        assert_eq!(msg_i32(&msg, 16), 42);
    }

    #[test]
    fn test_waitpid_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_WAITPID as i32);
        msg_set_i32(&mut msg, OFF_WAIT_PID, 123);
        msg_set_i32(&mut msg, OFF_WAIT_OPTIONS, 1); // WNOHANG
        assert_eq!(msg_i32(&msg, 8), 0x003);
        assert_eq!(msg_i32(&msg, 12), 123);
        assert_eq!(msg_i32(&msg, 16), 1);
    }

    #[test]
    fn test_getpid_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_GETPID as i32);
        assert_eq!(msg_i32(&msg, 8), 0x004);
    }

    #[test]
    fn test_exec_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, PM_EXEC_NEW as i32);
        assert_eq!(msg_i32(&msg, 8), 0x02B);
    }

    #[test]
    fn test_fork_returns_enosys_on_host() {
        let result = unsafe { fork() };
        assert!(result.is_err());
    }

    #[test]
    fn test_waitpid_returns_enosys_on_host() {
        let result = waitpid(-1, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_getpid_returns_enosys_on_host() {
        let result = getpid();
        assert!(result.is_err());
    }

    #[test]
    fn test_exec_returns_enosys_on_host() {
        let result = exec("/bin/sh", &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_i32() {
        assert_eq!(MinixErr::from_i32(0), MinixErr(0));
        assert_eq!(MinixErr::from_i32(-1), MinixErr(1)); // EPERM
        assert_eq!(MinixErr::from_i32(-22), MinixErr(22)); // EINVAL
        assert_eq!(MinixErr::from_i32(42), MinixErr(0)); // positive = OK
    }

    #[test]
    fn test_getpid_parse_result() {
        // Simulate PM_GETPID returning pid=123, ppid=1
        // The PM server returns the combined value in the IPC result.
        // Our getpid() reads from message offsets.
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, crate::OK);
        msg_set_i32(&mut msg, OFF_PID, 123);
        msg_set_i32(&mut msg, OFF_PPID, 1);
        assert_eq!(msg_i32(&msg, OFF_PID), 123);
        assert_eq!(msg_i32(&msg, OFF_PPID), 1);
    }

    #[test]
    fn test_exit_does_not_return() {
        // Verify the function signature is `fn(i32) -> !`
        fn _check(f: fn(i32) -> !) {
            let _ = f;
        }
        _check(exit);
    }

    #[test]
    fn test_fork_signature() {
        fn _check(f: unsafe fn() -> Result<i32, MinixErr>) {
            let _ = f;
        }
        _check(fork);
    }
}
