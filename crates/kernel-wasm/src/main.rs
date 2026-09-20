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

    // The same sequence `kernel-boot` runs before it starts any process, and it
    // is not optional here either: `proc_init` builds the process table *and*
    // attaches a privilege structure to each boot process, `system_init` fills
    // the kernel-call vector (`SYS_VIRCOPY` among them), and
    // `register_ipc_syscalls` maps the IPC call numbers. Leaving any of them out
    // is invisible until something depends on it — no privileges means a
    // pending notification is silently never delivered, and an empty call vector
    // means `sys_vircopy` answers ENOSYS.
    // SAFETY: single-threaded, and each only writes tables nothing has read yet.
    unsafe {
        kernel::table::proc_init();
        kernel::system::system_init();
        kernel::ipc::register_ipc_syscalls();
        // Fill the syscall table. Until this runs, every number resolves to -38,
        // so a process's very first getpid would fail.
        kernel::syscall::init_basic_syscalls();
    }

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

/// The same, for a slot that is an **ordinary user process** rather than a server.
///
/// `minix_proc_spawn` above fills in a slot and nothing else, which is right for every server
/// here: `proc_init` already attached a privilege structure of its own to each `BOOT_IMAGE`
/// entry. A slot outside that image has none, and a priv-less slot is not the same thing as a
/// user process — it cannot send to PM, VFS, VM or DS, because there is no `s_ipc_to` mask to
/// allow it. So the two exports are separate rather than one with a flag, and the harness asks
/// the kernel which kind it made (`minix_proc_kind`) instead of trusting the slot number.
///
/// The privilege link is `do_fork`'s, which is the kernel's own statement of what a new user
/// process is (`system.rs`: `rpc->p_priv = priv_addr(USER_PRIV_ID)`). Exec keeps the caller's
/// structure rather than taking a new one, so a program started from INIT gets this one too —
/// which is why the link is here, in the arch layer that creates the process, and not in a
/// program module that cannot see the privilege table.
///
/// What is deliberately *not* copied from `do_fork` is its `RTS_NO_PRIV`. That flag is MINIX's
/// "created, but the scheduler has not started it yet", and clearing it is `sched_start_user`'s
/// job — reached here through `do_schedule_handler`, which there is no SCHED server to call in
/// this harness. `minix_proc_spawn` above is what starts a process here (it stores a zero into
/// `p_rts_flags`, which is the only reason the servers and INIT run at all), and `NO_PRIV` on
/// its own makes a process unrunnable — `is_runnable` is `p_rts_flags == 0`. So matching
/// `do_fork` line for line here would re-block the process the spawn just started: the program
/// is never picked, its trace stays empty, and the run-queue check fails.
#[unsafe(no_mangle)]
pub extern "C" fn minix_proc_spawn_user(slot: i32, endpoint: i32) -> i32 {
    let rc = minix_proc_spawn(slot, endpoint);
    if rc != 0 {
        return rc;
    }
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        (*rp).p_priv = kernel::r#priv::priv_addr_mut(kernel::r#priv::USER_PRIV_ID)
            as *mut kernel::r#priv::Priv;
        0
    }
}

/// Set the boot notification that starts PM's chain. Returns 0, or -1 if PM's
/// privilege structure is not reachable.
///
/// This is what `boot_init::enqueue_and_start` does on the shipping arches: RS's
/// notification is left pending on PM's priv structure, and PM discovers it the
/// first time it calls RECEIVE. Nothing is sent and nothing is copied — the
/// notification is kernel state, which is why the whole thing is a bit set
/// rather than a cross-instance transfer.
#[unsafe(no_mangle)]
pub extern "C" fn minix_boot_notify() -> i32 {
    unsafe {
        let pm = proc_at(arch_common::com::PM_PROC_NR);
        if pm.is_null() || (*pm).p_priv.is_null() {
            return -1;
        }
        let Some(rs_id) = kernel::r#priv::priv_find_proc_id(arch_common::com::RS_PROC_NR) else {
            return -1;
        };
        (*(*pm).p_priv).s_notify_pending.set(rs_id);
        0
    }
}

/// Which process should run next, by slot, or -1 when none is runnable.
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

/// The syscall gate.
///
/// Every MINIX syscall number arrives here and is routed exactly as the kernel
/// routes it on the other arches — through the table `init_basic_syscalls`
/// fills. Nothing is special-cased, which is the point: a server's `getpid`,
/// its `SENDREC`, and its `brk` take the same path they take on x86_64, and an
/// unregistered number still answers -38.
///
/// The only wasm-specific part is how the caller is identified: the host passes
/// the slot it is dispatching, where a hardware entry would read the current
/// proc from the CPU's local storage.
#[unsafe(no_mangle)]
pub extern "C" fn minix_syscall(
    slot: i32,
    nr: i64,
    a0: u64,
    a1: u64,
    a2: u64,
    a3: u64,
    a4: u64,
    a5: u64,
) -> i64 {
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        let args = [a0, a1, a2, a3, a4, a5];
        let result = kernel::syscall::dispatch_basic_syscall(rp, nr as usize, &args);

        // A syscall that queued a message leaves `DELIVERMSG` on the caller for
        // the arch's syscall-return path to act on. This instance has no such
        // epilogue — the host returns from this import straight into the process
        // — so the copy happens here, while the process is still inside the
        // syscall and before it can look at its buffer.
        kernel::ipc::deliver_pending_msg(rp);

        // The same missing epilogue costs a blocked call its return value: the
        // process is suspended inside the syscall and there is no return path to
        // carry the result out, where a hardware arch would restore the frame
        // the result was written into. Record it in the frame, so
        // `minix_proc_retval` can answer with it when the host rewinds — and so
        // that a later write to the same slot (a receive's sender endpoint,
        // which `mini_send` stores) overwrites a value rather than being the
        // only write.
        if (*rp).p_rts_flags.load(Ordering::Relaxed) & BLOCKED != 0 {
            arch_wasm32::hal::write_retval(&mut (*rp).p_reg, result as u64);
        }

        result
    }
}

/// The return value the kernel holds for `slot`'s suspended syscall.
///
/// A call that blocked has not returned to the instance yet. On a hardware arch
/// the syscall-return epilogue restores the saved frame on resume; a wasm
/// instance has no such path, so the host asks for the value here when it
/// rewinds. Returns -1 for a slot that is out of range.
#[unsafe(no_mangle)]
pub extern "C" fn minix_proc_retval(slot: i32) -> i64 {
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        arch_wasm32::hal::read_retval(&(*rp).p_reg) as i64
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

/// Which kind of process the kernel believes `slot` is.
///
/// Spawning a slot is not the same as that slot being the kind of process the host
/// intended, and the difference lives in the privilege structure `proc_init`
/// attaches: a server gets a slot of its own with `SYS_PROC`, an ordinary user gets
/// the shared USER slot without it. Finding 9 is what it looks like when that table
/// is never built at all — no process had privileges and nothing noticed, because
/// the servers' init only touches their own statics. So the host asks.
///
/// * `2` — a system server: `SYS_PROC` set, a privilege slot of its own.
/// * `1` — an ordinary user process, on the shared USER slot.
/// * `0` — no privilege structure attached to that slot.
/// * `-1` — not a slot.
#[unsafe(no_mangle)]
pub extern "C" fn minix_proc_kind(slot: i32) -> i32 {
    unsafe {
        let rp = proc_at(slot);
        if rp.is_null() {
            return -1;
        }
        let p = (*rp).p_priv;
        if p.is_null() {
            return 0;
        }
        if (*p).s_flags.contains(kernel::r#priv::PrivFlags::SYS_PROC) {
            2
        } else {
            1
        }
    }
}

/// Deliberately panic, so the host can verify the diagnostic path end to end:
/// the message and location reach the console, and the halt path traps.
#[unsafe(no_mangle)]
pub extern "C" fn minix_kernel_trigger_panic() {
    panic!("deliberate M1 panic");
}
