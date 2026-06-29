//! Kernel global variables — adapted from `minix/kernel/glo.h`
//!
//! Central storage for kernel-wide state: kernel info, machine info,
//! diagnostic messages, load statistics, randomness, VM state, IPC
//! call names, interrupt state, timing, and BKL statistics.
//!
//! **Rust 2024 `static_mut_refs` handling:**
//! Simple scalars use `AtomicU32`/`AtomicBool` for safe concurrent
//! access. Struct statics are wrapped in `UnsafeCell`-based types with
//! `unsafe impl Sync`. Compound statics use atomics or wrapper types.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64};

// ─────────────────────────────────────────────────────────────────────────
// Constants & config
// ─────────────────────────────────────────────────────────────────────────

/// System clock frequency (HZ).
pub static SYSTEM_HZ: AtomicU32 = AtomicU32::new(60);

/// APIC configuration.
pub static CONFIG_NO_APIC: AtomicBool = AtomicBool::new(false);
pub static CONFIG_APIC_TIMER_X: AtomicU32 = AtomicU32::new(0);

/// SMP configuration.
pub static CONFIG_NO_SMP: AtomicBool = AtomicBool::new(true);

// ─────────────────────────────────────────────────────────────────────────
// Wrapper types for struct statics
// ─────────────────────────────────────────────────────────────────────────

pub struct KInfoCell(UnsafeCell<KInfo>);
unsafe impl Sync for KInfoCell {}
impl KInfoCell {
    pub const fn new(val: KInfo) -> Self {
        Self(UnsafeCell::new(val))
    }
    /// Get a raw pointer. Valid for the lifetime of the program (static).
    pub fn get(&self) -> *mut KInfo {
        self.0.get()
    }
}

pub struct MachineCell(UnsafeCell<Machine>);
unsafe impl Sync for MachineCell {}
impl MachineCell {
    pub const fn new(val: Machine) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut Machine {
        self.0.get()
    }
}

pub struct KMessagesCell(UnsafeCell<KMessages>);
unsafe impl Sync for KMessagesCell {}
impl KMessagesCell {
    pub const fn new(val: KMessages) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut KMessages {
        self.0.get()
    }
}

pub struct LoadInfoCell(UnsafeCell<LoadInfo>);
unsafe impl Sync for LoadInfoCell {}
impl LoadInfoCell {
    pub const fn new(val: LoadInfo) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut LoadInfo {
        self.0.get()
    }
}

pub struct KRandomnessCell(UnsafeCell<KRandomness>);
unsafe impl Sync for KRandomnessCell {}
impl KRandomnessCell {
    pub const fn new(val: KRandomness) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut KRandomness {
        self.0.get()
    }
}

pub struct MinixKernInfoCell(UnsafeCell<MinixKernInfo>);
unsafe impl Sync for MinixKernInfoCell {}
impl MinixKernInfoCell {
    pub const fn new(val: MinixKernInfo) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut MinixKernInfo {
        self.0.get()
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Kernel info structures
// ─────────────────────────────────────────────────────────────────────────

/// Kernel information for userspace.
pub static KINFO: KInfoCell = KInfoCell::new(KInfo::new());

/// Machine information for userspace.
pub static MACHINE: MachineCell = MachineCell::new(Machine::new());

/// Diagnostic messages buffer.
pub static KMESSAGES: KMessagesCell = KMessagesCell::new(KMessages::new());

/// Load average status.
pub static LOADINFO: LoadInfoCell = LoadInfoCell::new(LoadInfo::new());

/// Randomness source.
pub static KRANDOM: KRandomnessCell = KRandomnessCell::new(KRandomness::new());

/// Minix kernel info struct (ABI).
pub static MINIX_KERNINFO: MinixKernInfoCell = MinixKernInfoCell::new(MinixKernInfo::new());

// ─────────────────────────────────────────────────────────────────────────
// Simple globals (atomic for Rust 2024 safety)
// ─────────────────────────────────────────────────────────────────────────

/// Pointer to user-facing kernel info (address).
pub static MINIX_KERNINFO_USER: AtomicU64 = AtomicU64::new(0);

/// Boot time.
pub static BOOTTIME: AtomicU64 = AtomicU64::new(0);

/// Verbose boot flag.
pub static VERBOSEBOOT: AtomicU32 = AtomicU32::new(1);

/// Kernel ticks lost outside clock task.
pub static LOST_TICKS: AtomicU32 = AtomicU32::new(0);

/// Whether VM is running.
pub static VM_RUNNING: AtomicBool = AtomicBool::new(false);

/// Whether to catch page faults.
pub static CATCH_PAGEFAULTS: AtomicBool = AtomicBool::new(true);

/// Whether the kernel may allocate.
pub static KERNEL_MAY_ALLOC: AtomicBool = AtomicBool::new(false);

/// Feature flags.
pub static MINIX_FEATURE_FLAGS: AtomicU32 = AtomicU32::new(0);

/// Serial debug active.
pub static SERIAL_DEBUG_ACTIVE: AtomicBool = AtomicBool::new(false);

// ─────────────────────────────────────────────────────────────────────────
// CPU frequency
// ─────────────────────────────────────────────────────────────────────────

const fn new_atomic_u64_array() -> [AtomicU64; 32] {
    [
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
        AtomicU64::new(0),
    ]
}

const fn new_atomic_u32_array() -> [AtomicU32; 32] {
    [
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
        AtomicU32::new(0),
    ]
}

/// Per-CPU frequency in Hz.
pub static CPU_HZ: [AtomicU64; 32] = new_atomic_u64_array();

/// Set the CPU frequency for a given CPU.
pub fn cpu_set_freq(cpu: usize, freq: u64) {
    if cpu < 32 {
        CPU_HZ[cpu].store(freq, core::sync::atomic::Ordering::Relaxed);
    }
}

/// Get the CPU frequency for a given CPU.
pub fn cpu_get_freq(cpu: usize) -> u64 {
    if cpu < 32 {
        CPU_HZ[cpu].load(core::sync::atomic::Ordering::Relaxed)
    } else {
        0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// BKL statistics
// ─────────────────────────────────────────────────────────────────────────

/// BKL statistics per-CPU.
pub static KERNEL_TICKS: [AtomicU64; 32] = new_atomic_u64_array();
pub static BKL_TICKS: [AtomicU64; 32] = new_atomic_u64_array();
pub static BKL_TRIES: [AtomicU32; 32] = new_atomic_u32_array();
pub static BKL_SUCC: [AtomicU32; 32] = new_atomic_u32_array();

// ─────────────────────────────────────────────────────────────────────────
// IPC call names
// ─────────────────────────────────────────────────────────────────────────

pub struct IpcCallNamesCell(UnsafeCell<[Option<&'static str>; 256]>);
unsafe impl Sync for IpcCallNamesCell {}
impl IpcCallNamesCell {
    pub const fn new(val: [Option<&'static str>; 256]) -> Self {
        Self(UnsafeCell::new(val))
    }
    pub fn get(&self) -> *mut [Option<&'static str>; 256] {
        self.0.get()
    }
}

/// Human-readable IPC call names.
pub static IPC_CALL_NAMES: IpcCallNamesCell = IpcCallNamesCell::new([None; 256]);

/// Initialize IPC call names.
pub fn init_ipc_call_names() {
    let names = IPC_CALL_NAMES.get();
    unsafe {
        (*names)[0] = Some("SYS_FORK");
        (*names)[1] = Some("SYS_EXEC");
        (*names)[2] = Some("SYS_CLEAR");
        (*names)[3] = Some("SYS_SCHEDULE");
        (*names)[4] = Some("SYS_PRIVCTL");
        // More can be added as-needed
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Struct definitions
// ─────────────────────────────────────────────────────────────────────────

/// Kernel information structure.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KInfo {
    pub boottime: u64,
    pub loadinfo: u64,
    pub reserved: [u64; 14],
}

impl Default for KInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl KInfo {
    pub const fn new() -> Self {
        Self {
            boottime: 0,
            loadinfo: 0,
            reserved: [0u64; 14],
        }
    }
}

/// Machine information.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Machine {
    pub processors_count: u32,
    pub bsp_id: u32,
    pub padding: i32,
    pub apic_enabled: i32,
    pub acpi_rsdp: u64,
    pub board_id: u32,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub const fn new() -> Self {
        Self {
            processors_count: 1,
            bsp_id: 0,
            padding: 0,
            apic_enabled: 0,
            acpi_rsdp: 0,
            board_id: 0,
        }
    }
}

/// Diagnostic messages circular buffer.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KMessages {
    pub km_next: i32,
    pub km_size: i32,
    pub km_buf: [u8; 10000],
    pub kmess_buf: [u8; 80 * 25],
    pub blpos: i32,
}

impl Default for KMessages {
    fn default() -> Self {
        Self::new()
    }
}

impl KMessages {
    pub const fn new() -> Self {
        Self {
            km_next: 0,
            km_size: 0,
            km_buf: [0u8; 10000],
            kmess_buf: [0u8; 80 * 25],
            blpos: 0,
        }
    }
}

/// Load average information.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct LoadInfo {
    pub proc_load_history: [u16; 150],
    pub proc_last_slot: u16,
    pub last_clock: u64,
}

impl Default for LoadInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl LoadInfo {
    pub const fn new() -> Self {
        Self {
            proc_load_history: [0u16; 150],
            proc_last_slot: 0,
            last_clock: 0,
        }
    }
}

/// Randomness source.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KRandomness {
    pub random_elements: i32,
    pub random_sources: i32,
    pub bin: [KRandomnessBin; 16],
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct KRandomnessBin {
    pub r_next: i32,
    pub r_size: i32,
    pub r_buf: [u16; 64],
}

impl Default for KRandomness {
    fn default() -> Self {
        Self::new()
    }
}

impl KRandomness {
    pub const fn new() -> Self {
        Self {
            random_elements: 0,
            random_sources: 0,
            bin: [KRandomnessBin::new(); 16],
        }
    }
}

impl Default for KRandomnessBin {
    fn default() -> Self {
        Self::new()
    }
}

impl KRandomnessBin {
    pub const fn new() -> Self {
        Self {
            r_next: 0,
            r_size: 0,
            r_buf: [0u16; 64],
        }
    }
}

/// Minix kernel info (ABI structure for userspace).
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct MinixKernInfo {
    pub magic: u32,
    pub feature_flags: u32,
    pub ki_flags: u32,
    pub frclock_tcrr: u32,
    pub _unused: [u32; 2],
    pub kinfo: u64,
    pub machine: u64,
    pub kmessages: u64,
    pub loadinfo: u64,
    pub ipcvecs: u64,
    pub arm_frclock_hz: u64,
}

impl Default for MinixKernInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl MinixKernInfo {
    pub const fn new() -> Self {
        Self {
            magic: 0xfc3b84bf,
            feature_flags: 0,
            ki_flags: 0,
            frclock_tcrr: 0,
            _unused: [0u32; 2],
            kinfo: 0,
            machine: 0,
            kmessages: 0,
            loadinfo: 0,
            ipcvecs: 0,
            arm_frclock_hz: 0,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// VM request queue head
// ─────────────────────────────────────────────────────────────────────────

/// First process on VM request queue.
pub static VMREQUEST: AtomicPtr<crate::proc::Proc> = AtomicPtr::new(core::ptr::null_mut());

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::Ordering;

    #[test]
    fn test_default_values() {
        assert_eq!(SYSTEM_HZ.load(Ordering::Relaxed), 60);
        assert!(!CONFIG_NO_APIC.load(Ordering::Relaxed));
        assert_eq!(CONFIG_APIC_TIMER_X.load(Ordering::Relaxed), 0);
        assert!(CONFIG_NO_SMP.load(Ordering::Relaxed));
    }

    #[test]
    fn test_boottime() {
        BOOTTIME.store(1000, Ordering::Relaxed);
        assert_eq!(BOOTTIME.load(Ordering::Relaxed), 1000);
        BOOTTIME.store(0, Ordering::Relaxed);
    }

    #[test]
    fn test_cpu_hz_get_set() {
        cpu_set_freq(0, 2_500_000_000);
        assert_eq!(cpu_get_freq(0), 2_500_000_000);
        cpu_set_freq(0, 0);
    }

    #[test]
    fn test_cpu_hz_out_of_bounds() {
        cpu_set_freq(32, 1_000_000);
        assert_eq!(cpu_get_freq(32), 0);
    }

    #[test]
    fn test_vm_flags() {
        assert!(!VM_RUNNING.load(Ordering::Relaxed));
        assert!(CATCH_PAGEFAULTS.load(Ordering::Relaxed));
        assert!(!KERNEL_MAY_ALLOC.load(Ordering::Relaxed));
    }

    #[test]
    fn test_verbose_boot() {
        assert_eq!(VERBOSEBOOT.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_kinfo_layout() {
        let ki = KInfo::new();
        assert_eq!(ki.boottime, 0);
    }

    #[test]
    fn test_machine_layout() {
        let m = Machine::new();
        assert_eq!(m.processors_count, 1);
    }

    #[test]
    fn test_kmessages_layout() {
        let km = KMessages::new();
        assert_eq!(km.km_next, 0);
        assert_eq!(km.km_buf.len(), 10000);
    }

    #[test]
    fn test_loadinfo_layout() {
        let li = LoadInfo::new();
        assert_eq!(li.proc_last_slot, 0);
    }

    #[test]
    fn test_krandomness_layout() {
        let kr = KRandomness::new();
        assert_eq!(kr.bin.len(), 16);
    }

    #[test]
    fn test_minix_kerninfo_magic() {
        let ki = MinixKernInfo::new();
        let magic = unsafe { core::ptr::addr_of!(ki.magic).read_unaligned() };
        assert_eq!(magic, 0xfc3b84bf);
    }

    #[test]
    fn test_bkl_stats_default() {
        assert_eq!(KERNEL_TICKS[0].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_ipc_call_names() {
        init_ipc_call_names();
        unsafe {
            assert_eq!((*IPC_CALL_NAMES.get())[0], Some("SYS_FORK"));
            assert_eq!((*IPC_CALL_NAMES.get())[3], Some("SYS_SCHEDULE"));
        }
    }

    #[test]
    fn test_vmrequest_null() {
        assert!(VMREQUEST.load(Ordering::Relaxed).is_null());
    }
}
