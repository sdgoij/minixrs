//! Constants from `minix/const.h`

/// Maximum number of processes.
pub const NR_PROCS: usize = 256;

/// Maximum number of kernel tasks.
pub const NR_TASKS: usize = 8;

/// Maximum number of system processes.
pub const NR_SYS_PROCS: usize = 64;

/// Number of memory chunks.
pub const NR_MEMS: usize = 16;

/// Click size (page size).
pub const CLICK_SIZE: usize = 4096;
pub const CLICK_SHIFT: u32 = 12;

/// Number of consoles.
pub const NR_CONS: usize = 4;

/// Number of RS232 serial lines.
pub const NR_RS_LINES: usize = 2;

/// Number of pseudo-terminals.
pub const NR_PTYS: usize = 4;

/// Number of scheduling queues.
pub const NR_SCHED_QUEUES: usize = 16;

// Signal delivery (SIGNALS.md Phase 4)

/// Magic value validating a sigframe on the target's stack.
pub const SC_MAGIC: u64 = 0xC0FFEE1;

// sigmsg (PM → kernel via SYS_SIGSEND), matching C `struct sigmsg`:
//   signo@0 (i32), mask@8 (16 bytes), sighandler@24 (u64),
//   sigreturn@32 (u64), stkptr@40 (u64) — 48 bytes total.
pub const SIGMSG_SIGNO_OFF: usize = 0;
pub const SIGMSG_MASK_OFF: usize = 8;
pub const SIGMSG_HANDLER_OFF: usize = 24;
pub const SIGMSG_SIGRETURN_OFF: usize = 32;
pub const SIGMSG_STKPTR_OFF: usize = 40;
pub const SIGMSG_SIZE: usize = 48;

// SYS_SIGSEND / SYS_SIGRETURN message layout (m_sigcalls):
//   endpt@16, sig@20, sigctx@24.
pub const SIGCALLS_ENDPT_OFF: usize = 16;
pub const SIGCALLS_SIG_OFF: usize = 20;
pub const SIGCALLS_SIGCTX_OFF: usize = 24;

// Sigframe on the target's stack. Layout differs per arch (the saved
// register file is arch-specific); the shared fields (mask / signal /
// magic) sit at the offsets below. The saved p_reg array occupies
// `SIZE - MASK_OFF` bytes at `REGS_OFF`.
#[cfg(target_arch = "x86_64")]
pub mod sigframe {
    pub const SIZE: usize = 296;
    pub const REGS_OFF: usize = 8; // [8..264) = saved p_reg; [0..8) = handler return addr
    pub const MASK_OFF: usize = 264;
    pub const SIGNAL_OFF: usize = 280;
    pub const MAGIC_OFF: usize = 288;
}

#[cfg(target_arch = "riscv64")]
pub mod sigframe {
    pub const SIZE: usize = 296;
    pub const REGS_OFF: usize = 0; // [0..256) = saved p_reg
    pub const MASK_OFF: usize = 256;
    pub const SIGNAL_OFF: usize = 272;
    pub const SC_P_OFF: usize = 280; // frame base, for the trampoline
    pub const MAGIC_OFF: usize = 288;
}

#[cfg(target_arch = "aarch64")]
pub mod sigframe {
    pub const SIZE: usize = 328;
    pub const REGS_OFF: usize = 0; // [0..288) = saved p_reg
    pub const MASK_OFF: usize = 288;
    pub const SIGNAL_OFF: usize = 304;
    pub const SC_P_OFF: usize = 312; // frame base, for the trampoline
    pub const MAGIC_OFF: usize = 320;
}

/// Number of I/O ranges per privilege.
pub const NR_IO_RANGE: usize = 16;

/// Number of memory ranges per privilege.
pub const NR_MEM_RANGE: usize = 16;

/// Number of IRQ ranges per privilege.
pub const NR_IRQ: usize = 8;

pub const MAX_INODE_NR: u64 = 65535;
pub const MAX_FILE_POS: u64 = 0x7fffffffffffffff;
pub const UMAX_FILE_POS: u64 = 0x7fffffffffffffff;
pub const MAX_SYM_LOOPS: usize = 8;

pub const I_TYPE: u16 = 0o170000;
pub const I_UNIX_SOCKET: u16 = 0o140000;
pub const I_SYMBOLIC_LINK: u16 = 0o120000;
pub const I_REGULAR: u16 = 0o100000;
pub const I_BLOCK_SPECIAL: u16 = 0o060000;
pub const I_DIRECTORY: u16 = 0o040000;
pub const I_CHAR_SPECIAL: u16 = 0o020000;
pub const I_NAMED_PIPE: u16 = 0o010000;
pub const I_SET_UID_BIT: u16 = 0o004000;
pub const I_SET_GID_BIT: u16 = 0o002000;
pub const I_SET_STCKY_BIT: u16 = 0o001000;
pub const ALL_MODES: u16 = 0x0fff;
pub const RWX_MODES: u16 = 0o0777;
pub const R_BIT: u16 = 0o0444;
pub const W_BIT: u16 = 0o0222;
pub const X_BIT: u16 = 0o0111;

pub const PMAGIC: u32 = 0x0BEEF;
pub const NO_BLOCK: u64 = !0u64;
pub const NO_ENTRY: i32 = -1;
pub const NO_ZONE: u64 = !0u64;
pub const NO_DEV: i32 = 0xffff;
pub const NO_LINK: i32 = 0;

pub const PREEMPTIBLE: u16 = 0x0001;
pub const BILLABLE: u16 = 0x0002;
pub const DYN_PRIV_ID: u16 = 0x0004;
pub const SYS_PROC: u16 = 0x0008;
pub const CHECK_IO_PORT: u16 = 0x0010;
pub const CHECK_IRQ: u16 = 0x0020;
pub const CHECK_MEM: u16 = 0x0040;
pub const ROOT_SYS_PROC: u16 = 0x0080;
pub const VM_SYS_PROC: u16 = 0x0100;

pub const VM_D: u32 = 0x0001;
pub const VM_GRANT: u32 = 0x0002;
pub const PHYS_SEG: u32 = 0x0004;
pub const SEGMENT_TYPE: u32 = 0x7;
pub const SEGMENT_INDEX: u32 = 3;

pub const VERBOSEBOOT_BASIC: u32 = 0x01;
pub const VERBOSEBOOT_EXTRA: u32 = 0x02;

pub const MKF_I386_INTEL_SYSENTER: u32 = 0x00000001;
pub const MKF_I386_AMD_SYSCALL: u32 = 0x00000002;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_limits() {
        assert_eq!(NR_PROCS, 256);
        assert_eq!(NR_TASKS, 8);
        assert_eq!(NR_SYS_PROCS, 64);
        assert_eq!(NR_MEMS, 16);
    }

    #[test]
    fn test_click_constants() {
        assert_eq!(CLICK_SIZE, 4096);
        assert_eq!(CLICK_SHIFT, 12);
    }

    #[test]
    fn test_file_mode_bits() {
        assert_eq!(I_TYPE, 0o170000);
        assert_eq!(I_REGULAR, 0o100000);
        assert_eq!(I_DIRECTORY, 0o040000);
        assert_eq!(R_BIT, 0o0444);
        assert_eq!(W_BIT, 0o0222);
        assert_eq!(X_BIT, 0o0111);
    }

    #[test]
    fn test_special_values() {
        assert_eq!(PMAGIC, 0x0BEEF);
        assert_eq!(NO_ENTRY, -1);
        assert_eq!(NO_DEV, 0xffff);
    }
}
