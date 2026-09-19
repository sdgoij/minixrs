//! Message-layout helpers shared by the client wrappers.
//!
//! Every server client here speaks the same wire layout, so the offsets and the
//! little set of readers and writers live in one place rather than being copied
//! per client. The request code goes at bytes 4..8 because the kernel overwrites
//! bytes 0..4 with the destination endpoint in `sendrec`, and the payload starts
//! at byte 8.

#![allow(dead_code)]

use minix_std::MinixErr;

pub(crate) type Message = [u8; 64];

/// i32 request code, or the reply status.
pub(crate) const OFF_CALL: usize = 4;
/// `m2` payload: the integers, then the long words.
pub(crate) const OFF_M2_I1: usize = 8;
pub(crate) const OFF_M2_I2: usize = 12;
pub(crate) const OFF_M2_I3: usize = 16;
pub(crate) const OFF_M2_L1: usize = 24;
pub(crate) const OFF_M2_L2: usize = 32;

pub(crate) fn msg_set_i32(msg: &mut Message, off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

pub(crate) fn msg_set_u64(msg: &mut Message, off: usize, val: u64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

pub(crate) fn msg_get_i32(msg: &Message, off: usize) -> i32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&msg[off..off + 4]);
    i32::from_ne_bytes(bytes)
}

pub(crate) fn msg_get_i64(msg: &Message, off: usize) -> i64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&msg[off..off + 8]);
    i64::from_ne_bytes(bytes)
}

pub(crate) fn build_msg(typ: u32) -> Message {
    let mut msg = [0u8; 64];
    msg_set_i32(&mut msg, OFF_CALL, typ as i32);
    msg
}

/// Read the reply status from the `m_type` field. Negative replies map to
/// `Err(MinixErr(pos))`.
pub(crate) fn reply_status(msg: &Message) -> Result<i32, MinixErr> {
    let mtype = msg_get_i32(msg, OFF_CALL);
    if mtype < 0 {
        Err(MinixErr(-mtype))
    } else {
        Ok(mtype)
    }
}

pub(crate) fn check_result(msg: &Message) -> Result<(), MinixErr> {
    reply_status(msg).map(|_| ())
}
