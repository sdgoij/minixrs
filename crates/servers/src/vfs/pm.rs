//! VFS↔PM communication protocol — adapted from `minix/servers/vfs/main.c` (service_pm)
//!
//! Handles messages from the Process Manager about process lifecycle events:
//! fork, exec, exit, setuid/setgid, setsid, reboot, dumpcore, etc.
//!
//! These are dispatched from the VFS main loop when a message arrives from
//! PM_PROC_NR. Most functions update the VFS process table (Fproc).

use crate::vfs::consts::*;
use crate::vfs::glo::vfs_global;
use crate::vfs::misc::*;

// ── PM↔VFS message types (from com.h) ─────────────────────────────────────

const VFS_PM_RQ_BASE: i32 = 0x900;
const VFS_PM_RS_BASE: i32 = 0x980;

const VFS_PM_INIT: i32 = VFS_PM_RQ_BASE;
const VFS_PM_SETUID: i32 = VFS_PM_RQ_BASE + 1;
const VFS_PM_SETGID: i32 = VFS_PM_RQ_BASE + 2;
const VFS_PM_SETSID: i32 = VFS_PM_RQ_BASE + 3;
const VFS_PM_EXIT: i32 = VFS_PM_RQ_BASE + 4;
const VFS_PM_DUMPCORE: i32 = VFS_PM_RQ_BASE + 5;
const VFS_PM_EXEC: i32 = VFS_PM_RQ_BASE + 6;
const VFS_PM_FORK: i32 = VFS_PM_RQ_BASE + 7;
const VFS_PM_SRV_FORK: i32 = VFS_PM_RQ_BASE + 8;
const VFS_PM_UNPAUSE: i32 = VFS_PM_RQ_BASE + 9;
const VFS_PM_REBOOT: i32 = VFS_PM_RQ_BASE + 10;
const VFS_PM_SETGROUPS: i32 = VFS_PM_RQ_BASE + 11;

// ── mess_7 field offsets (x86_64) ─────────────────────────────────────────
// The full message layout:
//   offset 0-3:  m_source (endpoint_t, 4 bytes)
//   offset 4-7:  m_type (i32, 4 bytes)
//   offset 8-63: payload (56 bytes) — mess_7 starts here
//
// mess_7 layout (relative to payload start = absolute offset 8):
//   m7i1..m7i5: int (4 bytes each) at rel 0, 4, 8, 12, 16
//   m7p1, m7p2: pointer (8 bytes each) at rel 20, 28

/// Offset of message type in the 64-byte message buffer.
const MSG_TYPE_OFF: usize = 4;

const PM_ENDPT_OFF: usize = 8; // VFS_PM_ENDPT = m7_i1 (rel 0)
const PM_EID_OFF: usize = 12; // VFS_PM_EID = m7_i2 (rel 4)
const PM_RID_OFF: usize = 16; // VFS_PM_RID = m7_i3 (rel 8)
const PM_REUID_OFF: usize = 20; // VFS_PM_REUID = m7_i4 (rel 12)
const PM_REGID_OFF: usize = 24; // VFS_PM_REGID = m7_i5 (rel 16)
const PM_PATH_OFF: usize = 28; // VFS_PM_PATH = m7_p1 (rel 20)

/// Read an `i32` field from a message buffer at the given offset.
fn r_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

/// Read a `u64` field from a message buffer at the given offset.
fn r_u64(buf: &[u8; 64], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}

/// Write an `i32` to a message buffer at the given offset.
fn w_i32(buf: &mut [u8; 64], off: usize, val: i32) {
    buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

// ── Dispatch ──────────────────────────────────────────────────────────────

/// Dispatch a PM message to the appropriate handler.
///
/// Called from the VFS main loop when a message arrives from PM_PROC_NR.
/// Reads the message type from `fs_m_in`, calls the correct pm_* handler,
/// prepares a reply in `fs_m_out`, and returns OK on success.
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/main.c` (service_pm)
pub fn service_pm() -> i32 {
    let glob = unsafe { &mut *vfs_global() };

    // The incoming message type is at MSG_TYPE_OFF.
    let call_nr = r_i32(&glob.fs_m_in, MSG_TYPE_OFF);

    match call_nr {
        VFS_PM_SETUID => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let euid = r_i32(&glob.fs_m_in, PM_EID_OFF);
            let ruid = r_i32(&glob.fs_m_in, PM_RID_OFF);
            pm_setuid(proc_e, euid, ruid);

            w_i32(&mut glob.fs_m_out, 0, VFS_PM_RS_BASE + 1); // VFS_PM_SETUID_REPLY
            w_i32(&mut glob.fs_m_out, PM_ENDPT_OFF, proc_e);
            OK
        }

        VFS_PM_SETGID => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let egid = r_i32(&glob.fs_m_in, PM_EID_OFF);
            let rgid = r_i32(&glob.fs_m_in, PM_RID_OFF);
            pm_setgid(proc_e, egid, rgid);

            w_i32(&mut glob.fs_m_out, 0, VFS_PM_RS_BASE + 2); // VFS_PM_SETGID_REPLY
            w_i32(&mut glob.fs_m_out, PM_ENDPT_OFF, proc_e);
            OK
        }

        VFS_PM_SETSID => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            pm_setsid(proc_e);

            w_i32(&mut glob.fs_m_out, 0, VFS_PM_RS_BASE + 3); // VFS_PM_SETSID_REPLY
            w_i32(&mut glob.fs_m_out, PM_ENDPT_OFF, proc_e);
            OK
        }

        VFS_PM_EXIT | VFS_PM_EXEC | VFS_PM_DUMPCORE | VFS_PM_UNPAUSE => {
            // These require deferred handling via service_pm_postponed.
            // For now, handle the simple cases inline.
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let glob2 = unsafe { &mut *vfs_global() };
            let fp = glob2.fp;
            if !fp.is_null() {
                unsafe { (*fp).fp_endpoint = proc_e };
            }
            if call_nr == VFS_PM_EXEC {
                pm_exec();
            } else if call_nr == VFS_PM_EXIT {
                pm_exit();
            }
            // TODO: send reply via IPC send to PM_PROC_NR
            OK
        }

        VFS_PM_FORK | VFS_PM_SRV_FORK => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF); // child endpoint
            let pproc_e = r_i32(&glob.fs_m_in, PM_EID_OFF); // parent endpoint (m7_i2 = PENDPT)
            let child_pid = r_i32(&glob.fs_m_in, PM_RID_OFF); // child pid (m7_i3 = CPID)
            let _reuid = r_i32(&glob.fs_m_in, PM_REUID_OFF);
            let _regid = r_i32(&glob.fs_m_in, PM_REGID_OFF);

            pm_fork(pproc_e, proc_e, child_pid);

            let reply_type = if call_nr == VFS_PM_SRV_FORK {
                pm_setuid(proc_e, _reuid, _reuid);
                pm_setgid(proc_e, _regid, _regid);
                VFS_PM_RS_BASE + 8 // VFS_PM_SRV_FORK_REPLY
            } else {
                VFS_PM_RS_BASE + 7 // VFS_PM_FORK_REPLY
            };
            w_i32(&mut glob.fs_m_out, 0, reply_type);
            OK
        }

        VFS_PM_REBOOT => {
            pm_reboot();
            w_i32(&mut glob.fs_m_out, 0, VFS_PM_RS_BASE + 10); // VFS_PM_REBOOT_REPLY
            OK
        }

        VFS_PM_INIT | VFS_PM_SETGROUPS => {
            // TODO: implement PM_INIT (process table exchange) and PM_SETGROUPS
            OK
        }

        _ => EINVAL,
    }
}

/// Handle postponed PM operations (exec continuation, core dump).
///
/// Called after VFS has processed a PM exec/dumpcore request, to perform
/// the second phase (e.g., setting up the new process's address space).
///
/// Source: `.refs/minix-3.3.0/minix/servers/vfs/main.c` (service_pm_postponed)
pub fn service_pm_postponed() {
    let glob = unsafe { &*vfs_global() };
    let call_nr = r_i32(&glob.fs_m_in, MSG_TYPE_OFF);

    match call_nr {
        VFS_PM_EXEC => {
            // TODO: complete VFS_PM_EXEC postponed handling:
            //   1. Read exec details from fs_m_in (path, stack_frame, etc.)
            //   2. Perform the exec (close on exec FDs via pm_exec already done)
            //   3. Read program counter from result
            //   4. Reply VFS_PM_EXEC_REPLY with PC, newsp, ps_str
        }

        VFS_PM_DUMPCORE => {
            let term_sig = r_i32(&glob.fs_m_in, PM_EID_OFF); // m7_i2 = TERM_SIG
            let _core_path = r_u64(&glob.fs_m_in, PM_PATH_OFF);
            let _r = pm_dumpcore(term_sig, _core_path);
        }

        _ => {}
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::glo::vfs_global;
    use crate::vfs::types::*;

    /// Helper: set up a test fp with the given endpoint, set it as current,
    /// and prepare fs_m_in with the given call number.
    unsafe fn setup_pm_msg(call_nr: i32, fp_endpt: i32) {
        let glob = vfs_global();
        let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
        let fp = &mut *fproc_arr.add(0);
        fp.fp_endpoint = fp_endpt;
        fp.fp_effuid = 0;
        fp.fp_realuid = 0;
        fp.fp_effgid = 0;
        fp.fp_realgid = 0;
        fp.fp_flags = 0;
        fp.fp_filp = [-1i32; OPEN_MAX];
        (*glob).fp = fp;
        (*glob).fs_m_in = [0u8; 64];
        (*glob).fs_m_out = [0u8; 64];
        // Write call number at MSG_TYPE_OFF (offset 4 in the message buffer)
        let fs_m_in = &mut (*glob).fs_m_in;
        fs_m_in[MSG_TYPE_OFF..MSG_TYPE_OFF + 4].copy_from_slice(&call_nr.to_le_bytes());
    }

    #[test]
    fn test_pm_setuid_dispatch() {
        unsafe {
            setup_pm_msg(VFS_PM_SETUID, 0);
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[PM_EID_OFF..PM_EID_OFF + 4].copy_from_slice(&1000i32.to_le_bytes());
            fs_m_in[PM_RID_OFF..PM_RID_OFF + 4].copy_from_slice(&999i32.to_le_bytes());
        }
        assert_eq!(service_pm(), OK);
        unsafe {
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &*fproc_arr.add(0);
            assert_eq!(fp.fp_effuid, 1000);
            assert_eq!(fp.fp_realuid, 999);
        }
    }

    #[test]
    fn test_pm_setgid_dispatch() {
        unsafe {
            setup_pm_msg(VFS_PM_SETGID, 0);
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
            fs_m_in[PM_EID_OFF..PM_EID_OFF + 4].copy_from_slice(&2000i32.to_le_bytes());
            fs_m_in[PM_RID_OFF..PM_RID_OFF + 4].copy_from_slice(&1999i32.to_le_bytes());
        }
        assert_eq!(service_pm(), OK);
        unsafe {
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &*fproc_arr.add(0);
            assert_eq!(fp.fp_effgid, 2000);
            assert_eq!(fp.fp_realgid, 1999);
        }
    }

    #[test]
    fn test_pm_setsid_dispatch() {
        unsafe {
            setup_pm_msg(VFS_PM_SETSID, 0);
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&0i32.to_le_bytes());
        }
        assert_eq!(service_pm(), OK);
        unsafe {
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let fp = &*fproc_arr.add(0);
            assert!(fp.fp_flags & FP_SESLDR != 0);
            assert_eq!(fp.fp_session, 0);
        }
    }

    #[test]
    fn test_pm_fork_dispatch() {
        unsafe {
            setup_pm_msg(VFS_PM_FORK, 10);
            let glob = vfs_global();
            // First set up a parent fproc at slot 2 with some state
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let parent = &mut *fproc_arr.add(2);
            parent.fp_endpoint = 2;
            parent.fp_pid = 100;
            parent.fp_filp = [-1i32; OPEN_MAX];
            parent.fp_filp[0] = 1;
            parent.fp_tty = 0;
            parent.fp_rdir = 1;
            parent.fp_cdir = 2;
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            (*filp_arr.add(1)).filp_count = 1;

            // Write fork message: child endpoint=10, parent endpoint=2, child_pid=101
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&10i32.to_le_bytes());
            fs_m_in[PM_EID_OFF..PM_EID_OFF + 4].copy_from_slice(&2i32.to_le_bytes()); // parent
            fs_m_in[PM_RID_OFF..PM_RID_OFF + 4].copy_from_slice(&101i32.to_le_bytes()); // child pid
        }
        assert_eq!(service_pm(), OK);
        unsafe {
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let child = &*fproc_arr.add(10);
            assert_eq!(child.fp_endpoint, 10);
            assert_eq!(child.fp_pid, 101);
            assert_eq!(child.fp_filp[0], 1);
            assert_eq!(child.fp_cdir, 2);
        }
    }

    #[test]
    fn test_pm_unknown_call_returns_einval() {
        unsafe {
            setup_pm_msg(0x999, 0);
        }
        assert_eq!(service_pm(), EINVAL);
    }
}
