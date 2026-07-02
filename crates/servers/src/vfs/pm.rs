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

// PM↔VFS message types (from com.h)

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

const VFS_PM_EXEC_REPLY: i32 = VFS_PM_RS_BASE + 6;
const VFS_PM_EXIT_REPLY: i32 = VFS_PM_RS_BASE + 4;
const VFS_PM_CORE_REPLY: i32 = VFS_PM_RS_BASE + 5;

// mess_7 field offsets (x86_64)
// Full message: m_source(4) + m_type(4) + mess_7 payload(56)
// mess_7 relative to payload start (= absolute offset 8):
//   m7i1..m7i5: int (4 bytes) at rel 0, 4, 8, 12, 16
//   m7p1, m7p2: pointer (8 bytes) at rel 20, 28

const MSG_TYPE_OFF: usize = 4;

const PM_ENDPT_OFF: usize = 8; // m7_i1
const PM_EID_OFF: usize = 12; // m7_i2  (also PATH_LEN, STATUS, TERM_SIG)
const PM_RID_OFF: usize = 16; // m7_i3  (also FRAME_LEN, CPID)
const PM_REUID_OFF: usize = 20; // m7_i4
const PM_REGID_OFF: usize = 24; // m7_i5  (also PS_STR, NEWPS_STR)
const PM_PATH_OFF: usize = 28; // m7_p1  (also PC)
const PM_FRAME_OFF: usize = 36; // m7_p2  (also NEWSP)

// Message field helpers

fn r_i32(buf: &[u8; 64], off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap_or([0; 4]))
}

fn r_u64(buf: &[u8; 64], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap_or([0; 8]))
}

fn w_i32(buf: &mut [u8; 64], off: usize, val: i32) {
    buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
}

fn w_u64(buf: &mut [u8; 64], off: usize, val: u64) {
    buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
}

// Dispatch

/// Dispatch a PM message to the appropriate handler.
///
/// Called from the VFS main loop when a message arrives from PM_PROC_NR.
/// Handles PM messages that can be processed immediately (non-blocking).
/// For exec/exit/dumpcore, the first phase is done here (closing FDs, etc.)
/// and the heavy lifting is deferred to service_pm_postponed.
pub fn service_pm() -> i32 {
    let glob = unsafe { &mut *vfs_global() };
    let call_nr = r_i32(&glob.fs_m_in, MSG_TYPE_OFF);

    match call_nr {
        VFS_PM_SETUID => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let euid = r_i32(&glob.fs_m_in, PM_EID_OFF);
            let ruid = r_i32(&glob.fs_m_in, PM_RID_OFF);
            pm_setuid(proc_e, euid, ruid);
            w_i32(&mut glob.fs_m_out, MSG_TYPE_OFF, VFS_PM_RS_BASE + 1);
            w_i32(&mut glob.fs_m_out, PM_ENDPT_OFF, proc_e);
            OK
        }

        VFS_PM_SETGID => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let egid = r_i32(&glob.fs_m_in, PM_EID_OFF);
            let rgid = r_i32(&glob.fs_m_in, PM_RID_OFF);
            pm_setgid(proc_e, egid, rgid);
            w_i32(&mut glob.fs_m_out, MSG_TYPE_OFF, VFS_PM_RS_BASE + 2);
            w_i32(&mut glob.fs_m_out, PM_ENDPT_OFF, proc_e);
            OK
        }

        VFS_PM_SETSID => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            pm_setsid(proc_e);
            w_i32(&mut glob.fs_m_out, MSG_TYPE_OFF, VFS_PM_RS_BASE + 3);
            w_i32(&mut glob.fs_m_out, PM_ENDPT_OFF, proc_e);
            OK
        }

        VFS_PM_EXIT | VFS_PM_EXEC | VFS_PM_DUMPCORE | VFS_PM_UNPAUSE => {
            // Phase 1: do quick non-blocking work, defer the rest.
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
            // Phase 2 (postponed) is called by the worker thread when ready.
            OK
        }

        VFS_PM_FORK | VFS_PM_SRV_FORK => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let pproc_e = r_i32(&glob.fs_m_in, PM_EID_OFF);
            let child_pid = r_i32(&glob.fs_m_in, PM_RID_OFF);
            let _reuid = r_i32(&glob.fs_m_in, PM_REUID_OFF);
            let _regid = r_i32(&glob.fs_m_in, PM_REGID_OFF);

            pm_fork(pproc_e, proc_e, child_pid);

            let reply_type = if call_nr == VFS_PM_SRV_FORK {
                pm_setuid(proc_e, _reuid, _reuid);
                pm_setgid(proc_e, _regid, _regid);
                VFS_PM_RS_BASE + 8
            } else {
                VFS_PM_RS_BASE + 7
            };
            w_i32(&mut glob.fs_m_out, MSG_TYPE_OFF, reply_type);
            OK
        }

        VFS_PM_REBOOT => {
            pm_reboot();
            w_i32(&mut glob.fs_m_out, MSG_TYPE_OFF, VFS_PM_RS_BASE + 10);
            OK
        }

        VFS_PM_INIT | VFS_PM_SETGROUPS => OK,

        _ => EINVAL,
    }
}

/// Handle postponed PM operations (exec continuation, exit finalization,
/// core dump).
///
/// Called by a worker thread after VFS has processed the initial phase
/// of a PM exec/exit/dumpcore request. Performs the potentially-blocking
/// I/O (reading the binary, writing the core file) and sends the reply.
pub fn service_pm_postponed() {
    let glob = unsafe { &*vfs_global() };
    let call_nr = r_i32(&glob.fs_m_in, MSG_TYPE_OFF);

    match call_nr {
        VFS_PM_EXEC => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let _exec_path = r_u64(&glob.fs_m_in, PM_PATH_OFF);
            let _exec_path_len = r_i32(&glob.fs_m_in, PM_EID_OFF) as usize;
            let _stack_frame = r_u64(&glob.fs_m_in, PM_FRAME_OFF);
            let _stack_frame_len = r_i32(&glob.fs_m_in, PM_RID_OFF) as usize;
            let _ps_str = r_i32(&glob.fs_m_in, PM_REGID_OFF) as u64;

            // TODO: full pm_exec with binary loading (needs FS request layer):
            //   r = pm_exec(exec_path, exec_path_len, stack_frame,
            //               stack_frame_len, &pc, &newsp, &ps_str);
            // For now, complete the exec with the lightweight version.
            pm_exec();

            // Build reply
            let glob_out = unsafe { &mut *vfs_global() };
            w_i32(&mut glob_out.fs_m_out, MSG_TYPE_OFF, VFS_PM_EXEC_REPLY);
            w_i32(&mut glob_out.fs_m_out, PM_ENDPT_OFF, proc_e);
            w_i32(&mut glob_out.fs_m_out, PM_EID_OFF, OK); // STATUS
            w_u64(&mut glob_out.fs_m_out, PM_PATH_OFF, 0); // PC (stub)
            w_u64(&mut glob_out.fs_m_out, PM_FRAME_OFF, 0); // NEWSP (stub)
            w_i32(&mut glob_out.fs_m_out, PM_REGID_OFF, 0); // NEWPS_STR (stub)
        }

        VFS_PM_EXIT => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);

            // Verify endpoint matches current process.
            let glob_fp = unsafe { &*vfs_global() }.fp;
            if !glob_fp.is_null() {
                unsafe {
                    if (*glob_fp).fp_endpoint == proc_e {
                        pm_exit();
                    }
                }
            }

            let glob_out = unsafe { &mut *vfs_global() };
            w_i32(&mut glob_out.fs_m_out, MSG_TYPE_OFF, VFS_PM_EXIT_REPLY);
            w_i32(&mut glob_out.fs_m_out, PM_ENDPT_OFF, proc_e);
        }

        VFS_PM_DUMPCORE => {
            let proc_e = r_i32(&glob.fs_m_in, PM_ENDPT_OFF);
            let term_sig = r_i32(&glob.fs_m_in, PM_EID_OFF);
            let _core_path = r_u64(&glob.fs_m_in, PM_PATH_OFF);

            let r = pm_dumpcore(term_sig, _core_path);

            let glob_out = unsafe { &mut *vfs_global() };
            w_i32(&mut glob_out.fs_m_out, MSG_TYPE_OFF, VFS_PM_CORE_REPLY);
            w_i32(&mut glob_out.fs_m_out, PM_ENDPT_OFF, proc_e);
            w_i32(&mut glob_out.fs_m_out, PM_EID_OFF, r);
        }

        _ => {}
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::glo::vfs_global;
    use crate::vfs::types::*;

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
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let parent = &mut *fproc_arr.add(2);
            parent.fp_endpoint = 2;
            parent.fp_pid = 100;
            parent.fp_filp = [-1i32; OPEN_MAX];
            parent.fp_filp[0] = 1;
            parent.fp_tty = 0;
            parent.fp_rdir = core::ptr::null_mut();
            parent.fp_cdir = core::ptr::null_mut();
            let filp_arr = core::ptr::addr_of_mut!((*glob).filp) as *mut Filp;
            (*filp_arr.add(1)).filp_count = 1;

            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&10i32.to_le_bytes());
            fs_m_in[PM_EID_OFF..PM_EID_OFF + 4].copy_from_slice(&2i32.to_le_bytes());
            fs_m_in[PM_RID_OFF..PM_RID_OFF + 4].copy_from_slice(&101i32.to_le_bytes());
        }
        assert_eq!(service_pm(), OK);
        unsafe {
            let glob = vfs_global();
            let fproc_arr = core::ptr::addr_of_mut!((*glob).fproc) as *mut Fproc;
            let child = &*fproc_arr.add(10);
            assert_eq!(child.fp_endpoint, 10);
            assert_eq!(child.fp_pid, 101);
            assert_eq!(child.fp_filp[0], 1);
            assert!(child.fp_cdir.is_null());
        }
    }

    #[test]
    fn test_pm_unknown_call_returns_einval() {
        unsafe { setup_pm_msg(0x999, 0) }
        assert_eq!(service_pm(), EINVAL);
    }

    #[test]
    fn test_pm_postponed_exec_builds_reply() {
        unsafe {
            setup_pm_msg(VFS_PM_EXEC, 42);
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&42i32.to_le_bytes());
            fs_m_in[PM_PATH_OFF..PM_PATH_OFF + 8].copy_from_slice(&0x1000u64.to_le_bytes());
            fs_m_in[PM_EID_OFF..PM_EID_OFF + 4].copy_from_slice(&8i32.to_le_bytes()); // path_len
            fs_m_in[PM_FRAME_OFF..PM_FRAME_OFF + 8].copy_from_slice(&0x2000u64.to_le_bytes());
            fs_m_in[PM_RID_OFF..PM_RID_OFF + 4].copy_from_slice(&256i32.to_le_bytes()); // frame_len
        }
        service_pm_postponed();
        unsafe {
            let glob = vfs_global();
            let fs_m_out = &(*glob).fs_m_out;
            let reply_type = r_i32(fs_m_out, MSG_TYPE_OFF);
            assert_eq!(reply_type, VFS_PM_EXEC_REPLY);
        }
    }

    #[test]
    fn test_pm_postponed_exit_replies() {
        unsafe {
            setup_pm_msg(VFS_PM_EXIT, 7);
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&7i32.to_le_bytes());
        }
        service_pm_postponed();
        unsafe {
            let glob = vfs_global();
            let fs_m_out = &(*glob).fs_m_out;
            let reply_type = r_i32(fs_m_out, MSG_TYPE_OFF);
            assert_eq!(reply_type, VFS_PM_EXIT_REPLY);
        }
    }

    #[test]
    fn test_pm_postponed_dumpcore_replies() {
        unsafe {
            setup_pm_msg(VFS_PM_DUMPCORE, 3);
            let glob = vfs_global();
            let fs_m_in = &mut (*glob).fs_m_in;
            fs_m_in[PM_ENDPT_OFF..PM_ENDPT_OFF + 4].copy_from_slice(&3i32.to_le_bytes());
            fs_m_in[PM_EID_OFF..PM_EID_OFF + 4].copy_from_slice(&11i32.to_le_bytes()); // SIGSEGV
        }
        service_pm_postponed();
        unsafe {
            let glob = vfs_global();
            let fs_m_out = &(*glob).fs_m_out;
            let reply_type = r_i32(fs_m_out, MSG_TYPE_OFF);
            assert_eq!(reply_type, VFS_PM_CORE_REPLY);
        }
    }
}
