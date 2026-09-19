//! RS client — announce a service to the Reincarnation Server.
//!
//! `rs_up` sends the `RS_UP` request RS's `do_up` reads: a label, its length, and
//! the endpoint the label names. RS records the pair and publishes it to DS
//! (`publish_service`), which is what makes the process a *known* one — DS
//! refuses a publish from an endpoint it cannot name, so a process that never
//! announces itself can read the store but not write to it.
//!
//! The endpoint a service announces has to be its own, and a process does not
//! otherwise know it: endpoint numbers are the kernel's encoding, not the PID. So
//! the request carries an endpoint obtained from `GET_WHOAMI` rather than a guess
//! — announcing the wrong one would leave the service named under an endpoint it
//! never sends from, and therefore still unnameable to DS.
//!
//! All functions return `Err(MinixErr(71))` on host (`cfg(not(target_os =
//! "minix"))`), like the other clients here.

#![allow(dead_code)]

use minix_std::MinixErr;

// Used from the `target_os = "minix"` bodies only; a host build has no wire
// traffic to lay out.
#[cfg(target_os = "minix")]
use crate::wire::{
    Message, OFF_M2_I1, OFF_M2_I2, OFF_M2_L1, build_msg, check_result, msg_get_i32, msg_set_i32,
    msg_set_u64,
};

const RS_ENDPOINT: i32 = 2; // RS_PROC_NR
const RS_UP: u32 = 0x700;

/// Kernel call 26 is `SYS_GETINFO`, and `GET_WHOAMI` asks it for the caller.
const SYS_GETINFO: i32 = 26;
/// `mess_krn_lsys_sys_getwhoami`'s reply: the endpoint at message offset 0.
const WHOAMI_ENDPT_OFF: usize = 0;
/// `mess_lsys_krn_sys_getinfo`'s request field.
const GETINFO_REQUEST_OFF: usize = 8;

/// The endpoint the kernel knows this process by.
pub fn self_endpoint() -> Result<i32, MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let mut msg = [0u8; 64];
        msg_set_i32(
            &mut msg,
            GETINFO_REQUEST_OFF,
            arch_common::com::GET_WHOAMI as i32,
        );
        let r = minix_rt::kernel_call(SYS_GETINFO, &mut msg);
        if r < 0 {
            return Err(MinixErr(-r));
        }
        Ok(msg_get_i32(&msg, WHOAMI_ENDPT_OFF))
    }
    #[cfg(not(target_os = "minix"))]
    {
        Err(MinixErr(71))
    }
}

/// Announce this process to RS under `label`.
///
/// The label is read straight out of this process's own memory, so a failed call
/// means RS could not read it rather than that it read something else.
pub fn rs_up(label: &[u8]) -> Result<(), MinixErr> {
    #[cfg(target_os = "minix")]
    {
        let endpoint = self_endpoint()?;

        let mut msg: Message = build_msg(RS_UP);
        msg_set_i32(&mut msg, OFF_M2_I1, label.len() as i32);
        msg_set_i32(&mut msg, OFF_M2_I2, endpoint);
        msg_set_u64(&mut msg, OFF_M2_L1, label.as_ptr() as u64);

        // SAFETY: the syscall takes the message pointer and overwrites it with
        // the reply; `msg` is a live 64-byte buffer.
        unsafe { minix_std::sendrec(RS_ENDPOINT, &mut msg) }?;
        check_result(&msg)
    }
    #[cfg(not(target_os = "minix"))]
    {
        let _ = label;
        Err(MinixErr(71))
    }
}
