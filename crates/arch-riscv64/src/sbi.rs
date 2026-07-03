//! RISC-V SBI (Supervisor Binary Interface) 1.0 calls.
//!
//! SBI provides the interface between S-mode (kernel) and M-mode
//! (OpenSBI firmware). Uses `ecall` with:
//!   a7 = extension ID
//!   a6 = function ID
//!   a0-a5 = arguments
//!   Return: a0 = error code, a1 = value

#![cfg(target_arch = "riscv64")]

/// SBI legacy extension IDs (SBI 0.1 / 1.0 legacy).
pub const SBI_SET_TIMER: u64 = 0;
pub const SBI_CONSOLE_PUTCHAR: u64 = 1;
pub const SBI_CONSOLE_GETCHAR: u64 = 2;
pub const SBI_CLEAR_IPI: u64 = 3;
pub const SBI_SEND_IPI: u64 = 4;
pub const SBI_REMOTE_FENCE_I: u64 = 5;
pub const SBI_REMOTE_SFENCE_VMA: u64 = 6;
pub const SBI_REMOTE_SFENCE_VMA_ASID: u64 = 7;
pub const SBI_SHUTDOWN: u64 = 8;

/// SBI 1.0 DBCN extension for debug console.
pub const SBI_EXT_DBCN: u64 = 0x4442434E;
pub const SBI_EXT_DBCN_CONSOLE_WRITE: u64 = 0;
pub const SBI_EXT_DBCN_CONSOLE_READ: u64 = 1;

/// SBI 1.0 SRST (System Reset) extension.
pub const SBI_EXT_SRST: u64 = 0x53525354;
pub const SBI_EXT_SRST_SYSTEM_RESET: u64 = 0;

/// SBI return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbiRet {
    pub error: u64,
    pub value: u64,
}

/// Perform a legacy SBI call (SBI v0.1 style, single function).
///
/// `extension` is the legacy extension ID (0-8).
/// `arg0`-`arg2` are the arguments.
#[inline]
pub unsafe fn sbi_legacy(extension: u64, arg0: u64, arg1: u64, arg2: u64) -> SbiRet {
    let error: u64;
    let value: u64;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") extension,
            in("a6") 0,
            in("a0") arg0,
            in("a1") arg1,
            in("a2") arg2,
            lateout("a0") error,
            lateout("a1") value,
            options(nomem, nostack),
        );
    }
    SbiRet { error, value }
}

/// Perform an SBI 1.0 extension call (with extension ID in a7, function in a6).
#[inline]
pub unsafe fn sbi_ecall(
    ext: u64,
    fid: u64,
    arg0: u64,
    arg1: u64,
    arg2: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> SbiRet {
    let error: u64;
    let value: u64;
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") ext,
            in("a6") fid,
            in("a0") arg0,
            in("a1") arg1,
            in("a2") arg2,
            in("a3") arg3,
            in("a4") arg4,
            in("a5") arg5,
            lateout("a0") error,
            lateout("a1") value,
            options(nomem, nostack),
        );
    }
    SbiRet { error, value }
}

// ── Legacy console functions ──────────────────────────────────────────────

/// Write a single character to the SBI debug console.
pub fn console_putchar(c: u8) {
    unsafe {
        sbi_legacy(SBI_CONSOLE_PUTCHAR, c as u64, 0, 0);
    }
}

/// Set the timer (next timer interrupt) via SBI.
/// `stime_value` is the absolute time in ticks (platform timebase).
pub fn set_timer(stime_value: u64) {
    unsafe {
        sbi_legacy(SBI_SET_TIMER, stime_value, 0, 0);
    }
}

/// Read a character from the SBI debug console.
/// Returns `None` if no character is available, `Some(c)` otherwise.
pub fn console_getchar() -> Option<u8> {
    let ret = unsafe { sbi_legacy(SBI_CONSOLE_GETCHAR, 0, 0, 0) };
    if ret.error != 0 || ret.value == u64::MAX {
        None
    } else {
        Some(ret.value as u8)
    }
}

// ── DBCN extension (SBI 1.0) ──────────────────────────────────────────────

/// Write bytes to the debug console (SBI 1.0 DBCN extension).
/// Returns the number of bytes written, or None on error.
pub fn debug_console_write(buf: &[u8]) -> Option<usize> {
    let phys_addr = buf.as_ptr() as u64;
    let ret = unsafe {
        sbi_ecall(
            SBI_EXT_DBCN,
            SBI_EXT_DBCN_CONSOLE_WRITE,
            phys_addr,
            buf.len() as u64,
            0,
            0,
            0,
            0,
        )
    };
    if ret.error != 0 {
        None
    } else {
        Some(ret.value as usize)
    }
}

// ── System reset ──────────────────────────────────────────────────────────

/// Shut down or reboot the system via SBI SRST.
pub fn system_reset(shutdown: bool) -> ! {
    let reset_type = if shutdown { 0u64 } else { 1u64 }; // 0 = shutdown, 1 = reboot
    unsafe {
        sbi_ecall(
            SBI_EXT_SRST,
            SBI_EXT_SRST_SYSTEM_RESET,
            reset_type,
            0,
            0,
            0,
            0,
            0,
        );
    }
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_legacy_constants() {
        assert_eq!(SBI_CONSOLE_PUTCHAR, 1);
        assert_eq!(SBI_CONSOLE_GETCHAR, 2);
        assert_eq!(SBI_SHUTDOWN, 8);
    }

    #[test]
    fn test_dbcn_constants() {
        assert_eq!(SBI_EXT_DBCN, 0x4442434E);
        assert_eq!(SBI_EXT_DBCN_CONSOLE_WRITE, 0);
    }

    #[test]
    fn test_srst_constant() {
        assert_eq!(SBI_EXT_SRST, 0x53525354);
    }
}
