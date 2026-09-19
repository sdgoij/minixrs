//! Runs the arch-independent kernel on the host, under `arch-sim`.
//!
//! The point is coverage that previously required QEMU. The process table, the
//! scheduler's run queues, and the VM's physical page allocator are all
//! ISA-independent, and until now the only way to execute them was to boot a
//! full image in an emulator.
//!
//! These drive real kernel entry points rather than testing the HAL in
//! isolation, in the same spirit as `crates/kernel-tests` — just on a host HAL
//! instead of bare metal.
//!
//! Run with:
//!
//! ```text
//! cargo run --manifest-path crates/kernel-sim-tests/Cargo.toml
//! ```

use core::ptr::null_mut;
use core::sync::atomic::Ordering;

use kernel::proc::RtsFlags;

struct Checks {
    pass: usize,
    fail: usize,
}

impl Checks {
    fn check(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            self.pass += 1;
            println!("PASS  {name}");
        } else {
            self.fail += 1;
            println!("FAIL  {name}");
            if !detail.is_empty() {
                println!("        {detail}");
            }
        }
    }

    /// Record an observation that is not pass/fail. Findings about existing
    /// behaviour belong here rather than as failures: this harness reports,
    /// it does not decide what the kernel ought to do.
    fn note(&mut self, name: &str, detail: String) {
        println!("NOTE  {name}");
        if !detail.is_empty() {
            println!("        {detail}");
        }
    }
}

fn main() -> std::process::ExitCode {
    let mut checks = Checks { pass: 0, fail: 0 };

    // `kernel::init` calls `hal::init`, which resets the simulator, so a run
    // cannot inherit console output, clock, or pages from a previous one.
    kernel::init();

    frame_layout(&mut checks);
    console_round_trip(&mut checks);
    physical_arena(&mut checks);
    process_table(&mut checks);
    scheduler(&mut checks);
    ipc_rendezvous(&mut checks);
    vm_pool(&mut checks);

    println!(
        "\n{}/{} checks passed",
        checks.pass,
        checks.pass + checks.fail
    );
    if checks.fail == 0 {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

fn frame_layout(checks: &mut Checks) {
    let mut frame = arch_sim::hal::frame_default();
    unsafe {
        arch_sim::hal::write_frame_field(&mut frame, arch_sim::frame::RIP, 0xDEAD_BEEF);
        arch_sim::hal::write_frame_field(&mut frame, arch_sim::frame::RSP, 0x1234_5678);
        arch_sim::hal::write_retval(&mut frame, 42);
    }
    let ip = unsafe { arch_sim::hal::read_frame_ip(&frame) };
    let sp = unsafe { arch_sim::hal::read_frame_sp(&frame) };
    let retval = unsafe { arch_sim::hal::read_syscall_nr(&frame) };
    checks.check(
        "frame fields round-trip through the HAL",
        ip == 0xDEAD_BEEF && sp == 0x1234_5678 && retval == 42,
        format!("ip={ip:#x} sp={sp:#x} retval={retval}"),
    );

    // `debug.rs` reads these two offsets directly rather than through the HAL,
    // so the layout has to agree with them or kernel crashes print garbage.
    let raw_ip = u64::from_ne_bytes(frame[160..168].try_into().unwrap());
    let raw_sp = u64::from_ne_bytes(frame[168..176].try_into().unwrap());
    checks.check(
        "frame layout matches the offsets the kernel reads directly",
        raw_ip == 0xDEAD_BEEF && raw_sp == 0x1234_5678,
        format!("offset 160 = {raw_ip:#x}, offset 168 = {raw_sp:#x}"),
    );
}

fn console_round_trip(checks: &mut Checks) {
    for byte in b"hello kernel" {
        arch_sim::hal::serial_write_byte(*byte);
    }
    let mut buf = [0u8; 64];
    let n = arch_sim::console_drain(&mut buf);
    checks.check(
        "kernel console output reaches the host",
        &buf[..n] == b"hello kernel",
        format!("drained {:?}", String::from_utf8_lossy(&buf[..n])),
    );

    arch_sim::console_push_input_str("x");
    checks.check(
        "host input is visible to the kernel",
        arch_sim::hal::poll_console() == Some(b'x'),
        "poll_console did not return the queued byte".to_string(),
    );
}

fn physical_arena(checks: &mut Checks) {
    let baseline = arch_sim::phys_free_pages();

    let page = unsafe { arch_sim::hal::alloc_phys_page() };
    let contig = unsafe { arch_sim::hal::alloc_phys_contig(8) };
    let after = arch_sim::phys_free_pages();
    checks.check(
        "physical allocations are accounted for",
        page.is_some() && contig.is_some() && baseline - after == 9,
        format!("baseline={baseline} after={after} (expected 9 pages taken)"),
    );

    if let Some(p) = page {
        unsafe { arch_sim::hal::free_phys_contig(p, 1) };
    }
    if let Some(c) = contig {
        unsafe { arch_sim::hal::free_phys_contig(c, 8) };
    }
    checks.check(
        "freeing returns the arena to its baseline",
        arch_sim::phys_free_pages() == baseline,
        format!("baseline={baseline} now={}", arch_sim::phys_free_pages()),
    );

    // An impossible request must fail rather than wrap around and hand back a
    // region that overlaps live pages.
    let too_big = unsafe { arch_sim::hal::alloc_phys_contig(usize::MAX / 4096) };
    checks.check(
        "an oversized contiguous request fails cleanly",
        too_big.is_none(),
        format!("got {too_big:?}"),
    );
}

fn process_table(checks: &mut Checks) {
    unsafe { kernel::table::proc_init() };

    // `proc_addr(0)` is the first *user* slot (the table is indexed from
    // `NR_TASKS`), so it equals `beg_user_addr()`, not the table base.
    let first_user = kernel::table::proc_addr(0);
    let user_base = kernel::table::beg_user_addr();
    let table_base = kernel::table::proc_table_base();
    checks.check(
        "process table initialises with the kernel/user split intact",
        !first_user.is_null()
            && first_user == user_base
            && table_base == kernel::table::beg_proc_addr()
            && first_user != table_base,
        format!(
            "proc_addr(0)={first_user:p} beg_user_addr()={user_base:p} table_base={table_base:p}"
        ),
    );

    checks.check(
        "process slot numbering is bounded",
        kernel::table::is_ok_proc_nr(0)
            && !kernel::table::is_ok_proc_nr(100_000)
            && kernel::table::is_ok_proc_nr(-1),
        "is_ok_proc_nr disagreed about slot 0, 100000, or -1".to_string(),
    );

    // Reproduced rather than read off the source: these helpers compute
    // `NR_TASKS as i32 + n` *before* validating it. Release builds get away with
    // it — the wrapped index casts to a huge `usize`, so the bound comparison
    // happens to be false — but a debug build panics inside the check itself,
    // which is the wrong failure mode for a function whose job is validating
    // input. Both are probed, since `proc_addr` does the same arithmetic.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let bad_slot = std::panic::catch_unwind(|| kernel::table::is_ok_proc_nr(i32::MAX)).is_err();
    let bad_addr = std::panic::catch_unwind(|| kernel::table::proc_addr(i32::MAX)).is_err();
    std::panic::set_hook(previous_hook);
    checks.note(
        "process-number helpers overflow on an out-of-range input",
        format!(
            "is_ok_proc_nr(i32::MAX) {}; proc_addr(i32::MAX) {}. Neither can be relied \
             on for a value the caller has not already clamped.",
            if bad_slot {
                "panicked"
            } else {
                "returned normally"
            },
            if bad_addr {
                "panicked"
            } else {
                "returned normally"
            }
        ),
    );
}

fn scheduler(checks: &mut Checks) {
    unsafe {
        let high = kernel::table::proc_addr(2);
        let low = kernel::table::proc_addr(3);
        if high.is_null() || low.is_null() {
            checks.check(
                "scheduler test can reach two process slots",
                false,
                format!("proc_addr(2)={high:p} proc_addr(3)={low:p}"),
            );
            return;
        }

        // MINIX priority: lower number wins. `pick_proc` scans queues upward.
        (*high).p_priority = 5;
        (*high).p_nextready = null_mut();
        (*high).p_q_link = null_mut();
        (*low).p_priority = 9;
        (*low).p_nextready = null_mut();
        (*low).p_q_link = null_mut();

        kernel::sched::enqueue(low);
        kernel::sched::enqueue(high);
        checks.check(
            "run queues stay consistent across enqueue",
            kernel::sched::runqueues_ok(),
            "runqueues_ok() reported an inconsistency after two enqueues".to_string(),
        );

        let picked = kernel::sched::pick_proc();
        checks.check(
            "the scheduler picks the higher-priority process",
            picked == Some(high),
            format!("picked {picked:?}, expected the priority-5 process {high:p}"),
        );

        // `dequeue` requires the process to have been marked non-runnable first
        // (it asserts `!is_runnable()`, i.e. `p_rts_flags != 0`), because in the
        // blocking path the flag is set before the queue is touched.
        // `remove_from_queue` is the unlink-anything variant, which is what
        // taking two still-runnable processes back out needs.
        kernel::sched::remove_from_queue(high);
        kernel::sched::remove_from_queue(low);
        checks.check(
            "run queues stay consistent across removal",
            kernel::sched::runqueues_ok(),
            "runqueues_ok() reported an inconsistency after removing both".to_string(),
        );

        checks.check(
            "the run queue is empty once both processes are removed",
            kernel::sched::pick_proc().is_none(),
            format!(
                "pick_proc() still returned {:?}",
                kernel::sched::pick_proc()
            ),
        );
    }
}

/// Prepare a process slot for IPC, mirroring `kernel/src/tests.rs`'s helper.
///
/// # Safety
///
/// `nr` must name a slot in the process table, and `proc_init` must have run.
unsafe fn make_test_proc(nr: i32) -> *mut kernel::proc::Proc {
    let rp = kernel::table::proc_addr(nr);
    if rp.is_null() {
        return rp;
    }
    // SAFETY: the caller guarantees `rp` is a valid table slot.
    unsafe {
        (*rp).p_rts_flags.store(0, Ordering::Relaxed);
        (*rp).p_nr = nr;
        (*rp).p_endpoint = kernel::table::make_endpoint(0, nr);
        (*rp).p_caller_q = null_mut();
        (*rp).p_q_link = null_mut();
        (*rp).p_getfrom_e = 0;
        (*rp).p_sendto_e = 0;
        (*rp).p_magic = kernel::proc::PMAGIC;
    }
    rp
}

fn ipc_rendezvous(checks: &mut Checks) {
    unsafe {
        let src = make_test_proc(100);
        let dst = make_test_proc(101);
        if src.is_null() || dst.is_null() {
            checks.check(
                "IPC test can reach two process slots",
                false,
                format!("proc_addr(100)={src:p} proc_addr(101)={dst:p}"),
            );
            return;
        }
        let src_ep = (*src).p_endpoint;
        let dst_ep = (*dst).p_endpoint;

        // A receiver already waiting should take the message directly; this is
        // the rendezvous the whole IPC design rests on.
        (*dst)
            .p_rts_flags
            .store(RtsFlags::RECEIVING.bits(), Ordering::Relaxed);
        (*dst).p_getfrom_e = src_ep;
        let mut msg = [0u8; 64];
        msg[4..8].copy_from_slice(&42i32.to_ne_bytes());
        let direct = kernel::ipc::mini_send(src, dst_ep, msg.as_ptr(), 0);
        checks.check(
            "a send to a waiting receiver succeeds",
            direct == 0,
            format!("mini_send returned {direct}"),
        );
        let dst_flags = (*dst).p_rts_flags.load(Ordering::Relaxed);
        checks.check(
            "direct delivery unblocks the receiver",
            dst_flags & RtsFlags::RECEIVING.bits() == 0,
            format!("receiver flags = {dst_flags:#x}, RECEIVING still set"),
        );
        let payload = i32::from_ne_bytes((&(*dst).p_delivermsg)[0..4].try_into().unwrap());
        checks.check(
            "the receiver's delivery slot records the source",
            payload == src_ep,
            format!("delivery slot holds {payload}, expected the source endpoint {src_ep}"),
        );

        // With nobody waiting, the sender is the one that blocks.
        let mut msg2 = [0u8; 64];
        msg2[4..8].copy_from_slice(&7i32.to_ne_bytes());
        let queued = kernel::ipc::mini_send(src, dst_ep, msg2.as_ptr(), 0);
        let src_flags = (*src).p_rts_flags.load(Ordering::Relaxed);
        checks.check(
            "a send with no waiting receiver blocks the sender",
            queued == 0 && src_flags & RtsFlags::SENDING.bits() != 0,
            format!("mini_send={queued} sender flags={src_flags:#x}"),
        );
        checks.check(
            "run queues stay consistent across a blocking send",
            kernel::sched::runqueues_ok(),
            "runqueues_ok() reported an inconsistency after a blocking send".to_string(),
        );

        // A matching receive should pick up the queued sender and complete the
        // rendezvous. Receiving from ANY returns the *source endpoint*, which is
        // why this is not compared against 0.
        let mut rx = [0u8; 64];
        let got = kernel::ipc::mini_receive(dst, src_ep, rx.as_mut_ptr(), 0);
        let src_after = (*src).p_rts_flags.load(Ordering::Relaxed);
        checks.check(
            "a matching receive completes the queued send",
            got == src_ep && src_after & RtsFlags::SENDING.bits() == 0,
            format!("mini_receive={got} (expected source {src_ep}) sender flags {src_flags:#x} -> {src_after:#x}"),
        );

        // Leave the slots as free as they were found, so later checks cannot end
        // up reading a process this test invented.
        (*src)
            .p_rts_flags
            .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);
        (*dst)
            .p_rts_flags
            .store(RtsFlags::SLOT_FREE.bits(), Ordering::Relaxed);

        // The rendezvous bookkeeping above is fully exercised, but the message
        // *payload* cannot be: `mini_send` copies it out of the sender's address
        // space through `copy_from_user`, which needs real address translation.
        // In the simulator that copy fails, and the kernel discards the result
        // (`let _ = crate::ipc::copy_from_user(..)`), so the receiver's buffer
        // stays zeroed with no error surfaced anywhere. Worth knowing before
        // trusting a host-side IPC test to prove payload integrity.
        checks.note(
            "message payloads do not cross the simulated address space",
            "mini_send's copy_from_user is a no-op here, and its error is discarded, \
             so a receiver's buffer stays zeroed without any failure being reported. \
             Payload integrity has to be verified on a real arch or once the \
             simulator models address translation."
                .to_string(),
        );
    }
}

fn vm_pool(checks: &mut Checks) {
    const POOL: usize = 64;
    let chunks = [kernel::vm::MemoryChunk {
        base: 0x1000,
        size: POOL as u64,
    }];
    unsafe { kernel::vm::mem_init(&chunks) };

    let total = kernel::vm::total_pages();
    let (_, free0, largest0) = kernel::vm::mem_stats();
    checks.check(
        "the VM pool initialises from its memory chunk",
        total == POOL as i32 && free0 == POOL as i32 && largest0 == POOL as i32,
        format!("total={total} free={free0} largest_run={largest0}, expected {POOL} each"),
    );

    let alloc = unsafe { kernel::vm::alloc_mem(4, 0) };
    let (_, free1, _) = kernel::vm::mem_stats();
    checks.check(
        "allocation takes pages from the pool",
        alloc != kernel::vm::NO_MEM && free0 - free1 == 4,
        format!("alloc={alloc:#x} free {free0} -> {free1}"),
    );

    checks.check(
        "a zero-page allocation is rejected rather than accepted silently",
        unsafe { kernel::vm::alloc_mem(0, 0) } == kernel::vm::NO_MEM,
        "alloc_mem(0, ...) did not return NO_MEM".to_string(),
    );

    let over = unsafe { kernel::vm::alloc_mem(POOL * 2, 0) };
    checks.check(
        "an allocation larger than the pool fails",
        over == kernel::vm::NO_MEM,
        format!("alloc_mem({}, 0) returned {over:#x}", POOL * 2),
    );

    unsafe { kernel::vm::free_mem(alloc, 4) };
    let (_, free2, _) = kernel::vm::mem_stats();
    checks.check(
        "freeing returns the pages to the pool",
        free2 == free0,
        format!("free went {free0} -> {free1} -> {free2}"),
    );
}
