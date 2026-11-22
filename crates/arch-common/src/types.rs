//! Kernel type definitions matching `minix/type.h`
//!
//! ABI-critical: every struct must have verified size and field offsets.
//! Uses fixed-size integers (not `c_*` types) because these definitions
//! must match the x86_64 Minix kernel ABI regardless of host platform.

use core::fmt;
use core::mem::size_of;


/// Virtual address/length in bytes (unsigned long = 8 bytes on x86_64).
pub type VirBytes = u64;

/// Physical address/length in bytes.
pub type PhysBytes = u64;

/// Physical address/length in clicks (unsigned int = 4 bytes).
pub type PhysClicks = u32;

/// Virtual address/length in clicks.
pub type VirClicks = u32;

/// Process identifier (int = 4 bytes).
pub type Endpoint = i32;

/// Grant ID (int32_t = 4 bytes).
pub type CpGrantId = i32;


#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirAddr {
    pub proc_nr_e: Endpoint,
    pub offset: VirBytes,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirCpReq {
    pub src: VirAddr,
    pub dst: VirAddr,
    pub count: PhysBytes,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VumapVir {
    pub vv_u: VumapVirUnion,
    pub vv_size: usize,
}

impl fmt::Debug for VumapVir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VumapVir")
            .field("vv_size", &self.vv_size)
            .finish()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union VumapVirUnion {
    pub u_grant: CpGrantId,
    pub u_addr: VirBytes,
}

impl fmt::Debug for VumapVirUnion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VumapVirUnion")
            .field("u_grant", unsafe { &self.u_grant })
            .finish()
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VumapPhys {
    pub vp_addr: PhysBytes,
    pub vp_size: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Iovec {
    pub iov_addr: VirBytes,
    pub iov_size: VirBytes,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IovecS {
    pub iov_grant: CpGrantId,
    pub iov_size: VirBytes,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SigMsg {
    pub sm_signo: i32,
    pub _pad: i32,
    pub sm_mask: SigsetT,
    pub sm_sighandler: VirBytes,
    pub sm_sigreturn: VirBytes,
    pub sm_stkptr: VirBytes,
}

pub type SigsetT = [u64; 2];

pub const LOAD_UNIT_SECS: u32 = 6;
pub const LOAD_HISTORY_MINUTES: u32 = 15;
pub const LOAD_HISTORY_SECONDS: u32 = 60 * LOAD_HISTORY_MINUTES;
pub const LOAD_HISTORY: usize = (LOAD_HISTORY_SECONDS / LOAD_UNIT_SECS) as usize;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LoadInfo {
    pub proc_load_history: [u16; LOAD_HISTORY],
    pub proc_last_slot: u16,
    pub _pad: u16,
    pub last_clock: i64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Machine {
    pub processors_count: u32,
    pub bsp_id: u32,
    pub padding: i32,
    pub apic_enabled: i32,
    pub acpi_rsdp: PhysBytes,
    pub board_id: u32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct IoRange {
    pub ior_base: u32,
    pub ior_limit: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MinixMemRange {
    pub mr_base: PhysBytes,
    pub mr_limit: PhysBytes,
}

pub const PROC_NAME_LEN: usize = 16;

/// Longest exec path a process records, `/proc/<pid>/exe`'s target. Sized for a
/// toolchain path (`/usr/libexec/...`) with room to spare; PM leaves the field
/// empty rather than truncating a longer one.
pub const PROC_EXEC_LEN: usize = 256;

/// PM's `mp_flags` bits a reader of `/proc` needs, shared so the record's meaning is defined once.
///
/// PM's own constants (`IN_USE`, `ZOMBIE`, `PROC_STOPPED`) alias these, and `procfs` turns them
/// into the state letter it prints, so a bit moved on one side cannot silently change the other.
pub const PM_PROC_IN_USE: u32 = 0x00001;
pub const PM_PROC_ZOMBIE: u32 = 0x00004;
pub const PM_PROC_STOPPED: u32 = 0x00008;

/// One process as PM hands it to a reader of `/proc` (the `SI_PROC_TAB` answer).
///
/// C's `procfs` asks PM for the raw `mproc` array and parses it against
/// `minix/servers/pm/mproc.h`. This port hands out a compact record instead: `MProc` lives in the
/// `servers` crate, which a filesystem server cannot depend on, and a mirrored copy of a
/// several-hundred-byte struct would drift from the original without anything failing to compile.
/// The fields below are exactly the ones `procfs` prints, so adding or renaming one breaks both
/// sides at build time.
///
/// `flags` is PM's `mp_flags` (the `IN_USE`/`ZOMBIE`/`STOPPED`/`UNPAUSED` bits), which is what
/// `/proc/<pid>/status` turns into its state letter.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcTabEntry {
    pub pid: i32,
    pub ppid: i32,
    pub endpoint: i32,
    pub euid: i32,
    pub flags: u32,
    pub name: [u8; PROC_NAME_LEN],
    /// The path passed to `execve`, as PM read it from the caller. Empty when PM
    /// has none (a boot process, or a failed/partial exec), and left empty rather
    /// than truncated when it does not fit: a cut-off path names the wrong file.
    pub exec: [u8; PROC_EXEC_LEN],
}

impl ProcTabEntry {
    pub const fn empty() -> Self {
        Self {
            pid: 0,
            ppid: 0,
            endpoint: 0,
            euid: 0,
            flags: 0,
            name: [0; PROC_NAME_LEN],
            exec: [0; PROC_EXEC_LEN],
        }
    }

    /// The name as a string slice, stopping at the first NUL.
    pub fn name_str(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(PROC_NAME_LEN);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }

    /// The exec path as a string slice, or `""` when PM has none.
    ///
    /// This is what `/proc/<pid>/exe` links to. It is the path the caller gave
    /// `execve`, so it is absolute for every caller that resolved a `PATH` entry
    /// first, which is what the C library's `execvp` and this port's shell do.
    pub fn exec_str(&self) -> &str {
        let end = self.exec.iter().position(|&b| b == 0).unwrap_or(PROC_EXEC_LEN);
        core::str::from_utf8(&self.exec[..end]).unwrap_or("")
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BootImage {
    pub proc_nr: i32,
    pub proc_name: [u8; PROC_NAME_LEN],
    pub endpoint: Endpoint,
    pub start_addr: PhysBytes,
    pub len: PhysBytes,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Memory {
    pub base: PhysBytes,
    pub size: PhysBytes,
}


pub const KMESS_BUF_SIZE: usize = 10000;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KMessages {
    pub km_next: i32,
    pub km_size: i32,
    pub km_buf: [u8; KMESS_BUF_SIZE],
    pub kmess_buf: [u8; 80 * 25],
    pub blpos: i32,
    pub _pad: i32,
}


pub const RANDOM_SOURCES: usize = 16;
pub const RANDOM_ELEMENTS: usize = 64;
pub type RandT = u16;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KRandomnessBin {
    pub r_next: i32,
    pub r_size: i32,
    pub r_buf: [RandT; RANDOM_ELEMENTS],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KRandomness {
    pub random_elements: i32,
    pub random_sources: i32,
    pub bin: [KRandomnessBin; RANDOM_SOURCES],
}


pub const KERNINFO_MAGIC: u32 = 0xfc3b84bf;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct MinixKernInfo {
    pub kerninfo_magic: u32,
    pub minix_feature_flags: u32,
    pub ki_flags: u32,
    pub minix_frclock_tcrr: u32,
    pub flags_unused3: u32,
    pub flags_unused4: u32,
    pub kinfo: *mut u8,
    pub machine: *mut u8,
    pub kmessages: *mut u8,
    pub loadinfo: *mut u8,
    pub minix_ipcvecs: *mut u8,
    pub minix_arm_frclock_hz: u64,
}

pub const MINIX_KIF_IPCVECS: u32 = 1 << 0;


const _: () = assert!(size_of::<VirAddr>() == 16);
const _: () = assert!(size_of::<VirCpReq>() == 40);
const _: () = assert!(size_of::<Machine>() == 32);
const _: () = assert!(size_of::<IoRange>() == 8);
const _: () = assert!(size_of::<MinixMemRange>() == 16);
const _: () = assert!(size_of::<BootImage>() == 40);
const _: () = assert!(size_of::<Memory>() == 16);

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::offset_of;

    #[test]
    fn test_primitive_sizes() {
        assert_eq!(size_of::<VirBytes>(), 8);
        assert_eq!(size_of::<PhysBytes>(), 8);
        assert_eq!(size_of::<PhysClicks>(), 4);
        assert_eq!(size_of::<VirClicks>(), 4);
        assert_eq!(size_of::<Endpoint>(), 4);
        assert_eq!(size_of::<CpGrantId>(), 4);
    }

    #[test]
    fn test_struct_sizes() {
        assert_eq!(size_of::<VirAddr>(), 16);
        assert_eq!(size_of::<VirCpReq>(), 40);
        assert_eq!(size_of::<Machine>(), 32);
        assert_eq!(size_of::<IoRange>(), 8);
        assert_eq!(size_of::<MinixMemRange>(), 16);
        assert_eq!(size_of::<Memory>(), 16);
    }

    #[test]
    fn test_offsets() {
        assert_eq!(offset_of!(VirAddr, proc_nr_e), 0);
        assert_eq!(offset_of!(VirAddr, offset), 8);
        assert_eq!(offset_of!(BootImage, proc_nr), 0);
        assert_eq!(offset_of!(BootImage, proc_name), 4);
        assert_eq!(offset_of!(BootImage, endpoint), 20);
        assert_eq!(offset_of!(BootImage, start_addr), 24);
    }

    #[test]
    fn test_constants() {
        assert_eq!(KERNINFO_MAGIC, 0xfc3b84bf);
        assert_eq!(PROC_NAME_LEN, 16);
        assert_eq!(LOAD_HISTORY, 150);
    }

    #[test]
    fn proc_tab_entry_has_the_expected_layout() {
        // Five i32 fields, then the two byte arrays: 36 bytes of header and name, then the
        // 256-byte exec path, with nothing padded between them.
        assert_eq!(size_of::<ProcTabEntry>(), 292);
        assert_eq!(offset_of!(ProcTabEntry, name), 20);
        assert_eq!(offset_of!(ProcTabEntry, exec), 36);
        let mut e = ProcTabEntry {
            pid: 7,
            ppid: 1,
            endpoint: 0x10007,
            euid: 0,
            flags: 1,
            name: *b"init\0\0\0\0\0\0\0\0\0\0\0\0",
            exec: [0; PROC_EXEC_LEN],
        };
        assert_eq!(e.name_str(), "init");
        e.exec[..14].copy_from_slice(b"/bin/coreutils");
        assert_eq!(e.exec_str(), "/bin/coreutils");
        assert_eq!(ProcTabEntry::empty().name_str(), "");
        assert_eq!(ProcTabEntry::empty().exec_str(), "");
    }

    #[test]
    fn proc_tab_entry_exec_without_a_nul_is_the_whole_field() {
        let e = ProcTabEntry {
            exec: [b'x'; PROC_EXEC_LEN],
            ..ProcTabEntry::empty()
        };
        assert_eq!(e.exec_str().len(), PROC_EXEC_LEN);
    }

    #[test]
    fn proc_tab_entry_name_without_a_nul_is_the_whole_field() {
        let e = ProcTabEntry {
            name: [b'x'; PROC_NAME_LEN],
            ..ProcTabEntry::empty()
        };
        assert_eq!(e.name_str().len(), PROC_NAME_LEN);
    }
}
