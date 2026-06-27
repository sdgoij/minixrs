//! Kernel type definitions matching `minix/type.h`
//!
//! ABI-critical: every struct must have verified size and field offsets.
//! Uses fixed-size integers (not `c_*` types) because these definitions
//! must match the x86_64 Minix kernel ABI regardless of host platform.

use core::fmt;
use core::mem::size_of;

// ── Primitive type aliases ──────────────────────────────────────────────

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

// ── Core structures ─────────────────────────────────────────────────────

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

// ── Kernel messages ─────────────────────────────────────────────────────

pub const KMESS_BUF_SIZE: usize = 256;

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

// ── Randomness ──────────────────────────────────────────────────────────

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

// ── Minix kerninfo ──────────────────────────────────────────────────────

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

// ── Compile-time checks ─────────────────────────────────────────────────

const _: () = assert!(size_of::<VirAddr>() == 16);
const _: () = assert!(size_of::<VirCpReq>() == 40);
const _: () = assert!(size_of::<Machine>() == 32);
const _: () = assert!(size_of::<IoRange>() == 8);
const _: () = assert!(size_of::<MinixMemRange>() == 16);
const _: () = assert!(size_of::<BootImage>() == 40);
const _: () = assert!(size_of::<Memory>() == 16);
const _: () = assert!(size_of::<KMessages>() == 2272);

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
}
