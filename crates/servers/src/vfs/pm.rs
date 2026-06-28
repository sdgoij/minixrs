//! VFS↔PM communication protocol — adapted from `minix/servers/vfs/main.c` (service_pm)
//!
//! Handles messages from the Process Manager about process lifecycle events:
//! fork, exec, exit, setuid/setgid, setsid, reboot, dumpcore, etc.
//!
//! These are dispatched from the VFS main loop when a message arrives from
//! PM_PROC_NR. Most functions update the VFS process table (Fproc).

use crate::vfs::consts::*;

/// Dispatch a PM message to the appropriate handler.
///
/// Called from the VFS main loop when a message arrives from PM_PROC_NR.
/// Dispatches based on the message type to the correct pm_* handler.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/main.c` (service_pm)
///
/// TODO: read message type from job_m_in / scratchpad, call the appropriate
/// pm_* function (pm_fork, pm_exit, pm_exec, pm_setuid, pm_setgid, etc.),
/// and reply to PM.
pub fn service_pm() -> i32 {
    ENOSYS
}

/// Handle postponed PM operations (exec continuation, core dump).
///
/// Called after VFS has processed a PM exec/dumpcore request, to perform
/// the second phase (e.g., setting up the new process's address space).
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/main.c` (service_pm_postponed)
pub fn service_pm_postponed() {
    // TODO: handle second phase of PM_EXEC, PM_DUMPCORE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_pm_returns_enosys() {
        assert_eq!(service_pm(), ENOSYS);
    }
}
