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
    #[cfg(target_arch = "x86_64")]
    unsafe {
        let port: u16 = 0x3F8;
        loop {
            let lsr: u8;
            core::arch::asm!("in al, dx", out("al") lsr, in("dx") port + 5, options(nomem, nostack));
            if lsr & 0x20 != 0 {
                break;
            }
        }
        core::arch::asm!("out dx, al", in("dx") port, in("al") c, options(nomem, nostack));
    }
    #[cfg(not(target_arch = "x86_64"))]
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
        let head = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.run_q_head_ptr();
        let tail = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.run_q_tail_ptr();
        for q in 0..arch_x86_64::cpulocals::NR_SCHED_QUEUES {
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
        let head = arch_x86_64::cpulocals::CPU_LOCAL_STORAGE.run_q_head_ptr();
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

    total += run("proc_addr_tasks", test_proc_addr_valid_tasks);
    total += run("proc_addr_oob", test_proc_addr_out_of_range);
    total += run("endpoint_encoding", test_endpoint_encoding);
    total += run("endpoint_lookup", test_endpoint_lookup);
    total += run("is_ok_proc_nr", test_is_ok_proc_nr);
    total += run("is_kernel_nr", test_is_kernel_nr);

    total += run("tmr_never", test_tmr_never_value);

    total += run("enqueue_dequeue", test_enqueue_dequeue);

    total += run("priv_default", test_priv_default_proc_nr);
    total += run("priv_flags", test_priv_flags_empty);
    total += run("proc_size", test_proc_size_key);
    total += run("proc_ptr_ok", test_proc_ptr_ok);

    total
}
