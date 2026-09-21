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
    host_block_capacity, host_block_read, host_block_write, host_fb_geometry, host_fb_present,
    run_profile_callback, set_profile_callback, set_tsc_switch, trap, tsc_switch,
};

pub fn init() {
    arch_sim::hal::init();
    // SAFETY: the arena is this crate's own page-aligned static, and nothing else reads or writes it
    // before this point (the kernel calls `init` once, before any allocation).
    unsafe { arch_sim::hal::init_phys_alloc(arena_base(), ARENA_BYTES as u64) };
}

/// The kernel instance's "physical" memory (the frame arena `kernel::hal` hands out), and the
/// reason it is a static rather than `arch-sim`'s stand-in base.
///
/// On a hardware arch "physical" and "kernel virtual" are the same number, and the arena is a range
/// of RAM above the kernel image. On this port there is no such range and no identity to keep
/// (`ARCH_WASM32.md` §5.3): the only memory the kernel can dereference is its own module's, so the
/// arena has to *be* part of it, at an address private to the instance. `arch-sim`'s base is
/// documented as "arbitrary non-zero", which is true where nothing dereferences a returned page and
/// false here — the kernel writes what it was handed through the pointer, and with the default base
/// those pages are the module's own statics. The privilege table lives at 0x100e40, four kilobytes
/// into the first page, so the first exec frame written at 0x100000 landed on top of a `Priv` entry:
/// the desktop's `s_trap_mask` came back as 0, its next `RECEIVE` was refused with `ETRAPDENIED`, and
/// it spun until the front end's syscall budget ran out. A page-aligned static is what makes the
/// arena's addresses mean what §5.3 says they mean.
#[repr(C, align(4096))]
struct ArenaCell(core::cell::UnsafeCell<[u8; ARENA_BYTES]>);

// SAFETY: single-threaded, like every other static the HAL hands out.
unsafe impl Sync for ArenaCell {}

static PHYS_ARENA: ArenaCell = ArenaCell(core::cell::UnsafeCell::new([0u8; ARENA_BYTES]));

/// How much of it there is. Sized by what this port allocates from it, which is one exec frame
/// (bounded at 1 MiB by `do_exec_load_handler`) and VM's bookkeeping regions — not by the 8 MiB a
/// hardware arch's RAM has to spare. A request past the end is refused (`phys_claim` returns
/// `None`), so the failure is a failed exec rather than a write somewhere else.
const ARENA_BYTES: usize = 2 * 1024 * 1024;

fn arena_base() -> u64 {
    core::ptr::from_ref(&PHYS_ARENA) as u64
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

// ------------------------------------------------------------------- block device

/// Bytes of the block device the host has attached, or 0 when it has none (M4).
///
/// The host is the device on this port the way it is the console and the clock: there is no PCI
/// bus and no MMIO window to program, so a driver's access to its hardware is an import. What is
/// *not* the host's decision is anything about the protocol — which driver serves the root, what a
/// sector is, and what a request looks like are all the guest's (`virtio_blk.rs` answers `BDEV_*`,
/// MFS probes `BDEV_OPEN`, and the request path that moves the bytes is the driver's own).
///
/// This is what `virtio_blk_probe` asks instead of scanning a bus, so a host with no device makes
/// the driver answer `NotFound` and MFS's root fall back to the ramdisk — a diskless boot, which is
/// the same disposition a machine with an empty drive has.
pub fn block_capacity() -> u64 {
    // SAFETY: the import has no preconditions; the host answers 0 for "no device".
    unsafe { host_block_capacity() }
}

/// Read into `buf` from byte `offset` of the attached block device.
///
/// Returns the bytes read, or a negative errno — `ENODEV` when there is no device, `EIO` when the
/// host's store could not be reached. A read that runs past the device's end returns short, which
/// is what a block driver reports and what the filesystem above expects for a short device.
pub fn block_read(offset: u64, buf: &mut [u8]) -> i32 {
    let bytes = buf.len();
    if bytes > u32::MAX as usize {
        return EINVAL;
    }
    // SAFETY: `buf` is live and `bytes` long for the duration of the call, which is synchronous.
    unsafe { host_block_read(offset, buf.as_mut_ptr() as u32, bytes as u32) }
}

/// Write `buf` to byte `offset` of the attached block device.
///
/// A write is durable when it returns: the host persists the pages it dirtied before answering,
/// which is why there is nothing for the guest to flush and why a killed run loses only the write
/// in flight. The alternative — a write cache in the host with a flush the guest has to ask for —
/// would be a second place for data to be lost, and the guest's own cache is the one that matters.
pub fn block_write(offset: u64, buf: &[u8]) -> i32 {
    let bytes = buf.len();
    if bytes > u32::MAX as usize {
        return EINVAL;
    }
    // SAFETY: `buf` is live and `bytes` long for the duration of the call, which is synchronous.
    unsafe { host_block_write(offset, buf.as_ptr() as u32, bytes as u32) }
}

/// `EINVAL`, for an argument the host could not be given.
const EINVAL: i32 = -22;

// ---------------------------------------------------------------------- display

/// The display's mode as `(width, height)`, or `(0, 0)` when the host has no display (M5).
///
/// The host is the device on this port the way it is the console, the clock and the disk, and a
/// display is the one of them whose mode the guest does not choose: a canvas in a page is the
/// size the page made it, so the driver adopts it the way a driver adopts a panel's mode from
/// EDID. `fb`'s backend asks this instead of probing a bus.
pub fn fb_geometry() -> (u32, u32) {
    // SAFETY: the import has no preconditions; the host answers 0 for "no display".
    let packed = unsafe { host_fb_geometry() };
    ((packed >> 32) as u32, packed as u32)
}

/// Publish `buf` — a whole surface in the mode `fb_geometry` named — to the display.
///
/// Returns 0, or a negative errno: `ENODEV` when the host has no display, `EINVAL` for a buffer
/// this instance could not describe.
///
/// The pixels leave through here rather than through device memory because a page has none to
/// map: VFS's device-`mmap` path asks the kernel to map a *physical* range, and this port has no
/// address translation to do that with. What the display gets instead is the driver's flush.
pub fn fb_present(buf: &[u8]) -> i32 {
    let bytes = buf.len();
    if bytes > u32::MAX as usize {
        return EINVAL;
    }
    // SAFETY: `buf` is live and `bytes` long for the duration of the call, which is synchronous,
    // and the host reads only the bytes it is told about.
    unsafe { host_fb_present(buf.as_ptr() as u32, bytes as u32) }
}

/// The host's next queued input event, as `(page, code, press)`, or `None` when it has none (M5c).
///
/// The input server is the caller, and it calls this until it answers `None`: the host queues a DOM
/// event and raises the line the server registered with SYS_IRQCTL, whose handler wakes it, so an
/// empty answer is the end of a drain rather than a failure. The *pages* the host uses are the HID
/// ones the other backends decode into (`INPUT_PAGE_KEY`, `INPUT_PAGE_ABS`, ...), which is what
/// makes this a third backend for one driver rather than a second event format.
///
/// On an arch with a device the answer is always `None`: the keyboard there is an 8042 or a
/// virtio-input, and a drain has nothing to ask a host it does not have.
pub fn input_event() -> Option<(u16, u16, i32)> {
    crate::input_event()
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

/// Ask the host to clone the process in `parent_slot` into `child_slot`.
///
/// Fork on this port (§12 risk 1): the host owns both memories, so the clone is its work, and
/// the kernel's is to know that a fork happened and which two slots it names. Called from the
/// wasm arm of `do_fork_handler`.
///
/// # Safety
///
/// Both slots must name processes the host has instances for; `child_slot` must be empty. The
/// call is synchronous and borrows nothing.
pub unsafe fn fork_process(parent_slot: i32, child_slot: i32) -> i32 {
    crate::fork_process(parent_slot, child_slot)
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
