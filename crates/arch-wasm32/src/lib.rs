//! WebAssembly (wasm32) HAL.
//!
//! The host boundary is the point of this port. Everything the kernel treats as
//! a device — console, clock, and the halt/exit path — is a host import, so the
//! kernel instance holds policy while the host holds privilege.
//!
//! The parts of the HAL with no host boundary at all are re-exported from
//! `arch-sim` rather than duplicated a fourth time: paging stubs, the frame
//! layout, the VA layout, and the page arena. `ARCH_WASM32.md` §2 frames the two
//! as the same HAL, and §3 explains why the boundary is where the authority sits.
//!
//! # Imports
//!
//! Declared without a `wasm_import_module` attribute, so they land in `env` —
//! wasm-ld's default for undefined symbols — and so the crate still compiles when
//! it is built for the host as a workspace member. The harness supplies them.

#![no_std]

pub mod hal;

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

unsafe extern "C" {
    /// Append one byte to the console.
    fn host_console_write(byte: u32);
    /// Pop one console byte, or `-1` if none is pending.
    fn host_console_read() -> i32;
    /// Pending console byte count, without consuming any.
    fn host_console_available() -> i32;
    /// Monotonic cycle count.
    fn host_cycles() -> u64;
    /// Report a terminal condition and stop the instance.
    fn host_halt(code: u32);
    /// Copy `bytes` between two address spaces. `proc` is a process number, or
    /// negative for the kernel's own address space. Returns 0, or a negative
    /// errno.
    ///
    /// This is the port's page table. Two processes' address spaces are two
    /// linear memories here, and the host owns both, so a cross-process copy is
    /// not something the kernel can perform any more than a CR3 switch is
    /// something the host could (§5.1 of `ARCH_WASM32.md`).
    fn host_copy_between(
        src_proc: i32,
        src_addr: u32,
        dst_proc: i32,
        dst_addr: u32,
        bytes: u32,
    ) -> i32;
    /// Instantiate the module at `request_addr` as the process in `slot`.
    ///
    /// This is exec on this port (§7.2 of `ARCH_WASM32.md`): a program *is* a wasm module,
    /// so "install the new image" is instantiation, and only the host can instantiate. The
    /// kernel's half is to say which process, which bytes and with what arguments — one
    /// structure rather than a row of arguments, because this boundary has already paid for a
    /// dropped parameter (`PORTING_PLAN.md` finding 29: a console write that transferred
    /// `count = 0` bytes and reported success), and a layout with named fields is one thing both
    /// sides can read. See [`ExecModuleRequest`].
    ///
    /// The image is **not** copied through here. It is the caller's file, already in the
    /// caller's memory, and the request says whose and where; only the host can read one
    /// instance's memory on another's behalf (§5.1). Validating a wasm module means compiling
    /// it, so that is the engine's work — which is why this returns `ENOEXEC` rather than
    /// saying anything about what was wrong with the bytes.
    fn host_exec_module(slot: i32, request_addr: u32) -> i32;
    /// Clone the process in `parent_slot` into `child_slot`, as `fork` does.
    ///
    /// The one host entry with no addresses in it, because there is nothing to name: the
    /// parent's whole address space is what is being copied, and only the host can read one
    /// instance's memory and write another's (§5.1). On this port an address space *is* an
    /// instance's linear memory, so a fork is a byte copy where the other arches walk page
    /// tables and lay down COW leaves.
    ///
    /// The kernel calls this from `do_fork_handler`, where the child's `Proc` has just been
    /// made and the parent is still suspended inside the `SENDREC` that asked PM to fork —
    /// which is the state the clone has to happen in, because what makes the child's resume
    /// work is that the parent's stack is serialised in the memory being copied
    /// (`tools/fork-spike/`, §12 risk 1).
    ///
    /// Returns 0, or a negative errno: `ENOMEM` when the clone cannot be made.
    fn host_fork_process(parent_slot: i32, child_slot: i32) -> i32;
    /// Read `bytes` bytes at byte `offset` of the attached block device, into the caller's memory
    /// at `buf`.
    ///
    /// Returns the number of bytes read, or a negative errno. `ENODEV` means the host has no device
    /// attached — the same answer a hardware arch gets when its PCI probe finds nothing, and the
    /// one MFS asks for (`bdev_has_device`'s `BDEV_OPEN` probe) before it decides whether the root
    /// filesystem lives here or on the ramdisk.
    fn host_block_read(offset: u64, buf: u32, bytes: u32) -> i32;
    /// Write `bytes` bytes from the caller's memory at `src` to the attached block device, at byte
    /// `offset`.
    fn host_block_write(offset: u64, src: u32, bytes: u32) -> i32;
    /// The attached device's capacity in bytes, or 0 when there is none.
    fn host_block_capacity() -> u64;
}

/// What the host needs in order to instantiate a module as a process.
///
/// `#[repr(C)]` with all four-byte fields, so the host reads six `u32`s at 0, 4, 8, 12, 16 and
/// 20. The assertions below are the contract rather than a property of the implementation: a
/// silent layout change here would be read on the other side as plausible numbers.
///
/// The addresses are in two different memories, and the field names say which. The image is in
/// the process named by `image_proc` — the caller, which read the file — while `path_addr` and
/// `argv_addr` are in this instance's own memory: the path because the caller owns it and the
/// kernel passes the pointer along, and the arguments because the kernel parsed them out of the
/// exec frame and had to lay them out where the host can read them.
pub struct ExecModuleRequest {
    /// Whose memory `image_addr` refers to: the calling process's slot.
    pub image_proc: i32,
    /// Where the module's bytes are, and how many. Not copied: the host reads them where they
    /// are, so the only cost of a larger program is the engine's compile.
    pub image_addr: u32,
    pub image_len: u32,
    /// The path the caller executed, NUL-terminated, in `image_proc`'s memory, for the host to
    /// name in what it reports: a message about an image nobody can name is the shape of
    /// failure this port keeps meeting.
    pub path_addr: u32,
    /// `argc` NUL-terminated arguments back to back, in this instance's memory.
    pub argv_addr: u32,
    pub argc: u32,
}

const _: () = assert!(core::mem::size_of::<ExecModuleRequest>() == 24);
const _: () = assert!(core::mem::offset_of!(ExecModuleRequest, image_addr) == 4);
const _: () = assert!(core::mem::offset_of!(ExecModuleRequest, path_addr) == 12);
const _: () = assert!(core::mem::offset_of!(ExecModuleRequest, argc) == 20);

static TSC_SWITCH: AtomicU64 = AtomicU64::new(0);
static PROFILE_CB: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn console_write(byte: u8) {
    // SAFETY: the host supplies every import in this module at instantiation.
    unsafe { host_console_write(byte as u32) };
}

/// Ask the host to instantiate the module named by `request` as the process in `slot` — exec, on
/// this port.
///
/// A wrapper rather than a re-export so the kernel passes a reference and the narrowing happens
/// here, next to the layout it depends on.
pub(crate) fn exec_module(slot: i32, request: &ExecModuleRequest) -> i32 {
    // SAFETY: the host supplies every import in this module at instantiation, and `request` lives
    // on the caller's stack, which outlives a call that is synchronous — that is what lets the
    // import's contract be "valid for the duration of the call".
    unsafe { host_exec_module(slot, core::ptr::from_ref(request) as u32) }
}

/// Ask the host to clone the process in `parent_slot` into `child_slot` — fork, on this port.
pub(crate) fn fork_process(parent_slot: i32, child_slot: i32) -> i32 {
    // SAFETY: the host supplies every import in this module at instantiation. Nothing is
    // borrowed across the call — the arguments are the two slots — so there is nothing for the
    // caller to keep alive.
    unsafe { host_fork_process(parent_slot, child_slot) }
}

pub(crate) fn console_read() -> Option<u8> {
    // SAFETY: see `console_write`.
    let value = unsafe { host_console_read() };
    if value < 0 { None } else { Some(value as u8) }
}

pub(crate) fn console_available() -> i32 {
    // SAFETY: see `console_write`.
    unsafe { host_console_available() }
}

pub(crate) fn cycles() -> u64 {
    // SAFETY: see `console_write`.
    unsafe { host_cycles() }
}

pub(crate) fn halt_host(code: u32) {
    // SAFETY: see `console_write`.
    unsafe { host_halt(code) };
}

/// Perform a cross-process copy through the host.
///
/// Called only by [`hal::CROSS_ADDRESS_SPACE_COPY`], which `kernel::vm`
/// reaches for instead of switching page tables.
pub(crate) fn copy_between_procs(
    src_proc: i32,
    src_addr: u32,
    dst_proc: i32,
    dst_addr: u32,
    bytes: u32,
) -> i32 {
    // SAFETY: see `console_write`.
    unsafe { host_copy_between(src_proc, src_addr, dst_proc, dst_addr, bytes) }
}

pub(crate) fn tsc_switch() -> u64 {
    TSC_SWITCH.load(Ordering::Relaxed)
}

pub(crate) fn set_tsc_switch(val: u64) {
    TSC_SWITCH.store(val, Ordering::Relaxed);
}

pub(crate) fn set_profile_callback(callback: Option<unsafe extern "C" fn()>) {
    // The kernel installs this from its own thread, and nothing reads it
    // concurrently in a single-instance wasm build.
    PROFILE_CB.store(callback.map_or(0, |f| f as usize), Ordering::Relaxed);
}

pub(crate) fn run_profile_callback() {
    let raw = PROFILE_CB.load(Ordering::Relaxed);
    if raw != 0 {
        // SAFETY: only an address stored by `set_profile_callback` is converted
        // back, and its type is fixed at that call site.
        let callback: unsafe extern "C" fn() = unsafe { core::mem::transmute(raw) };
        // SAFETY: the kernel installed a function of exactly this signature.
        unsafe { callback() };
    }
}

/// Trap the instance. Reaching this means the kernel could not continue, so
/// there is nothing to fall back to.
pub(crate) fn trap() -> ! {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::unreachable()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Only reachable if this HAL were linked into a host binary, which the
        // kernel never does. Failing loudly beats returning a bogus value.
        panic!("arch-wasm32: trap on a non-wasm target")
    }
}
