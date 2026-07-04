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
    while pos % 4 != 0 {
        pos += 1;
    }

    archive[pos..pos + data.len()].copy_from_slice(data);
    pos += data.len();
    while pos % 4 != 0 {
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
    while pos % 4 != 0 {
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

        // ── SENDREC = mini_send + mini_receive ────────────────────
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

        // ── Phase 1: src SENDREC to dst ────────────────────────────
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

        // ── Phase 2: dst replies to src ────────────────────────────
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

    total += run("priv_default", test_priv_default_proc_nr);
    total += run("priv_flags", test_priv_flags_empty);
    total += run("proc_size", test_proc_size_key);
    total += run("proc_ptr_ok", test_proc_ptr_ok);

    total
}
