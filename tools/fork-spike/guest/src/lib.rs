//! Fork-spike guest: a miniature of the MINIX `fork()` path.
//!
//! The shape is what the wasm port needs. The process issues a blocking sendrec
//! to PM asking to be forked; the call suspends; and the value PM leaves in the
//! message's reply slot is what tells each instance whether it is the parent
//! (the child's pid) or the child (0).
//!
//! `RESUME_PC` stands in for the serialised call stack. Binaryen's Asyncify
//! keeps that state in a linear-memory buffer instead, but the property under
//! test is the same one: after a suspension, everything needed to continue is
//! reachable from linear memory alone.

#![no_std]

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

/// PM's endpoint, and the message type/target for `fork`.
const PM_PROC_NR: i32 = 0;
const PM_FORK: i32 = 1;
const CALL: i32 = 2;

/// `host_sendrec` returns this when the call cannot complete yet: the caller
/// must unwind. This is exactly where Asyncify would serialise the stack and
/// hand control back to the host.
const SUSPENDED: i32 = -1;

const PC_ENTRY: u32 = 0;
const PC_AFTER_FORK: u32 = 1;

/// Index of the reply slot inside the message (`m_type` at 0, `m2l1`/`m2l2`,
/// then the payload the kernel writes before resuming the process).
const REPLY_SLOT: usize = 3;

/// The stand-in for the saved call stack.
static mut RESUME_PC: u32 = PC_ENTRY;

/// The 64-byte MINIX message buffer.
static mut MSG: [i32; 8] = [0; 8];

// `static mut` access through raw pointers only, so no `static_mut_refs`
// reference is ever created.
unsafe fn msg_set(idx: usize, val: i32) {
    write_volatile(addr_of_mut!(MSG).cast::<i32>().add(idx), val);
}

unsafe fn msg_get(idx: usize) -> i32 {
    read_volatile(addr_of!(MSG).cast::<i32>().add(idx))
}

#[link(wasm_import_module = "minix")]
extern "C" {
    /// Perform the sendrec. Returns the reply, or `SUSPENDED` if the process
    /// must block (in which case the host resumes it later).
    fn host_sendrec(msg_ptr: u32) -> i32;

    /// Diagnostic sink, so the host can show the interleaving of the two
    /// instances after the fork.
    fn host_trace(tag: i32, value: i32);
}

/// Address of the message buffer, so the host never hardcodes offsets.
#[no_mangle]
pub extern "C" fn msg_ptr() -> u32 {
    addr_of_mut!(MSG) as u32
}

/// Address of the reply slot the kernel writes before resuming a process.
#[no_mangle]
pub extern "C" fn reply_slot_ptr() -> u32 {
    addr_of_mut!(MSG) as u32 + (REPLY_SLOT * 4) as u32
}

/// Address of the resume point, for the host to inspect after a suspension.
#[no_mangle]
pub extern "C" fn resume_pc_ptr() -> u32 {
    addr_of_mut!(RESUME_PC) as u32
}

/// The process body. Returns the value `fork()` would hand back to userland.
#[no_mangle]
pub extern "C" fn process_main() -> i32 {
    unsafe {
        if read_volatile(addr_of!(RESUME_PC)) == PC_ENTRY {
            msg_set(0, CALL);
            msg_set(1, PM_PROC_NR);
            msg_set(2, PM_FORK);
            host_trace(1, 0);

            let reply = host_sendrec(addr_of_mut!(MSG) as u32);
            if reply == SUSPENDED {
                // Unwind point. Asyncify serialises the stack into linear
                // memory here instead of writing RESUME_PC, but what matters
                // is identical: the continuation is memory-resident.
                write_volatile(addr_of_mut!(RESUME_PC), PC_AFTER_FORK);
                return SUSPENDED;
            }
            return reply;
        }

        // Resumed: read what the kernel left in the reply slot.
        let reply = msg_get(REPLY_SLOT);
        host_trace(2, reply);
        reply
    }
}

#[no_mangle]
pub extern "C" fn async_process_main() -> i32 {
    unsafe {
        msg_set(0, CALL);
        msg_set(1, PM_PROC_NR);
        msg_set(2, PM_FORK);
        host_trace(1, 0);

        // No RESUME_PC here. The suspension is driven entirely by the host,
        // which calls asyncify_start_unwind from inside host_sendrec; the
        // Asyncify instrumentation saves and restores this local on the way
        // out and back. Straight-line code is the whole point.
        let reply = host_sendrec(addr_of_mut!(MSG) as u32);

        host_trace(2, reply);
        reply
    }
}

// The recursion must produce one *real* stack frame per level, and the backend
// will defeat that if given the chance. `#[inline(never)]` stops inlining;
// `black_box(&scratch)` is what stops the rest — it keeps a stack slot live
// across the recursive call, so the call cannot become a tail call and the
// recursion cannot be rewritten as a loop. Without it LLVM flattened this into
// a loop and the probe reported a constant 48 bytes at every depth.
#[inline(never)]
unsafe fn descend(depth: i32, acc: i32) -> i32 {
    let mut scratch = [0u8; 32];
    core::ptr::write_volatile(scratch.as_mut_ptr(), (acc & 0xff) as u8);

    if depth <= 0 {
        msg_set(0, CALL);
        msg_set(1, PM_PROC_NR);
        msg_set(2, PM_FORK);
        host_trace(1, acc);
        return host_sendrec(addr_of_mut!(MSG) as u32);
    }

    let down = descend(depth - 1, acc + 1);
    core::hint::black_box(&scratch);
    down
}

/// Process body that suspends at the bottom of a stack `depth` frames deep.
/// Used to size the asyncify buffer for a realistic `fork()`.
#[no_mangle]
pub extern "C" fn async_deep_process_main(depth: i32) -> i32 {
    unsafe {
        let reply = descend(depth, 0);
        host_trace(2, reply);
        reply
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}
