//! Message constant definitions from `minix/com.h`
//!
//! This is the single most important header in the Minix kernel —
//! it defines all process endpoints, subsystem message bases, system
//! call numbers, and IPC message field names. ABI-critical: every
//! constant must match the C `#define` value exactly.
use crate::types::Endpoint;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 1: Kernel Task & Process Endpoints
// ═════════════════════════════════════════════════════════════════════════

pub const ASYNCM: Endpoint = -5;
pub const IDLE: Endpoint = -4;
pub const CLOCK: Endpoint = -3;
pub const SYSTEM: Endpoint = -2;
pub const KERNEL: Endpoint = -1;
pub const HARDWARE: Endpoint = KERNEL;

pub const MAX_NR_TASKS: u32 = 1023;
pub const NR_TASKS: u32 = 5;

pub const PM_PROC_NR: Endpoint = 0;
pub const VFS_PROC_NR: Endpoint = 1;
pub const RS_PROC_NR: Endpoint = 2;
pub const MEM_PROC_NR: Endpoint = 3;
pub const SCHED_PROC_NR: Endpoint = 4;
pub const TTY_PROC_NR: Endpoint = 5;
pub const DS_PROC_NR: Endpoint = 6;
pub const MFS_PROC_NR: Endpoint = 7;
pub const VM_PROC_NR: Endpoint = 8;
pub const PFS_PROC_NR: Endpoint = 9;

pub const LAST_SPECIAL_PROC_NR: Endpoint = 10;
pub const INIT_PROC_NR: Endpoint = LAST_SPECIAL_PROC_NR;
pub const NR_BOOT_MODULES: Endpoint = INIT_PROC_NR + 1;

pub const ROOT_SYS_PROC_NR: Endpoint = RS_PROC_NR;
pub const ROOT_USR_PROC_NR: Endpoint = INIT_PROC_NR;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 2: Notification Types
// ═════════════════════════════════════════════════════════════════════════

pub const NOTIFY_MESSAGE: u32 = 0x1000;

/// Check if an IPC status indicates a notification.
pub fn is_ipc_notify(ipc_status: i32) -> bool {
    crate::ipcconst::ipc_status_call(ipc_status) == crate::ipcconst::NOTIFY
}

/// Old-style notification check.
pub fn is_notify(a: i32) -> bool {
    ((a - NOTIFY_MESSAGE as i32) as u32) < 0x100
}

/// Check if IPC status is asynchronous (notify or senda).
pub fn is_ipc_asynch(ipc_status: i32) -> bool {
    is_ipc_notify(ipc_status)
        || crate::ipcconst::ipc_status_call(ipc_status) == crate::ipcconst::SENDA
}

// ═════════════════════════════════════════════════════════════════════════
// Chapter 3: Bus Controller Driver Messages
// ═════════════════════════════════════════════════════════════════════════

pub const BUSC_RQ_BASE: u32 = 0x300;
pub const BUSC_RS_BASE: u32 = 0x380;

pub const BUSC_PCI_INIT: u32 = BUSC_RQ_BASE;
pub const BUSC_PCI_FIRST_DEV: u32 = BUSC_RQ_BASE + 1;
pub const BUSC_PCI_NEXT_DEV: u32 = BUSC_RQ_BASE + 2;
pub const BUSC_PCI_FIND_DEV: u32 = BUSC_RQ_BASE + 3;
pub const BUSC_PCI_IDS: u32 = BUSC_RQ_BASE + 4;
pub const BUSC_PCI_RESERVE: u32 = BUSC_RQ_BASE + 7;
pub const BUSC_PCI_ATTR_R8: u32 = BUSC_RQ_BASE + 8;
pub const BUSC_PCI_ATTR_R16: u32 = BUSC_RQ_BASE + 9;
pub const BUSC_PCI_ATTR_R32: u32 = BUSC_RQ_BASE + 10;
pub const BUSC_PCI_ATTR_W8: u32 = BUSC_RQ_BASE + 11;
pub const BUSC_PCI_ATTR_W16: u32 = BUSC_RQ_BASE + 12;
pub const BUSC_PCI_ATTR_W32: u32 = BUSC_RQ_BASE + 13;
pub const BUSC_PCI_RESCAN: u32 = BUSC_RQ_BASE + 14;
pub const BUSC_PCI_DEV_NAME_S: u32 = BUSC_RQ_BASE + 15;
pub const BUSC_PCI_SLOT_NAME_S: u32 = BUSC_RQ_BASE + 16;
pub const BUSC_PCI_SET_ACL: u32 = BUSC_RQ_BASE + 17;
pub const BUSC_PCI_DEL_ACL: u32 = BUSC_RQ_BASE + 18;
pub const BUSC_PCI_GET_BAR: u32 = BUSC_RQ_BASE + 19;
pub const IOMMU_MAP: u32 = BUSC_RQ_BASE + 32;

pub const BUSC_I2C_RESERVE: u32 = BUSC_RQ_BASE + 64;
pub const BUSC_I2C_EXEC: u32 = BUSC_RQ_BASE + 65;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 4: Data Link Layer (Networking) Messages
// ═════════════════════════════════════════════════════════════════════════

pub const DL_RQ_BASE: u32 = 0x200;
pub const DL_RS_BASE: u32 = 0x280;

pub fn is_dl_rq(typ: u32) -> bool {
    (typ & !0x7f) == DL_RQ_BASE
}
pub fn is_dl_rs(typ: u32) -> bool {
    (typ & !0x7f) == DL_RS_BASE
}

pub const DL_CONF: u32 = DL_RQ_BASE;
pub const DL_GETSTAT_S: u32 = DL_RQ_BASE + 1;
pub const DL_WRITEV_S: u32 = DL_RQ_BASE + 2;
pub const DL_READV_S: u32 = DL_RQ_BASE + 3;

pub const DL_CONF_REPLY: u32 = DL_RS_BASE;
pub const DL_STAT_REPLY: u32 = DL_RS_BASE + 1;
pub const DL_TASK_REPLY: u32 = DL_RS_BASE + 2;

pub const DL_NOFLAGS: u32 = 0x00;
pub const DL_PACK_SEND: u32 = 0x01;
pub const DL_PACK_RECV: u32 = 0x02;

pub const DL_NOMODE: u32 = 0x0;
pub const DL_PROMISC_REQ: u32 = 0x1;
pub const DL_MULTI_REQ: u32 = 0x2;
pub const DL_BROAD_REQ: u32 = 0x4;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 5: System Call Numbers (SYSTASK / KERNEL_CALL)
// ═════════════════════════════════════════════════════════════════════════

pub const KERNEL_CALL: u32 = 0x600;

/// System call number constants.
pub mod sys {
    use super::KERNEL_CALL;

    pub const FORK: u32 = KERNEL_CALL;
    pub const EXEC: u32 = KERNEL_CALL + 1;
    pub const CLEAR: u32 = KERNEL_CALL + 2;
    pub const SCHEDULE: u32 = KERNEL_CALL + 3;
    pub const PRIVCTL: u32 = KERNEL_CALL + 4;
    pub const TRACE: u32 = KERNEL_CALL + 5;
    pub const KILL: u32 = KERNEL_CALL + 6;
    pub const GETKSIG: u32 = KERNEL_CALL + 7;
    pub const ENDKSIG: u32 = KERNEL_CALL + 8;
    pub const SIGSEND: u32 = KERNEL_CALL + 9;
    pub const SIGRETURN: u32 = KERNEL_CALL + 10;
    pub const MEMSET: u32 = KERNEL_CALL + 13;
    pub const UMAP: u32 = KERNEL_CALL + 14;
    pub const VIRCOPY: u32 = KERNEL_CALL + 15;
    pub const PHYSCOPY: u32 = KERNEL_CALL + 16;
    pub const UMAP_REMOTE: u32 = KERNEL_CALL + 17;
    pub const VUMAP: u32 = KERNEL_CALL + 18;
    pub const IRQCTL: u32 = KERNEL_CALL + 19;
    pub const INT86: u32 = KERNEL_CALL + 20;
    pub const DEVIO: u32 = KERNEL_CALL + 21;
    pub const SDEVIO: u32 = KERNEL_CALL + 22;
    pub const VDEVIO: u32 = KERNEL_CALL + 23;
    pub const SETALARM: u32 = KERNEL_CALL + 24;
    pub const TIMES: u32 = KERNEL_CALL + 25;
    pub const GETINFO: u32 = KERNEL_CALL + 26;
    pub const ABORT: u32 = KERNEL_CALL + 27;
    pub const IOPENABLE: u32 = KERNEL_CALL + 28;
    pub const SAFECOPYFROM: u32 = KERNEL_CALL + 31;
    pub const SAFECOPYTO: u32 = KERNEL_CALL + 32;
    pub const VSAFECOPY: u32 = KERNEL_CALL + 33;
    pub const SETGRANT: u32 = KERNEL_CALL + 34;
    pub const READBIOS: u32 = KERNEL_CALL + 35;
    pub const SPROF: u32 = KERNEL_CALL + 36;
    pub const CPROF: u32 = KERNEL_CALL + 37;
    pub const PROFBUF: u32 = KERNEL_CALL + 38;
    pub const STIME: u32 = KERNEL_CALL + 39;
    pub const SETTIME: u32 = KERNEL_CALL + 40;
    pub const VMCTL: u32 = KERNEL_CALL + 43;
    pub const DIAGCTL: u32 = KERNEL_CALL + 44;
    pub const VTIMER: u32 = KERNEL_CALL + 45;
    pub const RUNCTL: u32 = KERNEL_CALL + 46;
    pub const GETMCONTEXT: u32 = KERNEL_CALL + 50;
    pub const SETMCONTEXT: u32 = KERNEL_CALL + 51;
    pub const UPDATE: u32 = KERNEL_CALL + 52;
    pub const EXIT: u32 = KERNEL_CALL + 53;
    pub const SCHEDCTL: u32 = KERNEL_CALL + 54;
    pub const STATECTL: u32 = KERNEL_CALL + 55;
    pub const SAFEMEMSET: u32 = KERNEL_CALL + 56;
    pub const PADCONF: u32 = KERNEL_CALL + 57;
}

pub const NR_SYS_CALLS: u32 = 58;

/// Basic kernel calls allowed to every system process.
pub const SYS_BASIC_CALLS: [u32; 12] = [
    sys::EXIT,
    sys::SAFECOPYFROM,
    sys::SAFECOPYTO,
    sys::VSAFECOPY,
    sys::GETINFO,
    sys::TIMES,
    sys::SETALARM,
    sys::SETGRANT,
    sys::PROFBUF,
    sys::DIAGCTL,
    sys::STATECTL,
    sys::SAFEMEMSET,
];

// ═════════════════════════════════════════════════════════════════════════
// Chapter 6: Device I/O Direction/Size Flags
// ═════════════════════════════════════════════════════════════════════════

pub const DIO_INPUT: u32 = 0x001;
pub const DIO_OUTPUT: u32 = 0x002;
pub const DIO_DIRMASK: u32 = 0x00f;
pub const DIO_BYTE: u32 = 0x010;
pub const DIO_WORD: u32 = 0x020;
pub const DIO_LONG: u32 = 0x030;
pub const DIO_TYPEMASK: u32 = 0x0f0;
pub const DIO_SAFE: u32 = 0x100;
pub const DIO_SAFEMASK: u32 = 0xf00;

pub const DIO_INPUT_BYTE: u32 = DIO_INPUT | DIO_BYTE;
pub const DIO_INPUT_WORD: u32 = DIO_INPUT | DIO_WORD;
pub const DIO_INPUT_LONG: u32 = DIO_INPUT | DIO_LONG;
pub const DIO_OUTPUT_BYTE: u32 = DIO_OUTPUT | DIO_BYTE;
pub const DIO_OUTPUT_WORD: u32 = DIO_OUTPUT | DIO_WORD;
pub const DIO_OUTPUT_LONG: u32 = DIO_OUTPUT | DIO_LONG;
pub const DIO_SAFE_INPUT_BYTE: u32 = DIO_INPUT | DIO_BYTE | DIO_SAFE;
pub const DIO_SAFE_INPUT_WORD: u32 = DIO_INPUT | DIO_WORD | DIO_SAFE;
pub const DIO_SAFE_INPUT_LONG: u32 = DIO_INPUT | DIO_LONG | DIO_SAFE;
pub const DIO_SAFE_OUTPUT_BYTE: u32 = DIO_OUTPUT | DIO_BYTE | DIO_SAFE;
pub const DIO_SAFE_OUTPUT_WORD: u32 = DIO_OUTPUT | DIO_WORD | DIO_SAFE;
pub const DIO_SAFE_OUTPUT_LONG: u32 = DIO_OUTPUT | DIO_LONG | DIO_SAFE;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 7: IRQ Control
// ═════════════════════════════════════════════════════════════════════════

pub const IRQ_SETPOLICY: u32 = 1;
pub const IRQ_RMPOLICY: u32 = 2;
pub const IRQ_ENABLE: u32 = 3;
pub const IRQ_DISABLE: u32 = 4;
pub const IRQ_REENABLE: u32 = 0x001;
pub const IRQ_BYTE: u32 = 0x100;
pub const IRQ_WORD: u32 = 0x200;
pub const IRQ_LONG: u32 = 0x400;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 8: SYS_GETINFO Subcodes
// ═════════════════════════════════════════════════════════════════════════

pub const GET_KINFO: u32 = 0;
pub const GET_IMAGE: u32 = 1;
pub const GET_PROCTAB: u32 = 2;
pub const GET_RANDOMNESS: u32 = 3;
pub const GET_MONPARAMS: u32 = 4;
pub const GET_KENV: u32 = 5;
pub const GET_IRQHOOKS: u32 = 6;
pub const GET_PRIVTAB: u32 = 8;
pub const GET_KADDRESSES: u32 = 9;
pub const GET_SCHEDINFO: u32 = 10;
pub const GET_PROC: u32 = 11;
pub const GET_MACHINE: u32 = 12;
pub const GET_LOCKTIMING: u32 = 13;
pub const GET_BIOSBUFFER: u32 = 14;
pub const GET_LOADINFO: u32 = 15;
pub const GET_IRQACTIDS: u32 = 16;
pub const GET_PRIV: u32 = 17;
pub const GET_HZ: u32 = 18;
pub const GET_WHOAMI: u32 = 19;
pub const GET_RANDOMNESS_BIN: u32 = 20;
pub const GET_IDLETSC: u32 = 21;
pub const GET_CPUINFO: u32 = 23;
pub const GET_REGS: u32 = 24;
pub const GET_RUSAGE: u32 = 25;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 9: SYS_PRIVCTL Subfunctions
// ═════════════════════════════════════════════════════════════════════════

pub const SYS_PRIV_ALLOW: u32 = 1;
pub const SYS_PRIV_DISALLOW: u32 = 2;
pub const SYS_PRIV_SET_SYS: u32 = 3;
pub const SYS_PRIV_SET_USER: u32 = 4;
pub const SYS_PRIV_ADD_IO: u32 = 5;
pub const SYS_PRIV_ADD_MEM: u32 = 6;
pub const SYS_PRIV_ADD_IRQ: u32 = 7;
pub const SYS_PRIV_QUERY_MEM: u32 = 8;
pub const SYS_PRIV_UPDATE_SYS: u32 = 9;
pub const SYS_PRIV_YIELD: u32 = 10;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 10: Exec & Fork Flags
// ═════════════════════════════════════════════════════════════════════════

pub const PMEF_AUXVECTORS: u32 = 20;
pub const PFF_VMINHIBIT: u32 = 0x01;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 11: SYS_VMCTL - VM Control Subcodes
// ═════════════════════════════════════════════════════════════════════════

pub const VMCTL_CLEAR_PAGEFAULT: u32 = 12;
pub const VMCTL_GET_PDBR: u32 = 13;
pub const VMCTL_MEMREQ_GET: u32 = 14;
pub const VMCTL_MEMREQ_REPLY: u32 = 15;
pub const VMCTL_NOPAGEZERO: u32 = 18;
pub const VMCTL_I386_KERNELLIMIT: u32 = 19;
pub const VMCTL_I386_INVLPG: u32 = 25;
pub const VMCTL_FLUSHTLB: u32 = 26;
pub const VMCTL_KERN_PHYSMAP: u32 = 27;
pub const VMCTL_KERN_MAP_REPLY: u32 = 28;
pub const VMCTL_SETADDRSPACE: u32 = 29;
pub const VMCTL_VMINHIBIT_SET: u32 = 30;
pub const VMCTL_VMINHIBIT_CLEAR: u32 = 31;
pub const VMCTL_CLEARMAPCACHE: u32 = 32;
pub const VMCTL_BOOTINHIBIT_CLEAR: u32 = 33;

pub const VMMF_UNCACHED: u32 = 1 << 0;
pub const VMMF_USER: u32 = 1 << 1;
pub const VMMF_WRITE: u32 = 1 << 2;
pub const VMMF_GLO: u32 = 1 << 3;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 12: SYS_DIAGCTL Codes
// ═════════════════════════════════════════════════════════════════════════

pub const DIAGCTL_CODE_DIAG: u32 = 1;
pub const DIAGCTL_CODE_STACKTRACE: u32 = 2;
pub const DIAGCTL_CODE_REGISTER: u32 = 3;
pub const DIAGCTL_CODE_UNREGISTER: u32 = 4;
pub const DIAG_BUFSIZE: u32 = 80 * 25;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 13: SYS_VTIMER, SYS_RUNCTL, SYS_UPDATE, SYS_STATECTL, SYS_SCHEDCTL
// ═════════════════════════════════════════════════════════════════════════

pub const VT_VIRTUAL: u32 = 1;
pub const VT_PROF: u32 = 2;

pub const RC_STOP: u32 = 0;
pub const RC_RESUME: u32 = 1;
pub const RC_DELAY: u32 = 1;

pub const SYS_STATE_CLEAR_IPC_REFS: u32 = 1;

pub const SCHEDCTL_FLAG_KERNEL: u32 = 1;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 14: RS (Reincarnation Server) Messages
// ═════════════════════════════════════════════════════════════════════════

pub const RS_RQ_BASE: u32 = 0x700;

pub const RS_UP: u32 = RS_RQ_BASE;
pub const RS_DOWN: u32 = RS_RQ_BASE + 1;
pub const RS_REFRESH: u32 = RS_RQ_BASE + 2;
pub const RS_RESTART: u32 = RS_RQ_BASE + 3;
pub const RS_SHUTDOWN: u32 = RS_RQ_BASE + 4;
pub const RS_UPDATE: u32 = RS_RQ_BASE + 5;
pub const RS_CLONE: u32 = RS_RQ_BASE + 6;
pub const RS_EDIT: u32 = RS_RQ_BASE + 7;
pub const RS_LOOKUP: u32 = RS_RQ_BASE + 8;
pub const RS_GETSYSINFO: u32 = RS_RQ_BASE + 9;
pub const RS_INIT: u32 = RS_RQ_BASE + 20;
pub const RS_LU_PREPARE: u32 = RS_RQ_BASE + 21;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 15: DS (Data Store) Messages
// ═════════════════════════════════════════════════════════════════════════

pub const DS_RQ_BASE: u32 = 0x800;

pub const DS_PUBLISH: u32 = DS_RQ_BASE;
pub const DS_RETRIEVE: u32 = DS_RQ_BASE + 1;
pub const DS_SUBSCRIBE: u32 = DS_RQ_BASE + 2;
pub const DS_CHECK: u32 = DS_RQ_BASE + 3;
pub const DS_DELETE: u32 = DS_RQ_BASE + 4;
pub const DS_SNAPSHOT: u32 = DS_RQ_BASE + 5;
pub const DS_RETRIEVE_LABEL: u32 = DS_RQ_BASE + 6;
pub const DS_GETSYSINFO: u32 = DS_RQ_BASE + 7;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 16: VFS ↔ PM Messages
// ═════════════════════════════════════════════════════════════════════════

pub const VFS_PM_RQ_BASE: u32 = 0x900;
pub const VFS_PM_RS_BASE: u32 = 0x980;

pub fn is_vfs_pm_rq(typ: u32) -> bool {
    (typ & !0x7f) == VFS_PM_RQ_BASE
}
pub fn is_vfs_pm_rs(typ: u32) -> bool {
    (typ & !0x7f) == VFS_PM_RS_BASE
}

// Requests
pub const VFS_PM_INIT: u32 = VFS_PM_RQ_BASE;
pub const VFS_PM_SETUID: u32 = VFS_PM_RQ_BASE + 1;
pub const VFS_PM_SETGID: u32 = VFS_PM_RQ_BASE + 2;
pub const VFS_PM_SETSID: u32 = VFS_PM_RQ_BASE + 3;
pub const VFS_PM_EXIT: u32 = VFS_PM_RQ_BASE + 4;
pub const VFS_PM_DUMPCORE: u32 = VFS_PM_RQ_BASE + 5;
pub const VFS_PM_EXEC: u32 = VFS_PM_RQ_BASE + 6;
pub const VFS_PM_FORK: u32 = VFS_PM_RQ_BASE + 7;
pub const VFS_PM_SRV_FORK: u32 = VFS_PM_RQ_BASE + 8;
pub const VFS_PM_UNPAUSE: u32 = VFS_PM_RQ_BASE + 9;
pub const VFS_PM_REBOOT: u32 = VFS_PM_RQ_BASE + 10;
pub const VFS_PM_SETGROUPS: u32 = VFS_PM_RQ_BASE + 11;

// Replies
pub const VFS_PM_SETUID_REPLY: u32 = VFS_PM_RS_BASE + 1;
pub const VFS_PM_SETGID_REPLY: u32 = VFS_PM_RS_BASE + 2;
pub const VFS_PM_SETSID_REPLY: u32 = VFS_PM_RS_BASE + 3;
pub const VFS_PM_EXIT_REPLY: u32 = VFS_PM_RS_BASE + 4;
pub const VFS_PM_CORE_REPLY: u32 = VFS_PM_RS_BASE + 5;
pub const VFS_PM_EXEC_REPLY: u32 = VFS_PM_RS_BASE + 6;
pub const VFS_PM_FORK_REPLY: u32 = VFS_PM_RS_BASE + 7;
pub const VFS_PM_SRV_FORK_REPLY: u32 = VFS_PM_RS_BASE + 8;
pub const VFS_PM_UNPAUSE_REPLY: u32 = VFS_PM_RS_BASE + 9;
pub const VFS_PM_REBOOT_REPLY: u32 = VFS_PM_RS_BASE + 10;
pub const VFS_PM_SETGROUPS_REPLY: u32 = VFS_PM_RS_BASE + 11;

// Message field names (these map to m7 variants in the Message union)
pub mod vfs_pm {
    pub const ENDPT: i32 = 0; // m7_i1

    pub const SLOT: i32 = 1; // m7_i2
    pub const PID: i32 = 2; // m7_i3

    pub const EID: i32 = 1; // m7_i2 (alias in SETUID/SETGID context)
    pub const RID: i32 = 2; // m7_i3

    pub const PATH: i32 = 0; // m7_p1
    pub const PATH_LEN: i32 = 1; // m7_i2
    pub const FRAME: i32 = 1; // m7_p2
    pub const FRAME_LEN: i32 = 2; // m7_i3
    pub const PS_STR: i32 = 4; // m7_i5

    pub const STATUS: i32 = 1; // m7_i2
    pub const PC: i32 = 0; // m7_p1
    pub const NEWSP: i32 = 1; // m7_p2
    pub const NEWPS_STR: i32 = 4; // m7_i5

    pub const PENDPT: i32 = 1; // m7_i2
    pub const CPID: i32 = 2; // m7_i3
    pub const REUID: i32 = 3; // m7_i4
    pub const REGID: i32 = 4; // m7_i5

    pub const TERM_SIG: i32 = 1; // m7_i2
}

// ═════════════════════════════════════════════════════════════════════════
// Chapter 17: Common Request Base
// ═════════════════════════════════════════════════════════════════════════

pub const COMMON_RQ_BASE: u32 = 0xE00;

pub const SIGS_SIGNAL_RECEIVED: u32 = COMMON_RQ_BASE;
pub const COMMON_REQ_GCOV_DATA: u32 = COMMON_RQ_BASE + 1;
pub const COMMON_REQ_FI_CTL: u32 = COMMON_RQ_BASE + 2;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 18: VM Server Messages
// ═════════════════════════════════════════════════════════════════════════

pub const VM_RQ_BASE: u32 = 0xC00;

pub const VM_EXIT: u32 = VM_RQ_BASE;
pub const VM_FORK: u32 = VM_RQ_BASE + 1;
pub const VM_BRK: u32 = VM_RQ_BASE + 2;
pub const VM_EXEC_NEWMEM: u32 = VM_RQ_BASE + 3;
pub const VM_WILLEXIT: u32 = VM_RQ_BASE + 5;
pub const VM_MMAP: u32 = VM_RQ_BASE + 10;
pub const VM_ADDDMA: u32 = VM_RQ_BASE + 12;
pub const VM_DELDMA: u32 = VM_RQ_BASE + 13;
pub const VM_GETDMA: u32 = VM_RQ_BASE + 14;
pub const VM_MAP_PHYS: u32 = VM_RQ_BASE + 15;
pub const VM_UNMAP_PHYS: u32 = VM_RQ_BASE + 16;
pub const VM_MUNMAP: u32 = VM_RQ_BASE + 17;
pub const VM_MAPCACHEPAGE: u32 = VM_RQ_BASE + 26;
pub const VM_SETCACHEPAGE: u32 = VM_RQ_BASE + 27;
pub const VM_CLEARCACHE: u32 = VM_RQ_BASE + 28;
pub const VM_VFS_REPLY: u32 = VM_RQ_BASE + 30;
pub const VM_REMAP: u32 = VM_RQ_BASE + 33;
pub const VM_SHM_UNMAP: u32 = VM_RQ_BASE + 34;
pub const VM_GETPHYS: u32 = VM_RQ_BASE + 35;
pub const VM_GETREF: u32 = VM_RQ_BASE + 36;
pub const VM_RS_SET_PRIV: u32 = VM_RQ_BASE + 37;
pub const VM_QUERY_EXIT: u32 = VM_RQ_BASE + 38;
pub const VM_NOTIFY_SIG: u32 = VM_RQ_BASE + 39;
pub const VM_INFO: u32 = VM_RQ_BASE + 40;
pub const VM_RS_UPDATE: u32 = VM_RQ_BASE + 41;
pub const VM_RS_MEMCTL: u32 = VM_RQ_BASE + 42;
pub const VM_WATCH_EXIT: u32 = VM_RQ_BASE + 43;
pub const VM_REMAP_RO: u32 = VM_RQ_BASE + 44;
pub const VM_PROCCTL: u32 = VM_RQ_BASE + 45;
pub const VM_VFS_MMAP: u32 = VM_RQ_BASE + 46;
pub const VM_GETRUSAGE: u32 = VM_RQ_BASE + 47;
pub const NR_VM_CALLS: u32 = 48;
pub const VM_PAGEFAULT: u32 = VM_RQ_BASE + 0xff;

// VM_INFO subcodes
pub const VMIW_STATS: u32 = 1;
pub const VMIW_USAGE: u32 = 2;
pub const VMIW_REGION: u32 = 3;

// VM_RS_MEMCTL subcodes
pub const VM_RS_MEM_PIN: u32 = 0;
pub const VM_RS_MEM_MAKE_VM: u32 = 1;

// VM_PROCCTL subcodes
pub const VMPPARAM_CLEAR: u32 = 1;
pub const VMPPARAM_HANDLEMEM: u32 = 2;

// VM->VFS request codes
pub const VMVFSREQ_FDLOOKUP: u32 = 101;
pub const VMVFSREQ_FDCLOSE: u32 = 102;
pub const VMVFSREQ_FDIO: u32 = 103;

// Basic VM calls allowed to every process
pub const VM_BASIC_CALLS: [u32; 7] = [
    VM_BRK,
    VM_MMAP,
    VM_MUNMAP,
    VM_MAP_PHYS,
    VM_UNMAP_PHYS,
    VM_INFO,
    VM_GETRUSAGE,
];

// ═════════════════════════════════════════════════════════════════════════
// Chapter 19: IPC Server Messages
// ═════════════════════════════════════════════════════════════════════════

pub const IPC_BASE: u32 = 0xD00;

pub const IPC_SHMGET: u32 = IPC_BASE + 1;
pub const IPC_SHMAT: u32 = IPC_BASE + 2;
pub const IPC_SHMDT: u32 = IPC_BASE + 3;
pub const IPC_SHMCTL: u32 = IPC_BASE + 4;
pub const IPC_SEMGET: u32 = IPC_BASE + 5;
pub const IPC_SEMCTL: u32 = IPC_BASE + 6;
pub const IPC_SEMOP: u32 = IPC_BASE + 7;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 20: Scheduling Messages
// ═════════════════════════════════════════════════════════════════════════

pub const SCHEDULING_BASE: u32 = 0xF00;

pub const SCHEDULING_NO_QUANTUM: u32 = SCHEDULING_BASE + 1;
pub const SCHEDULING_START: u32 = SCHEDULING_BASE + 2;
pub const SCHEDULING_STOP: u32 = SCHEDULING_BASE + 3;
pub const SCHEDULING_SET_NICE: u32 = SCHEDULING_BASE + 4;
pub const SCHEDULING_INHERIT: u32 = SCHEDULING_BASE + 5;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 21: USB Messages
// ═════════════════════════════════════════════════════════════════════════

pub const USB_BASE: u32 = 0x1100;

pub const USB_RQ_INIT: u32 = USB_BASE;
pub const USB_RQ_DEINIT: u32 = USB_BASE + 1;
pub const USB_RQ_SEND_URB: u32 = USB_BASE + 2;
pub const USB_RQ_CANCEL_URB: u32 = USB_BASE + 3;
pub const USB_RQ_SEND_INFO: u32 = USB_BASE + 4;
pub const USB_REPLY: u32 = USB_BASE + 5;

pub const USB_COMPLETE_URB: u32 = USB_BASE + 6;
pub const USB_ANNOUCE_DEV: u32 = USB_BASE + 7;
pub const USB_WITHDRAW_DEV: u32 = USB_BASE + 8;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 22: Device Manager (DEVMAN) Messages
// ═════════════════════════════════════════════════════════════════════════

pub const DEVMAN_BASE: u32 = 0x1200;

pub const DEVMAN_ADD_DEV: u32 = DEVMAN_BASE;
pub const DEVMAN_DEL_DEV: u32 = DEVMAN_BASE + 1;
pub const DEVMAN_ADD_BUS: u32 = DEVMAN_BASE + 2;
pub const DEVMAN_DEL_BUS: u32 = DEVMAN_BASE + 3;
pub const DEVMAN_ADD_DEVFILE: u32 = DEVMAN_BASE + 4;
pub const DEVMAN_DEL_DEVFILE: u32 = DEVMAN_BASE + 5;
pub const DEVMAN_REQUEST: u32 = DEVMAN_BASE + 6;
pub const DEVMAN_REPLY: u32 = DEVMAN_BASE + 7;
pub const DEVMAN_BIND: u32 = DEVMAN_BASE + 8;
pub const DEVMAN_UNBIND: u32 = DEVMAN_BASE + 9;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 23: TTY Messages
// ═════════════════════════════════════════════════════════════════════════

pub const TTY_RQ_BASE: u32 = 0x1300;

pub const TTY_FKEY_CONTROL: u32 = TTY_RQ_BASE + 1;
pub const FKEY_MAP: u32 = 10;
pub const FKEY_UNMAP: u32 = 11;
pub const FKEY_EVENTS: u32 = 12;

pub const TTY_INPUT_UP: u32 = TTY_RQ_BASE + 2;
pub const TTY_INPUT_EVENT: u32 = TTY_RQ_BASE + 3;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 24: Input Messages
// ═════════════════════════════════════════════════════════════════════════

pub const INPUT_RQ_BASE: u32 = 0x1500;
pub const INPUT_RS_BASE: u32 = 0x1580;

pub const INPUT_CONF: u32 = INPUT_RQ_BASE;
pub const INPUT_SETLEDS: u32 = INPUT_RQ_BASE + 1;
pub const INPUT_EVENT: u32 = INPUT_RS_BASE;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 25: VFS-FS Transaction IDs
// ═════════════════════════════════════════════════════════════════════════

pub const VFS_TRANSACTION_BASE: u32 = 0xB00;

pub const VFS_TRANSID: u32 = VFS_TRANSACTION_BASE + 1;

pub fn is_vfs_fs_transid(typ: u32) -> bool {
    (typ & !0xff) == VFS_TRANSACTION_BASE
}

// ═════════════════════════════════════════════════════════════════════════
// Chapter 26: Character Device (CDEV) Messages
// ═════════════════════════════════════════════════════════════════════════

pub const CDEV_RQ_BASE: u32 = 0x400;
pub const CDEV_RS_BASE: u32 = 0x480;

pub fn is_cdev_rq(typ: u32) -> bool {
    (typ & !0x7f) == CDEV_RQ_BASE
}
pub fn is_cdev_rs(typ: u32) -> bool {
    (typ & !0x7f) == CDEV_RS_BASE
}

pub const CDEV_OPEN: u32 = CDEV_RQ_BASE;
pub const CDEV_CLOSE: u32 = CDEV_RQ_BASE + 1;
pub const CDEV_READ: u32 = CDEV_RQ_BASE + 2;
pub const CDEV_WRITE: u32 = CDEV_RQ_BASE + 3;
pub const CDEV_IOCTL: u32 = CDEV_RQ_BASE + 4;
pub const CDEV_CANCEL: u32 = CDEV_RQ_BASE + 5;
pub const CDEV_SELECT: u32 = CDEV_RQ_BASE + 6;

pub const CDEV_REPLY: u32 = CDEV_RS_BASE;
pub const CDEV_SEL1_REPLY: u32 = CDEV_RS_BASE + 1;
pub const CDEV_SEL2_REPLY: u32 = CDEV_RS_BASE + 2;

pub const CDEV_R_BIT: u32 = 0x01;
pub const CDEV_W_BIT: u32 = 0x02;
pub const CDEV_NOCTTY: u32 = 0x04;

pub const CDEV_NOFLAGS: u32 = 0x00;
pub const CDEV_NONBLOCK: u32 = 0x01;

pub const CDEV_OP_RD: u32 = 0x01;
pub const CDEV_OP_WR: u32 = 0x02;
pub const CDEV_OP_ERR: u32 = 0x04;
pub const CDEV_NOTIFY: u32 = 0x08;

pub const CDEV_CLONED: u32 = 0x20000000;
pub const CDEV_CTTY: u32 = 0x40000000;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 27: Block Device (BDEV) Messages
// ═════════════════════════════════════════════════════════════════════════

pub const BDEV_RQ_BASE: u32 = 0x500;
pub const BDEV_RS_BASE: u32 = 0x580;

pub fn is_bdev_rq(typ: u32) -> bool {
    (typ & !0x7f) == BDEV_RQ_BASE
}
pub fn is_bdev_rs(typ: u32) -> bool {
    (typ & !0x7f) == BDEV_RS_BASE
}

pub const BDEV_OPEN: u32 = BDEV_RQ_BASE;
pub const BDEV_CLOSE: u32 = BDEV_RQ_BASE + 1;
pub const BDEV_READ: u32 = BDEV_RQ_BASE + 2;
pub const BDEV_WRITE: u32 = BDEV_RQ_BASE + 3;
pub const BDEV_GATHER: u32 = BDEV_RQ_BASE + 4;
pub const BDEV_SCATTER: u32 = BDEV_RQ_BASE + 5;
pub const BDEV_IOCTL: u32 = BDEV_RQ_BASE + 6;

pub const BDEV_REPLY: u32 = BDEV_RS_BASE;

pub const BDEV_R_BIT: u32 = 0x01;
pub const BDEV_W_BIT: u32 = 0x02;

pub const BDEV_NOFLAGS: u32 = 0x00;
pub const BDEV_FORCEWRITE: u32 = 0x01;
pub const BDEV_NOPAGE: u32 = 0x02;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 28: Real Time Clock (RTCDEV) Messages
// ═════════════════════════════════════════════════════════════════════════

pub const RTCDEV_RQ_BASE: u32 = 0x1400;
pub const RTCDEV_RS_BASE: u32 = 0x1480;

pub fn is_rtcdev_rq(typ: u32) -> bool {
    (typ & !0x7f) == RTCDEV_RQ_BASE
}
pub fn is_rtcdev_rs(typ: u32) -> bool {
    (typ & !0x7f) == RTCDEV_RS_BASE
}

pub const RTCDEV_GET_TIME: u32 = RTCDEV_RQ_BASE;
pub const RTCDEV_SET_TIME: u32 = RTCDEV_RQ_BASE + 1;
pub const RTCDEV_PWR_OFF: u32 = RTCDEV_RQ_BASE + 2;
pub const RTCDEV_GET_TIME_G: u32 = RTCDEV_RQ_BASE + 3;
pub const RTCDEV_SET_TIME_G: u32 = RTCDEV_RQ_BASE + 4;

pub const RTCDEV_REPLY: u32 = RTCDEV_RS_BASE;

pub const RTCDEV_NOFLAGS: u32 = 0x00;
pub const RTCDEV_Y2KBUG: u32 = 0x01;
pub const RTCDEV_CMOSREG: u32 = 0x02;

// ═════════════════════════════════════════════════════════════════════════
// Chapter 29: SUSPEND — Internal Code
// ═════════════════════════════════════════════════════════════════════════

/// Status to suspend caller, reply later.
pub const SUSPEND: i32 = -998;

// ═════════════════════════════════════════════════════════════════════════
// Tests
// ═════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_endpoints() {
        assert_eq!(ASYNCM, -5);
        assert_eq!(IDLE, -4);
        assert_eq!(CLOCK, -3);
        assert_eq!(SYSTEM, -2);
        assert_eq!(KERNEL, -1);
        assert_eq!(HARDWARE, -1);
        assert_eq!(PM_PROC_NR, 0);
        assert_eq!(VFS_PROC_NR, 1);
        assert_eq!(VM_PROC_NR, 8);
        assert_eq!(INIT_PROC_NR, 10);
        assert_eq!(NR_BOOT_MODULES, 11);
    }

    #[test]
    fn test_notification() {
        assert_eq!(NOTIFY_MESSAGE, 0x1000);
    }

    #[test]
    fn test_bus_pci_constants() {
        assert_eq!(BUSC_RQ_BASE, 0x300);
        assert_eq!(BUSC_PCI_INIT, 0x300);
        assert_eq!(BUSC_PCI_FIRST_DEV, 0x301);
        assert_eq!(BUSC_PCI_GET_BAR, 0x313);
        assert_eq!(IOMMU_MAP, 0x320);
        assert_eq!(BUSC_I2C_RESERVE, 0x340);
    }

    #[test]
    fn test_dl_constants() {
        assert_eq!(DL_RQ_BASE, 0x200);
        assert_eq!(DL_CONF, 0x200);
        assert_eq!(DL_READV_S, 0x203);
        assert_eq!(DL_CONF_REPLY, 0x280);
    }

    #[test]
    fn test_syscall_numbers() {
        assert_eq!(KERNEL_CALL, 0x600);
        assert_eq!(sys::FORK, 0x600);
        assert_eq!(sys::EXEC, 0x601);
        assert_eq!(sys::KILL, 0x606);
        assert_eq!(sys::GETINFO, 0x61A);
        assert_eq!(sys::EXIT, 0x635);
        assert_eq!(sys::SAFEMEMSET, 0x638);
        assert_eq!(NR_SYS_CALLS, 58);
    }

    #[test]
    fn test_dio_constants() {
        assert_eq!(DIO_INPUT, 0x001);
        assert_eq!(DIO_OUTPUT, 0x002);
        assert_eq!(DIO_BYTE, 0x010);
        assert_eq!(DIO_WORD, 0x020);
        assert_eq!(DIO_LONG, 0x030);
        assert_eq!(DIO_SAFE, 0x100);
        assert_eq!(DIO_INPUT_BYTE, 0x011);
        assert_eq!(DIO_SAFE_OUTPUT_LONG, 0x132);
    }

    #[test]
    fn test_getinfo_subcodes() {
        assert_eq!(GET_KINFO, 0);
        assert_eq!(GET_IMAGE, 1);
        assert_eq!(GET_MACHINE, 12);
        assert_eq!(GET_RUSAGE, 25);
    }

    #[test]
    fn test_privctl() {
        assert_eq!(SYS_PRIV_ALLOW, 1);
        assert_eq!(SYS_PRIV_SET_SYS, 3);
        assert_eq!(SYS_PRIV_YIELD, 10);
    }

    #[test]
    fn test_vmctl() {
        assert_eq!(VMCTL_CLEAR_PAGEFAULT, 12);
        assert_eq!(VMCTL_GET_PDBR, 13);
        assert_eq!(VMCTL_BOOTINHIBIT_CLEAR, 33);
        assert_eq!(VMMF_UNCACHED, 1);
        assert_eq!(VMMF_GLO, 8);
    }

    #[test]
    fn test_rs_messages() {
        assert_eq!(RS_RQ_BASE, 0x700);
        assert_eq!(RS_UP, 0x700);
        assert_eq!(RS_INIT, 0x714);
        assert_eq!(RS_LU_PREPARE, 0x715);
    }

    #[test]
    fn test_ds_messages() {
        assert_eq!(DS_RQ_BASE, 0x800);
        assert_eq!(DS_PUBLISH, 0x800);
        assert_eq!(DS_GETSYSINFO, 0x807);
    }

    #[test]
    fn test_vfs_pm_messages() {
        assert_eq!(VFS_PM_RQ_BASE, 0x900);
        assert_eq!(VFS_PM_EXEC, 0x906);
        assert_eq!(VFS_PM_EXEC_REPLY, 0x986);
    }

    #[test]
    fn test_vm_messages() {
        assert_eq!(VM_RQ_BASE, 0xC00);
        assert_eq!(VM_EXIT, 0xC00);
        assert_eq!(VM_FORK, 0xC01);
        assert_eq!(VM_MMAP, 0xC0A);
        assert_eq!(VM_PAGEFAULT, 0xCFF);
        assert_eq!(NR_VM_CALLS, 48);

        assert_eq!(VMIW_STATS, 1);
        assert_eq!(VMIW_REGION, 3);
        assert_eq!(VMVFSREQ_FDLOOKUP, 101);
    }

    #[test]
    fn test_ipc_messages() {
        assert_eq!(IPC_BASE, 0xD00);
        assert_eq!(IPC_SHMGET, 0xD01);
        assert_eq!(IPC_SEMOP, 0xD07);
    }

    #[test]
    fn test_scheduling_messages() {
        assert_eq!(SCHEDULING_BASE, 0xF00);
        assert_eq!(SCHEDULING_NO_QUANTUM, 0xF01);
        assert_eq!(SCHEDULING_INHERIT, 0xF05);
    }

    #[test]
    fn test_usb_messages() {
        assert_eq!(USB_BASE, 0x1100);
        assert_eq!(USB_RQ_INIT, 0x1100);
        assert_eq!(USB_WITHDRAW_DEV, 0x1108);
    }

    #[test]
    fn test_devman_messages() {
        assert_eq!(DEVMAN_BASE, 0x1200);
        assert_eq!(DEVMAN_UNBIND, 0x1209);
    }

    #[test]
    fn test_tty_messages() {
        assert_eq!(TTY_RQ_BASE, 0x1300);
        assert_eq!(TTY_FKEY_CONTROL, 0x1301);
        assert_eq!(TTY_INPUT_EVENT, 0x1303);
    }

    #[test]
    fn test_input_messages() {
        assert_eq!(INPUT_RQ_BASE, 0x1500);
        assert_eq!(INPUT_CONF, 0x1500);
        assert_eq!(INPUT_EVENT, 0x1580);
    }

    #[test]
    fn test_cdev_messages() {
        assert_eq!(CDEV_RQ_BASE, 0x400);
        assert_eq!(CDEV_OPEN, 0x400);
        assert_eq!(CDEV_SELECT, 0x406);
        assert_eq!(CDEV_SEL2_REPLY, 0x482);
    }

    #[test]
    fn test_bdev_messages() {
        assert_eq!(BDEV_RQ_BASE, 0x500);
        assert_eq!(BDEV_OPEN, 0x500);
        assert_eq!(BDEV_IOCTL, 0x506);
    }

    #[test]
    fn test_rtcdev_messages() {
        assert_eq!(RTCDEV_RQ_BASE, 0x1400);
        assert_eq!(RTCDEV_GET_TIME, 0x1400);
        assert_eq!(RTCDEV_SET_TIME_G, 0x1404);
    }

    #[test]
    fn test_suspend() {
        assert_eq!(SUSPEND, -998);
    }
}
