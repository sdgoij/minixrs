//! wasm32 platform layer — the analogue of `kernel-boot` at the host boundary.
//!
//! `kernel-boot` owns multiboot parsing, the trampoline, BSS clearing, and ELF
//! process loading. None of that survives here: the host owns memory and module
//! loading. What remains is the same division of labour as every other arch —
//! this layer owns the entry points and the panic handler, and the kernel below
//! it is unchanged.
//!
//! M2 adds the dispatch protocol (§4.1 of `ARCH_WASM32.md`): the host drives a
//! loop, and this layer answers three questions — who should run next, what did
//! a syscall do, and is that process now blocked. The kernel keeps its own
//! process table and run queues; the host never decides any of it.
//!
//! # Message payloads
//!
//! The kernel's own cross-address-space copy is inert here (there is no address
//! translation), so payload bytes do not move on their own — the *identity* half
//! of IPC does, since `mini_send` writes the source endpoint into the receiver's
//! delivery slot with a plain store. The host moves the bytes, which is the
//! design's answer rather than a workaround: the host is the only layer that can
//! see two instances' memories (§3, §5.1).

#![no_std]
#![no_main]

use core::panic::PanicInfo;
use core::sync::atomic::Ordering;

use kernel::proc::{PMAGIC, Proc, RtsFlags};

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::panic::handle(info)
}

/// The kernel has no printing facility of its own — every arch prints from its
/// platform layer — so a banner goes out through the HAL, which here is a host
/// import.
fn print(s: &str) {
    for byte in s.bytes() {
        arch_wasm32::hal::serial_write_byte(byte);
    }
}

/// Bring the kernel up. The host calls this once, after instantiation.
#[unsafe(no_mangle)]
pub extern "C" fn minix_kernel_init() {
    kernel::init();
    // `panic::handle` inspects per-CPU state, which is only safe to read once
    // `init_cpulocals` has run; without this it must skip that part.
    kernel::panic::mark_cpulocals_ready();

    print("Hello MINIX!\r\n");
    print("kernel: wasm32 instance initialised\r\n");
}

// ------------------------------------------------------- dispatch protocol

/// Flags that mean "this process is waiting on IPC".
const BLOCKED: u32 = RtsFlags::SENDING.bits() | RtsFlags::RECEIVING.bits();

fn proc_at(slot: i32) -> *mut Proc {
    kernel::table::proc_addr(slot)
}

/// Register a process in the kernel's table and make it runnable.
///
/// `endpoint` must be a real endpoint from `minix_make_endpoint`: the kernel
/// resolves a destination by decoding the endpoint, so a hand-picked number will
/// not find the process it names. Returns 0, or -1 if the slot is out of range.
#[unsafe(no_mangle)]
pub extern "C" fn minix_proc_spawn(slot: i32, endpoint: i32) -> i32 {
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        (*rp).p_nr = slot;
        (*rp).p_endpoint = endpoint;
        (*rp).p_caller_q = core::ptr::null_mut();
        (*rp).p_q_link = core::ptr::null_mut();
        (*rp).p_getfrom_e = 0;
        (*rp).p_sendto_e = 0;
        (*rp).p_magic = PMAGIC;
        (*rp).p_rts_flags.store(0, Ordering::Relaxed);
        kernel::sched::enqueue(rp);
        0
    }
}

/// Which process should run next, by slot, or -1 when none is runnable.
///
/// The decision is the kernel's: this only reports it.
#[unsafe(no_mangle)]
pub extern "C" fn minix_step() -> i32 {
    unsafe {
        match kernel::sched::pick_proc() {
            Some(rp) => (*rp).p_nr,
            None => -1,
        }
    }
}

/// Is this process waiting on IPC? 1 or 0, or -1 for a bad slot.
///
/// The host needs this because a syscall's return value does not say whether the
/// caller blocked: `mini_send` returns OK in both cases, which is correct for the
/// kernel and useless to a dispatcher.
#[unsafe(no_mangle)]
pub extern "C" fn minix_proc_blocked(slot: i32) -> i32 {
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        let flags = (*rp).p_rts_flags.load(Ordering::Relaxed);
        if flags & BLOCKED != 0 { 1 } else { 0 }
    }
}

/// Perform one IPC syscall on behalf of `slot`.
///
/// `dst` is an endpoint for SEND, or the endpoint to receive from for RECEIVE.
/// Run-queue membership is the kernel's business: `mini_send` dequeues a caller
/// that blocks, and the matching side re-enqueues it, so nothing here has to.
#[unsafe(no_mangle)]
pub extern "C" fn minix_syscall(slot: i32, nr: i32, dst: i32) -> i32 {
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        match nr {
            // The message pointer is the process's own address space, which the
            // kernel cannot read here; the host moves the bytes instead.
            kernel::ipc::SEND => kernel::ipc::mini_send(rp, dst, core::ptr::null(), 0),
            kernel::ipc::RECEIVE => kernel::ipc::mini_receive(rp, dst, core::ptr::null_mut(), 0),
            _ => -1,
        }
    }
}

/// Take a process out of the scheduler and free its slot.
#[unsafe(no_mangle)]
pub extern "C" fn minix_proc_exit(slot: i32) -> i32 {
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        kernel::sched::remove_from_queue(rp);
        (*rp)
            .p_rts_flags
            .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        0
    }
}

/// Run-queue consistency, so the host can assert the kernel's invariants held
/// across a whole dispatch sequence rather than trusting the trace.
#[unsafe(no_mangle)]
pub extern "C" fn minix_runqueues_ok() -> i32 {
    if unsafe { kernel::sched::runqueues_ok() } {
        1
    } else {
        0
    }
}

/// The endpoint the kernel associates with a process slot.
///
/// The host needs this because endpoints are the kernel's encoding, not an
/// arbitrary handle: `make_endpoint` folds a generation number and a slot
/// together, and IPC resolves a destination by decoding the result.
#[unsafe(no_mangle)]
pub extern "C" fn minix_make_endpoint(slot: i32) -> i32 {
    kernel::table::make_endpoint(0, slot)
}

/// Deliberately panic, so the host can verify the diagnostic path end to end:
/// the message and location reach the console, and the halt path traps.
#[unsafe(no_mangle)]
pub extern "C" fn minix_kernel_trigger_panic() {
    panic!("deliberate M1 panic");
}
