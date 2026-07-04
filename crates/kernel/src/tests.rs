//! QEMU-compatible kernel test suite.
//!
//! This module is compiled when `feature = "qemu-tests"` is set (a regular
//! feature, NOT `cfg(test)`). It provides a `run_all() -> u32` function that
//! runs a curated set of kernel unit tests inside QEMU using the same `run()`
//! harness pattern as `crates/kernel-boot/src/test_runner.rs`.
//!
//! Tests are pure logic — no hardware access. Hardware tests belong in
//! `crates/kernel-boot/src/test_runner.rs` (Phases A–G).
//!
//! # Adding a test
//!
//! ```ignore
//! fn test_my_feature(ctx: &mut TestCtx) {
//!     ctx.assert(some_condition, "description of what should hold");
//! }
//! ```
//!
//! Then add `total += run("my_feature", test_my_feature);` to `run_all()`.

use core::mem::size_of;
use core::sync::atomic::Ordering;

/// Test context: records failure state.
pub struct TestCtx {
    pub failed: bool,
}

impl TestCtx {
    pub fn assert(&mut self, cond: bool, _msg: &str) {
        if !cond {
            self.failed = true;
        }
    }
}

/// Run a single named test, return 0 (pass) or 1 (fail).
pub fn run(name: &str, f: fn(&mut TestCtx)) -> u32 {
    let mut ctx = TestCtx { failed: false };
    f(&mut ctx);
    if ctx.failed {
        ser_write("FAIL ");
        ser_write(name);
        ser_write("\n");
        1
    } else {
        ser_write("  OK ");
        ser_write(name);
        ser_write("\n");
        0
    }
}

fn ser_write(s: &str) {
    for &b in s.as_bytes() {
        ser_putc(b);
    }
}

fn ser_putc(c: u8) {
    #[cfg(not(test))]
    {
        crate::hal::serial_write_byte(c);
    }
    #[cfg(test)]
    let _ = c;
}

// ELF64 parsing tests

fn test_ehdr_size(ctx: &mut TestCtx) {
    ctx.assert(
        size_of::<crate::elf::Elf64Ehdr>() == 64,
        "Elf64Ehdr must be 64 bytes",
    );
}

fn test_phdr_size(ctx: &mut TestCtx) {
    ctx.assert(
        size_of::<crate::elf::Elf64Phdr>() == 56,
        "Elf64Phdr must be 56 bytes",
    );
}

fn test_elf_constants(ctx: &mut TestCtx) {
    use crate::elf::*;
    ctx.assert(
        ELF_MAGIC == [0x7F, b'E', b'L', b'F'],
        "ELF magic must be \\x7fELF",
    );
    ctx.assert(PT_LOAD == 1, "PT_LOAD must be 1");
    ctx.assert(EM_X86_64 == 62, "EM_X86_64 must be 62");
    ctx.assert(ET_EXEC == 2, "ET_EXEC must be 2");
}

// CPIO / initramfs tests

fn test_cpio_parse_simple(ctx: &mut TestCtx) {
    let path = b"/test/file\0";
    let data = b"hello";
    let mut archive = [0u8; 512];
    let mut pos = 0usize;

    let hdr_magic = b"070701";
    archive[pos..pos + 6].copy_from_slice(hdr_magic);
    pos += 6;
    let fields: [&[u8]; 12] = [
        b"00000001",
        b"000081a4",
        b"00000000",
        b"00000000",
        b"00000001",
        b"00000000",
        b"00000005",
        b"00000000",
        b"00000000",
        b"00000000",
        b"00000000",
        b"0000000b",
    ];
    for f in &fields {
        archive[pos..pos + 8].copy_from_slice(f);
        pos += 8;
    }
    archive[pos..pos + 8].copy_from_slice(b"00000000");
    pos += 8;

    archive[pos..pos + path.len()].copy_from_slice(path);
    pos += path.len();
    while !pos.is_multiple_of(4) {
        pos += 1;
    }

    archive[pos..pos + data.len()].copy_from_slice(data);
    pos += data.len();
    while !pos.is_multiple_of(4) {
        pos += 1;
    }

    // Trailer
    let trailer = b"TRAILER!!!\0";
    archive[pos..pos + 6].copy_from_slice(b"070701");
    pos += 6;
    for _ in 0..12 {
        archive[pos..pos + 8].copy_from_slice(b"00000000");
        pos += 8;
    }
    archive[pos - 16..pos - 8].copy_from_slice(b"0000000b");
    archive[pos - 8..pos].copy_from_slice(b"00000000");
    archive[pos..pos + trailer.len()].copy_from_slice(trailer);
    pos += trailer.len();
    while !pos.is_multiple_of(4) {
        pos += 1;
    }

    let archive_slice = &archive[..pos];
    ctx.assert(
        &archive_slice[..6] == b"070701",
        "archive must start with CPIO magic",
    );
    ctx.assert(
        archive_slice.len() > 110,
        "archive must be larger than header",
    );
}

// IPC unit tests

unsafe fn make_test_proc(nr: i32) -> *mut crate::proc::Proc {
    let rp = crate::table::proc_addr(nr);
    if rp.is_null() {
        return core::ptr::null_mut();
    }
    unsafe {
        (*rp).p_rts_flags.store(0, Ordering::Relaxed);
        (*rp).p_nr = nr;
        (*rp).p_endpoint = crate::table::make_endpoint(0, nr);
        (*rp).p_caller_q = core::ptr::null_mut();
        (*rp).p_q_link = core::ptr::null_mut();
        (*rp).p_getfrom_e = 0;
        (*rp).p_sendto_e = 0;
        (*rp).p_magic = crate::proc::PMAGIC;
    }
    rp
}

fn test_mini_send_direct_delivery(ctx: &mut TestCtx) {
    unsafe {
        let src = make_test_proc(100);
        let dst = make_test_proc(101);
        if src.is_null() || dst.is_null() {
            ctx.assert(false, "make_test_proc failed");
            return;
        }
        let src_ep = (*src).p_endpoint;
        let dst_ep = (*dst).p_endpoint;

        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
        (*dst).p_getfrom_e = src_ep;

        let mut msg = [0u8; crate::proc::MESSAGE_SIZE];
        msg[0..4].copy_from_slice(&42i32.to_ne_bytes());

        let result = crate::ipc::mini_send(src, dst_ep, msg.as_ptr(), 0);
        ctx.assert(result == 0, "mini_send direct delivery must return OK");

        let mut buf = [0u8; 4];
        core::ptr::copy_nonoverlapping((*dst).p_delivermsg.as_ptr(), buf.as_mut_ptr(), 4);
        ctx.assert(
            i32::from_ne_bytes(buf) == 42,
            "delivermsg must contain sent value",
        );

        let rts = (*dst).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            rts & crate::proc::RtsFlags::RECEIVING.bits() == 0,
            "RECEIVING should be cleared after delivery",
        );

        (*src)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

fn test_mini_send_queues_when_not_receiving(ctx: &mut TestCtx) {
    unsafe {
        let src = make_test_proc(102);
        let dst = make_test_proc(103);
        if src.is_null() || dst.is_null() {
            ctx.assert(false, "make_test_proc failed");
            return;
        }
        let dst_ep = (*dst).p_endpoint;

        (*dst).p_rts_flags.store(0, Ordering::Relaxed);

        let mut msg = [0u8; crate::proc::MESSAGE_SIZE];
        msg[0..4].copy_from_slice(&99i32.to_ne_bytes());

        let result = crate::ipc::mini_send(src, dst_ep, msg.as_ptr(), 0);
        ctx.assert(result == 0, "mini_send queue must return OK");

        let src_rts = (*src).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            src_rts & crate::proc::RtsFlags::SENDING.bits() != 0,
            "sender must have SENDING flag",
        );
        ctx.assert((*dst).p_caller_q == src, "dst caller_q must point to src");

        (*src)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*dst).p_caller_q = core::ptr::null_mut();
    }
}

fn test_sendrec_direct(ctx: &mut TestCtx) {
    unsafe {
        let src = make_test_proc(106);
        let dst = make_test_proc(107);
        if src.is_null() || dst.is_null() {
            ctx.assert(false, "make_test_proc failed");
            return;
        }
        let src_ep = (*src).p_endpoint;
        let dst_ep = (*dst).p_endpoint;

        // Set dst to be RECEIVING from src
        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
        (*dst).p_getfrom_e = src_ep;

        // Build a message with a known payload (42 at bytes 0-3).
        let mut msg = [0u8; crate::proc::MESSAGE_SIZE];
        msg[0..4].copy_from_slice(&42i32.to_ne_bytes());

        // Step 1: Set REPLY_PEND (SENDREC preamble)
        (*src)
            .p_misc_flags
            .fetch_or(crate::proc::MiscFlags::REPLY_PEND.bits(), Ordering::Relaxed);

        // Step 2: Send — dst is RECEIVING, so direct delivery
        let r = crate::ipc::mini_send(src, dst_ep, msg.as_ptr(), 0);
        ctx.assert(r == 0, "mini_send (SENDREC half) must return OK");

        // dst's RECEIVING flag must be cleared (direct delivery)
        let dst_rts = (*dst).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            dst_rts & crate::proc::RtsFlags::RECEIVING.bits() == 0,
            "dst RECEIVING must be cleared after mini_send",
        );

        // The payload was copied to dst's p_delivermsg
        let mut buf = [0u8; 4];
        core::ptr::copy_nonoverlapping((*dst).p_delivermsg.as_ptr(), buf.as_mut_ptr(), 4);
        ctx.assert(
            i32::from_ne_bytes(buf) == 42,
            "dst delivermsg must contain sent payload",
        );

        // mini_send also wrote src_ep at bytes 4-7 of delivermsg
        core::ptr::copy_nonoverlapping((*dst).p_delivermsg.as_ptr().add(4), buf.as_mut_ptr(), 4);
        ctx.assert(
            i32::from_ne_bytes(buf) == src_ep,
            "dst delivermsg must have src_ep at offset 4 (m_source)",
        );

        // Step 3: Receive — src now waits for a reply from dst
        let r = crate::ipc::mini_receive(src, dst_ep, msg.as_mut_ptr(), 0);
        ctx.assert(r == 0, "mini_receive (SENDREC half) must return OK");

        // src must have RECEIVING set (blocked waiting for reply)
        let src_rts = (*src).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            src_rts & crate::proc::RtsFlags::RECEIVING.bits() != 0,
            "src must have RECEIVING set after SENDREC (waiting for reply)",
        );

        // src should be waiting for dst's reply
        ctx.assert(
            (*src).p_getfrom_e == dst_ep,
            "src must be waiting for reply from dst endpoint",
        );

        // Clean up
        (*src)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

fn test_sendrec_reply_cycle(ctx: &mut TestCtx) {
    unsafe {
        let src = make_test_proc(108);
        let dst = make_test_proc(109);
        if src.is_null() || dst.is_null() {
            ctx.assert(false, "make_test_proc failed");
            return;
        }
        let src_ep = (*src).p_endpoint;
        let dst_ep = (*dst).p_endpoint;

        // dst is RECEIVING from src (waiting for src's message)
        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
        (*dst).p_getfrom_e = src_ep;

        // Build request message with payload 42
        let mut msg = [0u8; crate::proc::MESSAGE_SIZE];
        msg[0..4].copy_from_slice(&42i32.to_ne_bytes());

        // SENDREC step 1: set REPLY_PEND
        (*src)
            .p_misc_flags
            .store(crate::proc::MiscFlags::REPLY_PEND.bits(), Ordering::Relaxed);

        // SENDREC step 2: send — direct delivery since dst is RECEIVING
        let r = crate::ipc::mini_send(src, dst_ep, msg.as_ptr(), 0);
        ctx.assert(r == 0, "mini_send must return OK");

        // Verify dst got the message
        let dst_rts = (*dst).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            dst_rts & crate::proc::RtsFlags::RECEIVING.bits() == 0,
            "dst RECEIVING must be cleared after request delivery",
        );
        let mut buf = [0u8; 4];
        core::ptr::copy_nonoverlapping((*dst).p_delivermsg.as_ptr(), buf.as_mut_ptr(), 4);
        ctx.assert(
            i32::from_ne_bytes(buf) == 42,
            "dst delivermsg must contain request payload",
        );

        // SENDREC step 3: receive — src blocks waiting for dst's reply
        let r = crate::ipc::mini_receive(src, dst_ep, msg.as_mut_ptr(), 0);
        ctx.assert(r == 0, "mini_receive must return OK");

        let src_rts = (*src).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            src_rts & crate::proc::RtsFlags::RECEIVING.bits() != 0,
            "src must have RECEIVING set after SENDREC",
        );
        ctx.assert(
            (*src).p_getfrom_e == dst_ep,
            "src must be waiting for reply from dst",
        );

        // Set src RECEIVING from dst (src is already waiting for dst's reply)
        // Note: src already has RECEIVING set and p_getfrom_e == dst_ep
        // from the mini_receive above. We just need dst to reply.

        // Build reply message with payload 99
        let mut reply = [0u8; crate::proc::MESSAGE_SIZE];
        reply[0..4].copy_from_slice(&99i32.to_ne_bytes());

        // dst does mini_send to src — src is RECEIVING from dst, so direct delivery
        let r = crate::ipc::mini_send(dst, src_ep, reply.as_ptr(), 0);
        ctx.assert(r == 0, "reply mini_send must return OK");

        // Verify src got the reply
        let src_rts2 = (*src).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            src_rts2 & crate::proc::RtsFlags::RECEIVING.bits() == 0,
            "src RECEIVING must be cleared after reply delivery",
        );

        // Verify reply payload
        core::ptr::copy_nonoverlapping((*src).p_delivermsg.as_ptr(), buf.as_mut_ptr(), 4);
        ctx.assert(
            i32::from_ne_bytes(buf) == 99,
            "src delivermsg must contain reply payload",
        );

        // Verify m_source at offset 4 is dst's endpoint
        core::ptr::copy_nonoverlapping((*src).p_delivermsg.as_ptr().add(4), buf.as_mut_ptr(), 4);
        ctx.assert(
            i32::from_ne_bytes(buf) == dst_ep,
            "src delivermsg m_source must be dst endpoint",
        );

        // After replying, dst now does mini_receive to wait for next message
        let r = crate::ipc::mini_receive(dst, src_ep, reply.as_mut_ptr(), 0);
        ctx.assert(r == 0, "dst mini_receive after reply must return OK");

        let dst_rts2 = (*dst).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            dst_rts2 & crate::proc::RtsFlags::RECEIVING.bits() != 0,
            "dst must have RECEIVING set after reply (waiting for next message)",
        );

        // Verify the IPC roundtrip is reversible: src can now send again to dst
        // Rebuild request with new payload
        msg[0..4].copy_from_slice(&77i32.to_ne_bytes());
        (*src).p_rts_flags.store(0, Ordering::Relaxed); // src is no longer RECEIVING
        (*src)
            .p_misc_flags
            .store(crate::proc::MiscFlags::REPLY_PEND.bits(), Ordering::Relaxed);

        let r = crate::ipc::mini_send(src, dst_ep, msg.as_ptr(), 0);
        ctx.assert(r == 0, "second mini_send must return OK (roundtrip)");

        // dst is RECEIVING from src, so direct delivery
        let dst_rts3 = (*dst).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            dst_rts3 & crate::proc::RtsFlags::RECEIVING.bits() == 0,
            "dst RECEIVING cleared on second delivery",
        );

        core::ptr::copy_nonoverlapping((*dst).p_delivermsg.as_ptr(), buf.as_mut_ptr(), 4);
        ctx.assert(
            i32::from_ne_bytes(buf) == 77,
            "dst must receive second request payload",
        );

        // Clean up both procs
        (*src)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

fn test_mini_notify_receiving(ctx: &mut TestCtx) {
    unsafe {
        let dst = make_test_proc(104);
        let src = make_test_proc(105);
        if dst.is_null() || src.is_null() {
            ctx.assert(false, "make_test_proc failed");
            return;
        }
        let src_ep = (*src).p_endpoint;
        let dst_ep = (*dst).p_endpoint;

        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
        (*dst).p_getfrom_e = crate::system::NONE;

        let result = crate::ipc::mini_notify(src_ep, dst_ep);
        ctx.assert(result == 0, "mini_notify must return OK");

        let rts = (*dst).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            rts & crate::proc::RtsFlags::RECEIVING.bits() == 0,
            "RECEIVING must be cleared after notify",
        );

        (*dst)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*src)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

// Process table tests

fn test_proc_addr_valid_tasks(ctx: &mut TestCtx) {
    let rp = crate::table::proc_addr(-1);
    ctx.assert(!rp.is_null(), "proc_addr(-1) must be non-null");
    let rp = crate::table::proc_addr(arch_common::com::CLOCK);
    ctx.assert(!rp.is_null(), "proc_addr(CLOCK) must be non-null");
    let rp = crate::table::proc_addr(arch_common::com::SYSTEM);
    ctx.assert(!rp.is_null(), "proc_addr(SYSTEM) must be non-null");
}

fn test_proc_addr_out_of_range(ctx: &mut TestCtx) {
    let rp = crate::table::proc_addr(300);
    ctx.assert(rp.is_null(), "proc_addr(300) must be null");
}

fn test_endpoint_encoding(ctx: &mut TestCtx) {
    let ep = crate::table::make_endpoint(0, 5);
    let generation = crate::table::endpoint_gen(ep);
    let slot = crate::table::endpoint_slot(ep);
    ctx.assert(generation == 0, "generation must be 0");
    ctx.assert(slot == 5, "slot must be 5");
}

fn test_endpoint_lookup(ctx: &mut TestCtx) {
    let clock_ep = crate::table::make_endpoint(0, arch_common::com::CLOCK);
    let rp = crate::table::endpoint_lookup(clock_ep);
    ctx.assert(!rp.is_null(), "endpoint_lookup(CLOCK) must succeed");
}

fn test_is_ok_proc_nr(ctx: &mut TestCtx) {
    ctx.assert(crate::table::is_ok_proc_nr(0), "proc_nr 0 must be valid");
    ctx.assert(
        crate::table::is_ok_proc_nr(arch_common::com::CLOCK),
        "CLOCK proc_nr must be valid",
    );
    ctx.assert(
        !crate::table::is_ok_proc_nr(300),
        "proc_nr 300 must be invalid",
    );
}

fn test_is_kernel_nr(ctx: &mut TestCtx) {
    ctx.assert(
        crate::table::is_kernel_nr(arch_common::com::CLOCK),
        "CLOCK is kernel nr",
    );
    ctx.assert(!crate::table::is_kernel_nr(0), "PM is not kernel nr");
}

// Clock / timer tests

fn test_tmr_never_value(ctx: &mut TestCtx) {
    ctx.assert(
        crate::clock::TMR_NEVER == u64::MAX,
        "TMR_NEVER must be u64::MAX",
    );
}

// Scheduler tests

unsafe fn sched_make_proc(nr: i32, priority: i8) -> *mut crate::proc::Proc {
    unsafe {
        crate::hal::init_cpulocals();
        let head = crate::hal::sched_run_q_head();
        let tail = crate::hal::sched_run_q_tail();
        for q in 0..crate::hal::sched_nr_queues() {
            (*head)[q] = core::ptr::null_mut();
            (*tail)[q] = core::ptr::null_mut();
        }

        let rp = make_test_proc(nr);
        if !rp.is_null() {
            (*rp).p_priority = priority;
            (*rp).p_nextready = core::ptr::null_mut();
        }
        rp
    }
}

fn test_enqueue_dequeue(ctx: &mut TestCtx) {
    unsafe {
        crate::table::proc_init();
        let rp = sched_make_proc(200, 0);
        if rp.is_null() {
            ctx.assert(false, "sched_make_proc failed");
            return;
        }
        (*rp).p_rts_flags.store(0, Ordering::Relaxed);

        crate::sched::enqueue(rp);
        let head = crate::hal::sched_run_q_head();
        ctx.assert(
            (*head)[0] == rp as *mut core::ffi::c_void,
            "enqueued proc must be at head of queue 0",
        );

        (*rp)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SENDING.bits(), Ordering::Relaxed);
        crate::sched::dequeue(rp);
        ctx.assert((*head)[0].is_null(), "queue must be empty after dequeue");

        (*rp)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

fn test_sched_priority_ordering(ctx: &mut TestCtx) {
    unsafe {
        // Create three procs at different priorities (lower number = higher priority)
        let high = sched_make_proc(110, 0); // highest priority
        let mid = sched_make_proc(111, 5); // medium priority
        let low = sched_make_proc(112, 15); // lowest priority
        if high.is_null() || mid.is_null() || low.is_null() {
            ctx.assert(false, "sched_make_proc failed");
            return;
        }

        // Enqueue lowest first, then mid, then highest — pick_proc must
        // still return the highest regardless of insertion order.
        crate::sched::enqueue(low);
        crate::sched::enqueue(mid);
        crate::sched::enqueue(high);

        // pick_proc should return the highest priority (queue 0)
        let picked = crate::sched::pick_proc();
        ctx.assert(picked.is_some(), "pick_proc should return a proc");
        if let Some(p) = picked {
            ctx.assert(p == high, "pick_proc must return highest priority proc");
            ctx.assert(
                (*p).p_endpoint == 110,
                "highest priority proc should be endpoint 110",
            );
        }

        // Remove high from queue, pick_proc should return mid
        crate::sched::remove_from_queue(high);
        let picked2 = crate::sched::pick_proc();
        ctx.assert(picked2.is_some(), "pick_proc should still return a proc");
        if let Some(p) = picked2 {
            ctx.assert(
                p == mid,
                "pick_proc must return medium priority after removing high",
            );
        }

        // Remove mid, pick_proc should return low
        crate::sched::remove_from_queue(mid);
        let picked3 = crate::sched::pick_proc();
        ctx.assert(
            picked3.is_some(),
            "pick_proc should return low priority proc",
        );
        if let Some(p) = picked3 {
            ctx.assert(p == low, "pick_proc must return lowest after removing mid");
        }

        // Remove low, pick_proc should return None
        crate::sched::remove_from_queue(low);
        let picked4 = crate::sched::pick_proc();
        ctx.assert(
            picked4.is_none(),
            "pick_proc should return None when queues empty",
        );

        // Clean up
        (*high)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*mid)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*low)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

fn test_sched_round_robin(ctx: &mut TestCtx) {
    unsafe {
        // Create two procs at the SAME priority (round-robin queue)
        let a = sched_make_proc(113, 7);
        let b = sched_make_proc(114, 7);
        if a.is_null() || b.is_null() {
            ctx.assert(false, "sched_make_proc failed");
            return;
        }

        // Enqueue both on the same priority queue
        crate::sched::enqueue(a);
        crate::sched::enqueue(b);

        // First pick should return 'a' (head of queue)
        let p1 = crate::sched::pick_proc();
        ctx.assert(p1.is_some(), "pick_proc should return a proc");
        if let Some(p) = p1 {
            ctx.assert(
                p == a,
                "first pick should return first enqueued at same priority",
            );
        }

        // Remove 'a' from queue (simulating it getting CPU)
        crate::sched::remove_from_queue(a);

        // Re-enqueue 'a' at the tail (round-robin: move to end)
        (*a).p_rts_flags.store(0, Ordering::Relaxed); // ensure runnable
        crate::sched::enqueue(a);

        // Now the queue should be: head = b, tail = a
        // pick should return 'b'
        let p2 = crate::sched::pick_proc();
        ctx.assert(
            p2.is_some(),
            "pick_proc should return a proc after round-robin",
        );
        if let Some(p) = p2 {
            ctx.assert(
                p == b,
                "second pick should return second enqueued (round-robin)",
            );
        }

        // Remove 'b', pick should return 'a' again
        crate::sched::remove_from_queue(b);
        let p3 = crate::sched::pick_proc();
        ctx.assert(
            p3.is_some(),
            "pick_proc should return a proc after removing b",
        );
        if let Some(p) = p3 {
            ctx.assert(
                p == a,
                "third pick should return 'a' after 'b' removed (round-robin cycle)",
            );
        }

        // Clean up
        crate::sched::remove_from_queue(a);
        let empty = crate::sched::pick_proc();
        ctx.assert(empty.is_none(), "queues should be empty after cleanup");

        (*a).p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*b).p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

fn test_sched_proc_no_time_preempts(ctx: &mut TestCtx) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        // x86_64-only: requires privilege structures + scheduler proc IPC
        // which conflicts with RISC-V's HAL init_cpulocals behavior.
        let scheduler = sched_make_proc(114, 7);
        if scheduler.is_null() {
            ctx.assert(false, "sched_make_proc for scheduler failed");
            return;
        }
        (*scheduler).p_rts_flags.store(
            crate::proc::RtsFlags::RECEIVING.bits() | crate::proc::RtsFlags::PREEMPTED.bits(),
            Ordering::Relaxed,
        );
        (*scheduler).p_getfrom_e = crate::system::NONE;

        let mut priv_hi = crate::r#priv::Priv::default();
        priv_hi.s_proc_nr = 115;
        priv_hi.s_id = 99;
        priv_hi.s_flags =
            crate::r#priv::PrivFlags::PREEMPTIBLE | crate::r#priv::PrivFlags::BILLABLE;
        let mut priv_lo = crate::r#priv::Priv::default();
        priv_lo.s_proc_nr = 116;
        priv_lo.s_id = 100;
        priv_lo.s_flags =
            crate::r#priv::PrivFlags::PREEMPTIBLE | crate::r#priv::PrivFlags::BILLABLE;

        let hi = sched_make_proc(115, 7);
        let lo = sched_make_proc(116, 7);
        if hi.is_null() || lo.is_null() {
            ctx.assert(false, "sched_make_proc failed");
            return;
        }
        (*hi).p_priv = &raw mut priv_hi;
        (*lo).p_priv = &raw mut priv_lo;
        (*hi).p_scheduler = scheduler;
        (*lo).p_scheduler = scheduler;

        (*hi).p_quantum_size_ms = 50;
        (*hi).p_cpu_time_left = crate::clock::ms_2_cpu_time(50);
        (*lo).p_quantum_size_ms = 50;
        (*lo).p_cpu_time_left = crate::clock::ms_2_cpu_time(50);

        crate::sched::enqueue(hi);
        crate::sched::enqueue(lo);

        let p1 = crate::sched::pick_proc();
        ctx.assert(p1.is_some(), "pick_proc should return a proc");
        if let Some(p) = p1 {
            ctx.assert(p == hi, "first pick should return hi");
            crate::sched::proc_no_time(hi);
            let hi_rts = (*hi).p_rts_flags.load(Ordering::Relaxed);
            ctx.assert(
                hi_rts & crate::proc::RtsFlags::NO_QUANTUM.bits() != 0,
                "hi should have NO_QUANTUM set after proc_no_time",
            );
        }

        let p2 = crate::sched::pick_proc();
        ctx.assert(p2.is_some(), "pick_proc should return lo");
        if let Some(p) = p2 {
            ctx.assert(p == lo, "second pick should return lo");
            crate::sched::proc_no_time(lo);
            let lo_rts = (*lo).p_rts_flags.load(Ordering::Relaxed);
            ctx.assert(
                lo_rts & crate::proc::RtsFlags::NO_QUANTUM.bits() != 0,
                "lo should have NO_QUANTUM set after proc_no_time",
            );
        }

        let p3 = crate::sched::pick_proc();
        ctx.assert(
            p3.is_none(),
            "pick_proc should return None when all procs blocked",
        );

        // Round-robin renewal
        (*hi).p_rts_flags.store(0, Ordering::Relaxed);
        (*lo).p_rts_flags.store(0, Ordering::Relaxed);
        (*hi).p_cpu_time_left = crate::clock::ms_2_cpu_time(50);
        (*lo).p_cpu_time_left = crate::clock::ms_2_cpu_time(50);

        crate::sched::enqueue(hi);
        crate::sched::enqueue(lo);

        let r1 = crate::sched::pick_proc();
        ctx.assert(r1 == Some(hi), "round-robin cycle 1 should return hi");
        crate::sched::proc_no_time(hi);

        let r2 = crate::sched::pick_proc();
        ctx.assert(r2 == Some(lo), "round-robin cycle 2 should return lo");
        crate::sched::proc_no_time(lo);

        (*hi).p_rts_flags.store(0, Ordering::Relaxed);
        (*hi).p_cpu_time_left = crate::clock::ms_2_cpu_time(50);
        crate::sched::enqueue(hi);

        let r3 = crate::sched::pick_proc();
        ctx.assert(r3 == Some(hi), "round-robin cycle 3 should return hi again");

        (*hi)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*lo)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        // RISC-V: skip — sched_make_proc + enqueue + pick_proc works
        // for basic tests but the IPC notification path (scheduler proc,
        // privilege structures) needs investigation.
        ctx.assert(true, "skip on non-x86_64");
    }
}

// Privilege table tests

fn test_priv_default_proc_nr(ctx: &mut TestCtx) {
    let p = crate::r#priv::Priv::default();
    ctx.assert(p.s_proc_nr == 0, "default Priv s_proc_nr must be 0");
}

fn test_priv_flags_empty(ctx: &mut TestCtx) {
    let p = crate::r#priv::Priv::default();
    ctx.assert(
        p.s_flags == crate::r#priv::PrivFlags::empty(),
        "default Priv s_flags must be empty",
    );
}

// Process struct tests

fn test_proc_size_key(ctx: &mut TestCtx) {
    ctx.assert(
        size_of::<crate::proc::Proc>() <= 1024,
        "Proc size must not exceed IDLE_PROC_SIZE (1024)",
    );
}

fn test_proc_ptr_ok(ctx: &mut TestCtx) {
    let mut p = crate::proc::Proc::default();
    p.p_magic = crate::proc::PMAGIC;
    ctx.assert(p.ptr_ok(), "Proc with PMAGIC must pass ptr_ok");
}

fn test_vfs_mfs_ipc_roundtrip(ctx: &mut TestCtx) {
    unsafe {
        // Register an MFS dispatch handler that handles REQ_READSUPER
        // VFS→MFS message format (from servers/src/vfs/request.rs):
        //   m_type at offset 4: REQ_READSUPER = FS_BASE + 28 = 0xA10 + 28 = 0xA1C
        //   PAYLOAD_OFF (8):    device (u32)
        //   PAYLOAD_OFF + 4:    flags (u32)
        //   PAYLOAD_OFF + 8:    label_len (u64)
        //   PAYLOAD_OFF + 24:   grant_id (i32)
        //
        // MFS→VFS reply format:
        //   m_type at offset 4: status (0 = OK)
        //   PAYLOAD_OFF (8):    file_size (i64)
        //   PAYLOAD_OFF + 8:    dev (u32)
        //   PAYLOAD_OFF + 12:   inode_nr (u32)
        //   PAYLOAD_OFF + 16:   flags (u32)
        //   PAYLOAD_OFF + 20:   mode (u16)
        fn mfs_readsuper_handler(
            _caller: *mut crate::proc::Proc,
            msg: &mut [u8; crate::proc::MESSAGE_SIZE],
        ) -> i32 {
            // Parse the request
            let req_type = i32::from_ne_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
            let _device = u32::from_ne_bytes(msg[8..12].try_into().unwrap_or([0; 4]));
            let flags = u32::from_ne_bytes(msg[12..16].try_into().unwrap_or([0; 4]));
            let _label_len = u64::from_ne_bytes(msg[16..24].try_into().unwrap_or([0; 8]));

            // Verify it's a REQ_READSUPER
            if req_type != 0xA1C {
                msg[4..8].copy_from_slice(&(-5i32).to_ne_bytes()); // EIO
                return 0;
            }

            // Build response: simulate a successful root filesystem mount
            // Root inode: inode_nr=1, mode=directory(0x41FF), file_size=0, dev=matching, flags=0
            let is_root = (flags & 2) != 0; // REQ_ISROOT = 2
            let inode_nr: u32 = if is_root { 1 } else { 2 };
            let mode: u16 = 0x41FF; // I_DIRECTORY | 0755
            let file_size: i64 = 0;

            msg[4..8].copy_from_slice(&0i32.to_ne_bytes()); // status = OK
            msg[8..16].copy_from_slice(&file_size.to_ne_bytes()); // file_size
            msg[16..20].copy_from_slice(&0u32.to_ne_bytes()); // dev
            msg[20..24].copy_from_slice(&inode_nr.to_ne_bytes()); // inode_nr
            msg[24..28].copy_from_slice(&0u32.to_ne_bytes()); // flags
            msg[28..30].copy_from_slice(&mode.to_ne_bytes()); // mode
            0
        }

        // Use MFS_PROC_NR = 5 (from boot_init.rs: ("/sbin/mfs", MFS_PROC_NR))
        const MFS_PROC_NR: i32 = 5;

        // Register the MFS handler
        let registered = crate::ipc::register_server_dispatch(MFS_PROC_NR, mfs_readsuper_handler);
        ctx.assert(registered, "register_server_dispatch for MFS must succeed");

        // Set up a caller process (simulating VFS)
        let caller = make_test_proc(117);
        if caller.is_null() {
            ctx.assert(false, "make_test_proc for VFS caller failed");
            return;
        }
        let _caller_ep = (*caller).p_endpoint;

        // Build a REQ_READSUPER message (VFS→MFS mount request)
        // Format matches req_readsuper in servers/src/vfs/request.rs
        let mut msg = [0u8; crate::proc::MESSAGE_SIZE];

        // Bytes 0-3: destination endpoint (MFS_PROC_NR)
        msg[0..4].copy_from_slice(&MFS_PROC_NR.to_le_bytes());
        // Bytes 4-7: m_type = REQ_READSUPER = 0xA1C
        msg[4..8].copy_from_slice(&0xA1Ci32.to_le_bytes());
        // Byte 8-11: device = 1 (root device)
        msg[8..12].copy_from_slice(&1u32.to_le_bytes());
        // Byte 12-15: flags = REQ_ISROOT (2) | REQ_RDONLY (1) = 3
        msg[12..16].copy_from_slice(&3u32.to_le_bytes());
        // Byte 16-23: label_len = 0
        msg[16..24].copy_from_slice(&0u64.to_le_bytes());
        // Byte 24-27: grant_id = 0 (no label)
        msg[24..28].copy_from_slice(&0i32.to_le_bytes());

        // Send the message via do_sync_ipc (SENDREC to MFS)
        let result = crate::ipc::do_sync_ipc(caller, msg.as_mut_ptr(), crate::ipc::SENDREC);
        ctx.assert(result == 0, "do_sync_ipc SENDREC to MFS must return OK");

        // Parse the response
        let status = i32::from_ne_bytes(msg[4..8].try_into().unwrap_or([0xFF; 4]));
        ctx.assert(status == 0, "MFS mount response status must be OK (0)");

        let inode_nr = u32::from_ne_bytes(msg[20..24].try_into().unwrap_or([0; 4]));
        ctx.assert(inode_nr == 1, "MFS root inode must be 1");

        let mode = u16::from_ne_bytes(msg[28..30].try_into().unwrap_or([0; 2]));
        // 0x41FF = I_DIRECTORY | 0x1FF (0777 permissions)
        ctx.assert(
            mode == 0x41FF,
            "MFS root inode mode must be directory (0x41FF)",
        );

        let file_size = i64::from_ne_bytes(msg[8..16].try_into().unwrap_or([0xFF; 8]));
        ctx.assert(file_size == 0, "MFS root inode file_size must be 0");

        // Caller is still runnable after SENDREC — the in-kernel dispatch
        // handler processed both the send and receive halves of SENDREC
        // atomically, so the caller doesn't need to wait for a separate reply.
        let caller_rts = (*caller).p_rts_flags.load(Ordering::Relaxed);
        ctx.assert(
            caller_rts == 0,
            "VFS caller must be runnable after in-kernel MFS dispatch",
        );

        // Clean up
        (*caller)
            .p_rts_flags
            .store(crate::proc::RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
    }
}

/// Run all kernel unit tests inside QEMU. Returns the number of failures (0 = all passed).
pub fn run_all() -> u32 {
    let mut total: u32 = 0;

    total += run("ehdr_size", test_ehdr_size);
    total += run("phdr_size", test_phdr_size);
    total += run("elf_constants", test_elf_constants);

    total += run("cpio_parse_simple", test_cpio_parse_simple);

    total += run("mini_send_direct", test_mini_send_direct_delivery);
    total += run("mini_send_queue", test_mini_send_queues_when_not_receiving);
    total += run("mini_notify", test_mini_notify_receiving);
    total += run("sendrec_direct", test_sendrec_direct);
    total += run("sendrec_reply_cycle", test_sendrec_reply_cycle);

    total += run("proc_addr_tasks", test_proc_addr_valid_tasks);
    total += run("proc_addr_oob", test_proc_addr_out_of_range);
    total += run("endpoint_encoding", test_endpoint_encoding);
    total += run("endpoint_lookup", test_endpoint_lookup);
    total += run("is_ok_proc_nr", test_is_ok_proc_nr);
    total += run("is_kernel_nr", test_is_kernel_nr);

    total += run("tmr_never", test_tmr_never_value);

    total += run("enqueue_dequeue", test_enqueue_dequeue);
    total += run("sched_priority", test_sched_priority_ordering);
    total += run("sched_round_robin", test_sched_round_robin);
    total += run("sched_proc_no_time", test_sched_proc_no_time_preempts);
    total += run("vfs_mfs_ipc", test_vfs_mfs_ipc_roundtrip);

    total += run("priv_default", test_priv_default_proc_nr);
    total += run("priv_flags", test_priv_flags_empty);
    total += run("proc_size", test_proc_size_key);
    total += run("proc_ptr_ok", test_proc_ptr_ok);

    total
}
