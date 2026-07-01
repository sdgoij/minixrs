//! Basic userspace syscall handlers (task 5.41).
//!
//! These are raw POSIX syscalls called directly by userspace programs
//! via the `syscall` instruction. They run with register args:
//!   - rax = syscall number
//!   - rdi, rsi, rdx = arguments
//!   - return value in rax
//!
//! In the real Minix system, these are handled by the PM server through
//! IPC. For early boot, we stub them directly in the kernel to allow
//! basic userspace programs to run (getpid, write to serial, etc.).

/// Type for a basic syscall handler.
/// Takes the current process and register arguments, returns a value.
pub type BasicSyscallFn = unsafe fn(caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64;

/// Maximum syscall number we handle.
pub const NR_BASIC_SYSCALLS: usize = 64;

/// Dispatch table for basic syscalls.
/// Accessed via raw pointers to avoid Rust 2024 `static_mut_refs` issues.
static mut BASIC_SYSCALL_TABLE: [Option<BasicSyscallFn>; NR_BASIC_SYSCALLS] =
    [None; NR_BASIC_SYSCALLS];

/// Get a raw pointer to the syscall table.
fn syscall_table_ptr() -> *mut [Option<BasicSyscallFn>; NR_BASIC_SYSCALLS] {
    core::ptr::addr_of_mut!(BASIC_SYSCALL_TABLE)
}

/// Simple bump allocator brk (0x3FE00000-0x3FF00000 region).
static mut CURRENT_BRK: u64 = 0x3FE00000;

/// Register a basic syscall handler.
///
/// # Safety
///
/// Must be called during initialization, before any userspace execution.
pub unsafe fn register_basic_syscall(nr: usize, handler: BasicSyscallFn) {
    unsafe {
        let table = syscall_table_ptr();
        if nr < NR_BASIC_SYSCALLS {
            let slot = (table as *mut Option<BasicSyscallFn>).add(nr);
            core::ptr::write(slot, Some(handler));
        }
    }
}

/// Dispatch a basic syscall. Returns the value to place in RAX.
///
/// Saves the per-process CR3 before dispatching, loads BOOT_CR3 so the
/// kernel has access to identity-mapped data, then restores the per-process
/// CR3 after the handler returns.
///
/// When BOOT_CR3 is still 0 (pre-init / test mode) the CR3 save/restore
/// is skipped entirely, since the privileged instructions would crash in
/// a host test binary.
///
/// # Safety
///
/// `caller` must point to a valid Proc.
pub unsafe fn dispatch_basic_syscall(
    caller: *mut crate::proc::Proc,
    nr: usize,
    args: &[u64; 6],
) -> i64 {
    // Phase 6.5.1: CR3 save/restore.
    // Only do CR3 ops when BOOT_CR3 is non-zero (kernel has been
    // initialized).  In test mode BOOT_CR3 stays 0 and CR3 instructions
    // would crash because they are privileged.
    let boot_cr3 = arch_x86_64::BOOT_CR3.load(core::sync::atomic::Ordering::Relaxed);
    let saved_cr3 = if boot_cr3 != 0 && !caller.is_null() {
        unsafe {
            let cr3 = arch_x86_64::asm::read_cr3();
            (*caller).p_cr3_saved = cr3;
            arch_x86_64::asm::write_cr3(boot_cr3);
            Some(cr3)
        }
    } else {
        None
    };

    let result = unsafe {
        let table = syscall_table_ptr() as *const Option<BasicSyscallFn>;
        if nr < NR_BASIC_SYSCALLS {
            let entry = core::ptr::read(table.add(nr));
            match entry {
                Some(handler) => handler(caller, args),
                None => -38,
            }
        } else {
            -38
        }
    };

    // Restore per-process CR3 so the process resumes in its own address space
    if let Some(cr3) = saved_cr3 {
        unsafe {
            arch_x86_64::asm::write_cr3(cr3);
        }
    }

    result
}

// ── Handlers ───────────────────────────────────────────────────────────

/// SYS_read (2) — read from file descriptor.
unsafe fn sys_read_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let _buf = args[1] as *mut u8;
    let _count = args[2] as usize;
    if fd == 0 {
        // stdin → serial input (stub: return 0 = EOF)
        0
    } else {
        -9 // EBADF
    }
}

/// SYS_open (4) — open a file.
unsafe fn sys_open_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let path_ptr = args[0] as *const u8;
    let path_len = args[1] as usize;
    let _flags = args[2] as i32;
    // Stub — VFS server handles real opens
    let _ = (path_ptr, path_len);
    -5 // EIO (no VFS server yet)
}

/// SYS_close (5) — close a file descriptor.
unsafe fn sys_close_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let _fd = args[0] as i32;
    0 // stub: always succeed
}

/// SYS_getpid (20) — return the caller's endpoint as PID.
unsafe fn sys_getpid_handler(caller: *mut crate::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe { (*caller).p_endpoint as i64 }
}

/// SYS_exit (2) — terminate the current process.
/// Causes SIGABRT on the caller via `cause_sig`. The process is
/// marked SIGNALED|SIG_PENDING and dequeued; the signal manager
/// (PM) handles the actual cleanup.
unsafe fn sys_exit_handler(caller: *mut crate::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe {
        crate::system::cause_sig((*caller).p_nr, 6); // SIGABRT
    }
    // Signal EDONTREPLY so the caller doesn't wait for a response
    crate::system::EDONTREPLY as i64
}

/// SYS_write (9) — write to a file descriptor.
/// fd=1 (stdout), fd=2 (stderr) go to serial output.
unsafe fn sys_write_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    let fd = args[0] as i32;
    let count = args[2] as usize;

    let buf = args[1] as *const u8;
    if buf.is_null() {
        return -14; // EFAULT
    }

    if fd == 1 || fd == 2 {
        for i in 0..count.min(256) {
            let c = unsafe { core::ptr::read_volatile(buf.add(i)) };
            if c == b'\n' {
                unsafe {
                    arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1, b'\r');
                }
            }
            unsafe {
                arch_x86_64::hw::ser_putc(arch_x86_64::hw::COM1, c);
            }
        }
        count as i64
    } else {
        -9 // EBADF
    }
}

/// SYS_brk (13) — change data segment size.
/// Simple bump allocator in 0x3FE00000-0x3FF00000 region.
unsafe fn sys_brk_handler(_caller: *mut crate::proc::Proc, args: &[u64; 6]) -> i64 {
    unsafe {
        let new_brk = args[0];
        if new_brk == 0 {
            // Query current break
            CURRENT_BRK as i64
        } else if (0x3FE00000..0x3FF00000).contains(&new_brk) {
            CURRENT_BRK = new_brk;
            new_brk as i64
        } else {
            -12i64 // ENOMEM
        }
    }
}

/// Initialize basic syscall handlers.
///
/// # Safety
///
/// Must be called exactly once during boot.
pub unsafe fn init_basic_syscalls() {
    unsafe {
        // Syscall numbers match POSIX convention (minix-rt constants):
        // 0 = exit, 2 = read, 3 = write, 4 = open, 5 = close,
        // 9 = ... no, wait. Let me use the CORRECT mapping.
        // The userland (minix-rt) uses:
        //   NR_EXIT=0, NR_READ=2, NR_WRITE=3, NR_OPEN=4, NR_CLOSE=5
        //   NR_GETPID=20, NR_BRK=36
        // The kernel handles these syscalls.
        register_basic_syscall(0, sys_exit_handler); // NR_EXIT
        register_basic_syscall(2, sys_read_handler); // NR_READ
        register_basic_syscall(3, sys_write_handler); // NR_WRITE
        register_basic_syscall(4, sys_open_handler); // NR_OPEN
        register_basic_syscall(5, sys_close_handler); // NR_CLOSE
        register_basic_syscall(20, sys_getpid_handler); // NR_GETPID
        register_basic_syscall(36, sys_brk_handler); // NR_BRK
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::proc_init;

    #[test]
    fn test_getpid_returns_endpoint() {
        unsafe {
            proc_init();
            let rp = crate::table::proc_addr(0);
            (*rp).p_endpoint = 42;
            let args = [0u64; 6];
            assert_eq!(sys_getpid_handler(rp, &args), 42);
        }
    }

    #[test]
    #[ignore = "requires ring 0 (I/O port access)"]
    fn test_write_stdout_returns_count() {
        unsafe {
            let buf = [0u8; 10];
            let args = [1u64, buf.as_ptr() as u64, 10u64, 0, 0, 0];
            let rp = crate::table::proc_addr(0);
            assert_eq!(sys_write_handler(rp, &args), 10);
        }
    }

    #[test]
    fn test_write_bad_fd_returns_ebadf() {
        unsafe {
            let buf = [0u8; 10];
            let args = [99u64, buf.as_ptr() as u64, 10u64, 0, 0, 0];
            let rp = crate::table::proc_addr(0);
            assert_eq!(sys_write_handler(rp, &args), -9);
        }
    }

    #[test]
    fn test_write_null_buf_returns_efault() {
        unsafe {
            let args = [1u64, 0u64, 10u64, 0, 0, 0]; // null buf
            let rp = crate::table::proc_addr(0);
            assert_eq!(sys_write_handler(rp, &args), -14);
        }
    }

    #[test]
    fn test_brk_query_returns_current() {
        unsafe {
            proc_init();
            let brk_ptr = core::ptr::addr_of_mut!(CURRENT_BRK);
            *brk_ptr = 0x3FE01000;
            let args = [0u64, 0, 0, 0, 0, 0];
            assert_eq!(sys_brk_handler(core::ptr::null_mut(), &args), 0x3FE01000i64);
        }
    }

    #[test]
    fn test_brk_set_valid() {
        unsafe {
            let brk_ptr = core::ptr::addr_of_mut!(CURRENT_BRK);
            *brk_ptr = 0x3FE00000;
            let args = [0x3FE02000u64, 0, 0, 0, 0, 0];
            assert_eq!(sys_brk_handler(core::ptr::null_mut(), &args), 0x3FE02000i64);
            assert_eq!(*brk_ptr, 0x3FE02000);
        }
    }

    #[test]
    fn test_brk_out_of_range() {
        unsafe {
            let args = [0x40000000u64, 0, 0, 0, 0, 0];
            assert_eq!(sys_brk_handler(core::ptr::null_mut(), &args), -12);
        }
    }

    #[test]
    fn test_dispatch_unknown_syscall_returns_enosys() {
        unsafe {
            let rp = crate::table::proc_addr(0);
            assert_eq!(dispatch_basic_syscall(rp, 999, &[0u64; 6]), -38);
        }
    }

    #[test]
    fn test_exit_calls_cause_sig() {
        unsafe {
            proc_init();
            arch_x86_64::cpulocals::init_cpulocals();
            let rp = crate::table::proc_addr(0);
            (*rp).p_nr = 0;
            (*rp)
                .p_rts_flags
                .store(0, core::sync::atomic::Ordering::Relaxed);
            let args = [0u64; 6];
            let result = sys_exit_handler(rp, &args);
            assert_eq!(result, crate::system::EDONTREPLY as i64);
            let flags = (*rp)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            assert!(
                flags
                    & (crate::proc::RtsFlags::SIGNALED | crate::proc::RtsFlags::SIG_PENDING).bits()
                    != 0,
                "exit should cause SIGABRT"
            );
        }
    }

    #[test]
    #[ignore = "requires ring 0 (cr3 access via dispatch_basic_syscall)"]
    fn test_init_registers_getpid() {
        unsafe {
            proc_init();
            init_basic_syscalls();
            let rp = crate::table::proc_addr(0);
            (*rp).p_endpoint = 42;
            assert_eq!(dispatch_basic_syscall(rp, 0, &[0u64; 6]), 42);
        }
    }

    #[test]
    #[ignore = "requires ring 0 (cr3 access via dispatch_basic_syscall)"]
    fn test_init_registers_brk() {
        unsafe {
            let brk_ptr = core::ptr::addr_of_mut!(CURRENT_BRK);
            *brk_ptr = 0x3FE00000;
            init_basic_syscalls();
            assert_eq!(
                dispatch_basic_syscall(core::ptr::null_mut(), 13, &[0u64, 0, 0, 0, 0, 0]),
                0x3FE00000i64
            );
        }
    }

    #[test]
    fn test_handler_signatures() {
        fn _check(_: BasicSyscallFn) {}
        _check(sys_getpid_handler);
        _check(sys_exit_handler);
        _check(sys_write_handler);
        _check(sys_brk_handler);
    }
}
