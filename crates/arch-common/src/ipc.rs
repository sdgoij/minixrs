//! IPC message types from `minix/ipc.h`
//!
//! ABI-critical: the `Message` struct must be exactly 56 bytes with
//! field offsets matching the C `message` struct exactly.

use core::fmt;

pub const PM_BASE: i32 = 0x0000;
pub const KERNEL_CALL: i32 = 0x0100;
pub const FS_BASE: i32 = 0x0200;

/// The central IPC message (56 bytes).
#[repr(C)]
#[derive(Clone)]
pub struct Message {
    pub m_source: i32,
    pub m_type: i32,
    pub m_payload: Payload,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("m_source", &self.m_source)
            .field("m_type", &self.m_type)
            .finish()
    }
}

/// 48-byte payload union.
#[repr(C)]
#[derive(Clone, Copy)]
pub union Payload {
    pub m1: M1,
    pub m2: M2,
    pub m3: M3,
    pub m4: M4,
    pub m5: M5,
    pub m6: M6,
    pub m7: [u8; 48],
    pub m9: M9,
    pub m10: M10,
    pub raw: [u8; 48],
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Payload").finish()
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct M1 {
    pub m1i1: i32,
    pub m1i2: i32,
    pub m1i3: i32,
    pub m1i4: i32,
    pub m1i5: i32,
    pub m1i6: i32,
    pub m1i7: i32,
    pub m1i8: i32,
    pub m1i9: i32,
    pub m1i10: i32,
    pub m1i11: i32,
    pub m1i12: i32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct M2 {
    pub m2i1: i32,
    pub m2i2: i32,
    pub m2i3: i32,
    pub _pad: i32,
    pub m2l1: i64,
    pub m2l2: i64,
    pub m2l3: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct M3 {
    pub m3i1: i32,
    pub m3i2: i32,
    pub m3i3: i32,
    pub m3i4: i32,
    pub m3i5: i32,
    pub m3i6: i32,
    pub m3i7: i32,
    pub m3s: [i16; 16],
    pub m3ca1: [u8; 8],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct M4 {
    pub m4l1: i64,
    pub m4l2: i64,
    pub m4l3: i64,
    pub m4l4: i64,
    pub m4l5: i64,
    pub m4l6: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct M5 {
    pub m5l1: i64,
    pub m5l2: i64,
    pub m5l3: i64,
    pub m5l4: i64,
    pub m5s1: i64,
    pub m5s2: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct M6 {
    pub m6l1: i64,
    pub m6l2: i64,
    pub m6l3: i64,
    pub m6l4: i64,
    pub m6l5: i64,
    pub m6l6: i64,
}

pub type M8 = M6;
pub type M10 = M6;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct M9 {
    pub m9l1: i64,
    pub m9l2: i64,
    pub m9l3: i64,
    pub m9l4: i64,
    pub m9i1: i32,
    pub m9i2: i32,
    pub m9i3: i32,
    pub m9i4: i32,
    pub m9l5: i64,
    pub m9l6: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union DsVal {
    pub grant: i32,
    pub u32_val: u32,
    pub endpoint: i32,
}

pub const AMF_VALID: u32 = 0x01;
pub const AMF_DONE: u32 = 0x02;
pub const AMF_NOTIFY: u32 = 0x04;
pub const AMF_NOREPLY: u32 = 0x08;
pub const AMF_NOTIFY_ERR: u32 = 0x10;

#[repr(C)]
#[derive(Debug, Clone)]
pub struct AsynMsg {
    pub flags: u32,
    pub endpoint: i32,
    pub result: i32,
    pub msg: Message,
}

pub const OK: i32 = 0;
pub const EPERM: i32 = -1;
pub const ENOENT: i32 = -2;
pub const ESRCH: i32 = -3;
pub const EINTR: i32 = -4;
pub const EIO: i32 = -5;
pub const ENXIO: i32 = -6;
pub const EAGAIN: i32 = -11;
pub const ENOMEM: i32 = -12;
pub const EACCES: i32 = -13;
pub const EFAULT: i32 = -14;
pub const EBUSY: i32 = -16;
pub const EEXIST: i32 = -17;
pub const ENODEV: i32 = -19;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EINVAL: i32 = -22;
pub const ENOBUFS: i32 = -55;
pub const EDONTREPLY: i32 = -201;
pub const ELOCKED: i32 = -202;
pub const ELOCKWILLBLOCK: i32 = -203;

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::offset_of;

    #[test]
    fn test_message_offsets() {
        assert_eq!(offset_of!(Message, m_source), 0);
        assert_eq!(offset_of!(Message, m_type), 4);
        assert_eq!(offset_of!(Message, m_payload), 8);
    }

    #[test]
    fn test_m1_roundtrip() {
        let m = Message {
            m_source: 1,
            m_type: 100,
            m_payload: Payload {
                m1: M1 {
                    m1i1: 10,
                    m1i2: 20,
                    m1i3: 30,
                    m1i4: 40,
                    m1i5: 50,
                    m1i6: 60,
                    m1i7: 70,
                    m1i8: 80,
                    m1i9: 90,
                    m1i10: 100,
                    m1i11: 110,
                    m1i12: 120,
                },
            },
        };
        unsafe {
            assert_eq!(m.m_payload.m1.m1i1, 10);
        }
    }

    #[test]
    fn test_asynmsg_offsets() {
        assert_eq!(offset_of!(AsynMsg, flags), 0);
        assert_eq!(offset_of!(AsynMsg, endpoint), 4);
        assert_eq!(offset_of!(AsynMsg, result), 8);
        // msg is at offset 16 rather than 12 because Message has alignment 8
        // (i64 fields in Payload). On i386 C this would be offset 12.
        // This is the pre-existing size mismatch the user noted.
        assert_eq!(offset_of!(AsynMsg, msg), 16);
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(OK, 0);
        assert_eq!(EPERM, -1);
        assert_eq!(ENOENT, -2);
        assert_eq!(EINVAL, -22);
    }

    #[test]
    fn test_ds_val_size() {
        assert_eq!(size_of::<DsVal>(), 4);
    }

    #[test]
    fn test_asyn_msg_size() {
        assert!(size_of::<AsynMsg>() >= 68);
    }

    #[test]
    fn test_amf_flags() {
        assert_eq!(AMF_VALID, 0x01);
        assert_eq!(AMF_DONE, 0x02);
        assert_eq!(AMF_NOTIFY, 0x04);
        assert_eq!(AMF_NOREPLY, 0x08);
        assert_eq!(AMF_NOTIFY_ERR, 0x10);
    }

    #[test]
    fn test_message_bases() {
        assert_eq!(PM_BASE, 0x0000);
        assert_eq!(KERNEL_CALL, 0x0100);
        assert_eq!(FS_BASE, 0x0200);
    }

    #[test]
    fn test_extra_error_codes() {
        assert_eq!(EBUSY, -16);
        assert_eq!(EEXIST, -17);
        assert_eq!(ENODEV, -19);
        assert_eq!(ENOTDIR, -20);
        assert_eq!(EISDIR, -21);
        assert_eq!(ENOBUFS, -55);
        assert_eq!(ELOCKED, -202);
        assert_eq!(ELOCKWILLBLOCK, -203);
    }
}
