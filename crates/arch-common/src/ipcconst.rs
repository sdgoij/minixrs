//! IPC constants from `minix/ipcconst.h`


pub const SEND: i32 = 1;
pub const RECEIVE: i32 = 2;
pub const SENDREC: i32 = 3;
pub const NOTIFY: i32 = 4;
pub const SENDNB: i32 = 5;
pub const MINIX_KERNINFO: i32 = 6;
pub const SENDA: i32 = 16;
pub const IPCNO_HIGHEST: i32 = SENDA;


pub const IPC_STATUS_CALL_SHIFT: i32 = 0;
pub const IPC_STATUS_CALL_MASK: i32 = 0xff;

pub fn ipc_status_call(status: i32) -> i32 {
    (status >> IPC_STATUS_CALL_SHIFT) & IPC_STATUS_CALL_MASK
}

pub fn ipc_status_call_to(call: i32) -> i32 {
    call << IPC_STATUS_CALL_SHIFT
}

pub const IPC_STATUS_FLAGS_SHIFT: i32 = 8;
pub const IPC_STATUS_FLAGS_MASK: i32 = 0xff00;

pub fn ipc_status_flags(status: i32) -> i32 {
    (status & IPC_STATUS_FLAGS_MASK) >> IPC_STATUS_FLAGS_SHIFT
}

pub fn ipc_status_flags_test(status: i32, flag: i32) -> bool {
    (status & flag) != 0
}

pub const IPC_FLG_MSG_FROM_KERNEL: i32 = 0x0001;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_call_constants() {
        assert_eq!(SEND, 1);
        assert_eq!(RECEIVE, 2);
        assert_eq!(SENDREC, 3);
        assert_eq!(NOTIFY, 4);
        assert_eq!(SENDNB, 5);
        assert_eq!(MINIX_KERNINFO, 6);
        assert_eq!(SENDA, 16);
    }

    #[test]
    fn test_ipc_status_roundtrip() {
        let call = SENDREC;
        let packed = ipc_status_call_to(call);
        assert_eq!(ipc_status_call(packed), call);
    }

    #[test]
    fn test_ipc_status_flags() {
        let status = ipc_status_call_to(SEND) | (IPC_FLG_MSG_FROM_KERNEL << IPC_STATUS_FLAGS_SHIFT);
        assert!(ipc_status_flags_test(status, IPC_FLG_MSG_FROM_KERNEL << 8));
    }
}
