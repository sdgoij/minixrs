//! The kernel's HAL surface for wasm32.
//!
//! The glob below brings in every item that has no host boundary — paging stubs,
//! frame accessors, the VA layout, the page arena, CPU and scheduler statics.
//! The explicit definitions that follow *shadow* it for the pieces that do cross
//! into the host, which is why there is no ambiguity to resolve: in Rust an
//! explicit item always wins over a glob import.

pub use arch_sim::hal::*;

use crate::{
    console_available, console_read, console_write, copy_between_procs, cycles, halt_host,
    run_profile_callback, set_profile_callback, set_tsc_switch, trap, tsc_switch,
};

pub fn init() {
    arch_sim::hal::init();
}

// ------------------------------------------------- cross-address-space copy

/// The hook `kernel::vm::virtual_copy` uses when page tables cannot join two
/// address spaces.
///
/// A value rather than a function so the hardware arches can state `None` —
/// one word saying "my CR3 switch already does this" beats three copies of an
/// unreachable body. On this port the two address spaces are two linear
/// memories and only the host owns both, so the copy is genuinely the host's
/// to make.
///
pub static CROSS_ADDRESS_SPACE_COPY: Option<arch_common::safecopies::CrossAddressSpaceCopy> =
    Some(copy_between_address_spaces);

/// # Safety
///
/// The addresses must be valid for the named processes; the host reports
/// `EFAULT` for any it cannot reach rather than trapping.
unsafe fn copy_between_address_spaces(
    src_proc: i32,
    src_addr: u64,
    dst_proc: i32,
    dst_addr: u64,
    bytes: usize,
) -> i32 {
    /// `EFAULT` — the address is not one this instance could ever hold.
    const EFAULT: i32 = -14;

    // Narrowing here rather than letting `as u32` do it: a message can carry a
    // 64-bit address, and silently truncating it would turn a bogus address
    // into a plausible one. `MAX_USER_ADDRESS` is 256 MiB, so anything above
    // 4 GiB is garbage by construction and is refused the way an unmapped
    // address would be.
    if src_addr > u32::MAX as u64 || dst_addr > u32::MAX as u64 || bytes > u32::MAX as usize {
        return EFAULT;
    }

    copy_between_procs(
        src_proc,
        src_addr as u32,
        dst_proc,
        dst_addr as u32,
        bytes as u32,
    )
}

// ------------------------------------------------------------------- exec

pub use crate::ExecModuleRequest;

/// Ask the host to instantiate the module named by `request` as the process in `slot`.
///
/// Exec on this port (§7.2 of `ARCH_WASM32.md`): a program is a wasm module and only the host
/// can instantiate one, so the kernel's half is to say which process, which bytes and with what
/// arguments. Called from the wasm arm of `do_exec_load_handler`, which is where the image would
/// otherwise be installed.
///
/// # Safety
///
/// `request` must be live for the duration of the call (it is synchronous: the host reads the
/// structure and everything it points at before returning), `argv_addr` must hold `argc`
/// NUL-terminated strings, and `image_addr`/`path_addr` must be valid addresses in the memory of
/// the process the request names.
pub unsafe fn exec_module(slot: i32, request: &ExecModuleRequest) -> i32 {
    crate::exec_module(slot, request)
}

// ------------------------------------------------------------------ console

pub fn serial_write_byte(byte: u8) {
    console_write(byte);
}

pub fn serial_byte_available() -> bool {
    console_available() > 0
}

pub fn poll_console() -> Option<u8> {
    console_read()
}

/// Blocking read. Bounded rather than unbounded: with no timer interrupt there is
/// nothing to wake a wait, so a missing byte would hang the host rather than fail
/// it. Panicking names the problem instead.
pub fn serial_read_byte() -> u8 {
    for _ in 0..1_000_000 {
        if let Some(byte) = console_read() {
            return byte;
        }
    }
    panic!("arch-wasm32: serial_read_byte found no input (nothing will supply one)");
}

// --------------------------------------------------------------------- clock

/// The host is the clock source: there is no TSC, and no timer interrupt to
/// sample it from.
pub fn read_cycles() -> u64 {
    cycles()
}

pub fn read_tsc() -> u64 {
    cycles()
}

/// # Safety
///
/// No preconditions; `unsafe` only to match the other ports' signatures.
pub unsafe fn read_tsc_ctr_switch() -> u64 {
    tsc_switch()
}

/// # Safety
///
/// No preconditions; see `read_tsc_ctr_switch`.
pub unsafe fn write_tsc_ctr_switch(val: u64) {
    set_tsc_switch(val);
}

/// # Safety
///
/// `callback` must be callable with no arguments.
pub unsafe fn init_profile_clock(_rate_code: u32, callback: unsafe extern "C" fn()) -> i32 {
    set_profile_callback(Some(callback));
    0
}

pub fn stop_profile_clock() {
    set_profile_callback(None);
}

/// Run the profiler callback, if one is installed. Sampling has to be driven
/// explicitly because there is no timer interrupt to drive it.
pub fn profile_tick() {
    run_profile_callback();
}

// -------------------------------------------------------------- stop paths

/// Idling does not need to advance anything: the clock is the host's, and the
/// host is what resumes an instance.
pub fn cpu_idle() {}

pub fn pause() {}

pub fn hlt() {}

pub fn halt() -> ! {
    halt_host(0);
    trap()
}

pub fn qemu_exit(code: u32) -> ! {
    halt_host(code);
    trap()
}

// ------------------------------------------------------------- VA layout
//
// The layout is this port's own, and it is *compact* — which the x86_64 numbers
// globbed from `arch-sim` are not. A wasm instance's linear memory is flat: there
// is no page table to map a high virtual address onto a low physical one, so
// every address a process uses has to physically exist in its instance. x86_64's
// heap at 0x3FE0_0000 would therefore require every process to carry a gigabyte
// of memory. These values instead describe a 16 MiB process that can grow.
//
// The heap base is load-bearing beyond this crate: `minix_rt::HEAP_BASE` and the
// kernel's `sys_brk_handler` window both have to agree with it, and the kernel
// derives its window from `user_heap_base()` precisely so there is one source of
// truth.

/// Nothing is mapped into a process's address space by a kernel here; there is
/// no kernel half.
pub const fn kern_vaddr() -> u64 {
    0
}

/// Bottom of the heap window: `[user_heap_base(), +1 MiB)` is what the kernel's
/// brk accepts before VM would grow it.
pub const fn user_heap_base() -> u64 {
    0x0020_0000
}

/// Exclusive upper bound for brk growth, below the anonymous-mmap base.
pub const fn user_heap_limit() -> u64 {
    0x0060_0000
}

pub const fn mmap_base() -> u64 {
    0x0060_0000
}

/// The stack region, growing down from its top. Kept clear of the heap and mmap
/// ranges below it.
pub const fn user_stack_base() -> u64 {
    0x00E0_0000
}

pub const fn user_stack_size() -> usize {
    0x0020_0000
}

/// The instance's grown ceiling: what `MAX_USER_ADDRESS` means is "how far this
/// process may ever reach", and for an instance that is its memory maximum.
pub const MAX_USER_ADDRESS: u64 = 0x1000_0000;
