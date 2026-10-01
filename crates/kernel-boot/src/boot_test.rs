//! QEMU boot integration test — verifies boot sequence and server IPC.
//!
//! VFS calls SYS_BOOT_COMPLETE (syscall 60) after mount_root succeeds.
//! The kernel handler runs assertions, then exits QEMU via isa-debug-exit.
//!
//! Gated behind cfg(feature = "boot-test") — no impact on normal builds.

use arch_common::com::{
    DEVMAN_PROC_NR, DS_PROC_NR, MFS_PROC_NR, NET_PROC_NR, PFS_PROC_NR, PM_PROC_NR, RAMDISK_PROC_NR,
    RS_PROC_NR, SCHED_PROC_NR, TTY_PROC_NR, VFS_PROC_NR, VIRTIO_BLK_PROC_NR, VIRTIO_NET_PROC_NR,
    VM_PROC_NR,
};

const FS_BASE: i32 = 0xA00;
const REQ_READSUPER: i32 = FS_BASE + 28;
/// `REQ_LOOKUP` — what VFS asks for next once the root is mounted.
const REQ_LOOKUP: i32 = FS_BASE + 26;

/// The processes that get a per-process page table while booting (the `boot_procs` list in
/// `main.rs` / `riscv64.rs` / `aarch64.rs`). Shared by the tests that have to visit every one of
/// those address spaces.
const BOOTED_PROCS: &[(i32, &str)] = &[
    (DS_PROC_NR, "ds"),
    (RS_PROC_NR, "rs"),
    (PM_PROC_NR, "pm"),
    (SCHED_PROC_NR, "sched"),
    (VFS_PROC_NR, "vfs"),
    (VM_PROC_NR, "vm"),
    (RAMDISK_PROC_NR, "ramdisk"),
    (VIRTIO_BLK_PROC_NR, "virtio_blk"),
    (VIRTIO_NET_PROC_NR, "virtio_net"),
    (NET_PROC_NR, "net"),
    (MFS_PROC_NR, "mfs"),
    (TTY_PROC_NR, "tty"),
    (DEVMAN_PROC_NR, "devman"),
];

/// Run all boot tests, then exit QEMU with success/failure.
///
/// # Safety
///
/// Must be called from the SYS_BOOT_COMPLETE syscall handler after VFS
/// has finished mount_root and all boot processes are initialized.
/// Requires that the kernel-allocator, process table, IPC, and per-process
/// page tables are fully set up. The function never returns — it exits QEMU.
pub unsafe fn run_boot_tests() {
    serial_write("\r\n=== BOOT TEST ===\r\n");
    let mut failures: u32 = 0;

    // A: Server liveness
    failures += test_alive(VFS_PROC_NR, "VFS");
    failures += test_alive(MFS_PROC_NR, "MFS");
    failures += test_alive(PM_PROC_NR, "PM");
    failures += test_all_boot_procs_alive();

    // A2: Process table consistency (endpoint / magic / priv)
    failures += test_boot_procs_consistent();

    // B: Process state
    failures += test_vfs_runnable();
    failures += test_mfs_post_readsuper();
    failures += test_pm_idle();

    // C: VFS→MFS IPC (request in VFS's sendmsg)
    failures += test_vfs_sent_readsuper();

    // D: MFS→VFS IPC (reply in VFS's delivermsg). The last sendrec VFS
    // performs before boot-complete is mount_devman's lookup of the
    // /devices mount point, so the delivered message is that lookup reply.
    failures += test_vfs_reply_from_mfs();
    failures += test_vfs_reply_devices_lookup();
    failures += test_vfs_reply_devices_size();

    // E: Grant table registration
    failures += test_grant_registered();

    // F: VM page table walk (required for safe copy)
    failures += test_vm_check_range();

    // G: PM notification
    failures += test_pm_has_message();

    // H: Physical memory allocator — kernel binary excluded from free pool
    failures += test_allocator();

    // H2: Per-process mappings (brk heap, boot image in ramdisk server)
    failures += test_brk_heap_mapped();
    failures += test_mfs_ramdisk_mapped();

    // I: Process signal manager — s_sig_mgr must be PM_PROC_NR
    //    so do_getksig_handler can find exited processes.
    failures += test_boot_procs_have_sig_mgr();

    // J: Exec / initramfs verification.
    //
    // Run on the kernel's own page tables: the embedded archive reaches past the user VA base, so
    // from a process's CR3 a lookup above `0x1000000` reads that process's own text instead of the
    // archive (`KNOWN_ISSUES.md` item 38, where the mechanism is measured).
    failures += unsafe {
        on_kernel_tables(|| {
            test_initramfs_echo_exists()
                + test_initramfs_echo_elf()
                + test_initramfs_sh_exists()
                + test_initramfs_boot_files()
                + test_initramfs_pipetest_elf()
                + test_dynamic_artifacts_present()
        })
    };

    // K: PM MPROC page table walk
    failures += test_pm_mproc_pt();

    // L: Page table creation + map + walk roundtrip
    // Exercises map_page() with freshly allocated pages, catching
    // validation-bound regressions (e.g. RISC-V 0x1000_0000 limit).
    failures += test_map_page_walk_roundtrip();

    // M: Every boot process has a walkable page table
    failures += test_boot_procs_page_tables();

    // M1: No EL1-only kernel mapping over the user window (`PHYSMAP.md` P4, first half)
    #[cfg(target_arch = "aarch64")]
    {
        failures += test_no_kernel_mapping_in_user_window();
    }

    // M2: Every boot process's address space reaches the physmap
    failures += test_physmap_everywhere();
    // M3: A walk reaches its table through the physmap even when the identity map is shadowed
    failures += test_walk_through_the_physmap();
    // M4: A fill with a process argument lands in *that* process's memory
    failures += test_vm_memset_targets_the_named_process();

    // N: Mouse wiring — the IRQ-12 hook must notify the input server.
    failures += test_mouse_irq_notifies_input();

    // O: Physmap — a frame is the same memory seen at phys and phys_to_virt(phys)
    failures += test_physmap();

    if failures != 0 {
        serial_write("FAILURES: ");
        print_dec(failures);
        serial_write("\r\n");
        exit_qemu_failure(failures);
    }

    // Phase 1 is the deterministic post-mount state, and it stops short of the boundary the boot was
    // being trusted for. Phase 2 is that boundary: a user process has to exec a program out of the
    // image and then leave user mode again. INIT was held off the run queue until now, so phase 1 saw
    // exactly the fixed state it always has; releasing it lets the boot continue past the point this
    // suite used to exit at, and the watch armed here turns "it never ran" into a failure instead of
    // a summary printed before anyone had tried.
    serial_write("\r\n  phase 1 complete; releasing init\r\n");
    kernel::bootwatch::arm(
        kernel::clock::get_monotonic() + BOOT_WATCH_TICKS,
        boot_watch_verdict,
    );
    unsafe { crate::boot_init::release_init() };
}

/// Ticks the userspace phase is given before it counts as stopped. A boot reaches the shell in well
/// under a second of ticks; what this guards against is a hang, so the value only has to be larger
/// than a boot.
const BOOT_WATCH_TICKS: u64 = 500;

/// The verdict for the userspace phase, called from the timer interrupt by `kernel::bootwatch`.
///
/// Prints the outcome and exits QEMU, so it never returns — including on the failure path, where the
/// process table is dumped first. A stuck boot saying *who* is stuck is the whole point.
fn boot_watch_verdict(passed: bool) {
    unsafe {
        serial_write("\r\n=== BOOT TEST: userspace ===\r\n");
        if passed {
            serial_write("  OK a user process exec'd an image and ran\r\n");
            serial_write("ALL TESTS PASSED\r\n");
            exit_qemu_success();
        } else {
            serial_write("  FAIL: no user process ran after exec — the system stopped\r\n");
            kernel::syscall::sys_hang_dump_handler(core::ptr::null_mut(), &[0u64; 6]);
            serial_write("\r\nFAILURES: 1\r\n");
            exit_qemu_failure(1);
        }
    }
}

fn rdi(msg: *const u8, off: usize) -> i32 {
    unsafe { core::ptr::read_unaligned(msg.add(off) as *const i32) }
}
fn rdu(msg: *const u8, off: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(msg.add(off) as *const u32) }
}

// A: Liveness

/// The IRQ-12 hook (registered by the input server via SYS_IRQCTL) must
/// notify the input server when the mouse interrupt fires. Runs the hook
/// path directly (`irq_handle(12)`, what the mouse ISR calls) and checks
/// the notification reaches the input server's pending-interrupt state.
fn test_mouse_irq_notifies_input() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(arch_common::com::INPUT_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let privp = (*rp).p_priv;
        if privp.is_null() {
            return 1;
        }
        // Clear any stale pending bit for notify_id 2 (register_mouse_irq).
        (*privp).s_int_pending &= !(1u64 << 2);
        kernel::interrupt::irq_handle(12);
        if (*privp).s_int_pending & (1u64 << 2) == 0 {
            serial_write("  FAIL: mouse IRQ12 notification not pending\r\n");
            return 1;
        }
        serial_write("  OK mouse IRQ12 -> input notification\r\n");
    }
    0
}

fn test_alive(ep: i32, name: &str) -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(ep);
        if rp.is_null() || (*rp).p_endpoint != ep {
            serial_write("  FAIL: ");
            serial_write(name);
            serial_write(" dead\r\n");
            return 1;
        }
        serial_write("  OK ");
        serial_write(name);
        serial_write("\r\n");
    }
    0
}

// B: State

fn test_vfs_runnable() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let f = (*rp)
            .p_rts_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        if f & (kernel::proc::RtsFlags::SENDING.bits() | kernel::proc::RtsFlags::RECEIVING.bits())
            != 0
        {
            serial_write("  FAIL: VFS blocked\r\n");
            return 1;
        }
        serial_write("  OK VFS main loop\r\n");
    }
    0
}

fn test_mfs_post_readsuper() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(MFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let f = (*rp)
            .p_rts_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        if f & kernel::proc::RtsFlags::RECEIVING.bits() == 0 {
            // MFS might not be in RECEIVE if mount hasn't sent it a message.
            // Check if it's runnable instead.
            if f == 0 {
                serial_write("  OK MFS runnable (mount not started)\r\n");
                return 0;
            }
            serial_write("  FAIL: MFS unexpected flags=");
            print_dec(f);
            serial_write("\r\n");
            return 1;
        }
        serial_write("  OK MFS waiting\r\n");
    }
    0
}

fn test_pm_idle() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(PM_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let f = (*rp)
            .p_rts_flags
            .load(core::sync::atomic::Ordering::Relaxed);
        if f & kernel::proc::RtsFlags::RECEIVING.bits() == 0 {
            // PM might be runnable (flags=0) between notifications.
            // That's OK — it means it's processing and will receive again.
            if f == 0 {
                serial_write("  OK PM runnable\r\n");
                return 0;
            }
            serial_write("  FAIL: PM unexpected flags=");
            print_dec(f);
            serial_write("\r\n");
            return 1;
        }
        serial_write("  OK PM idle\r\n");
    }
    0
}

// C: Did VFS reach the file server?
// We can't read MFS's user buffer (it's in MFS's address space),
// but we CAN read VFS's p_sendmsg which held the outgoing request.
//
// `p_sendmsg` holds the *last* message VFS sent, so this can only be sampled before
// VFS sends anything else — and what it sends next depends on how far the boot had
// got, not on the protocol. The readsuper reply stopped being the last message long
// ago (mount_devman's /devices lookup is; check D below records that), which is this
// same race hit once already. So the assertion here is the one that does not depend
// on the moment: VFS reached the file server, which is either the readsuper itself
// or a request that can only follow a successful `mount_root`. That the readsuper
// *succeeded* is pinned by check D, which reads MFS's reply.

fn test_vfs_sent_readsuper() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let ty = rdi((*rp).p_sendmsg.as_ptr(), 4);
        if ty == 0 {
            serial_write("  SKIP: mount not started\r\n");
            return 0;
        }
        if ty != REQ_READSUPER && ty != REQ_LOOKUP {
            serial_write("  FAIL: VFS send type=");
            print_dec(ty as u32);
            serial_write(" expected 2588 (readsuper) or a later request\r\n");
            return 1;
        }
        if ty == REQ_READSUPER {
            serial_write("  OK VFS sent REQ_READSUPER to MFS\r\n");
        } else {
            serial_write("  OK VFS reached MFS (readsuper sent; now past it)\r\n");
        }
    }
    0
}

// D: Did VFS receive a reply from MFS?

fn test_vfs_reply_from_mfs() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let msg = (*rp).p_delivermsg.as_ptr();
        let src = rdi(msg, 0); // m_source
        if src != MFS_PROC_NR {
            serial_write("  SKIP: no MFS reply (mount not ready)\r\n");
            return 0;
        }
        let st = rdi(msg, 4); // m_type (status)
        if st != 0 {
            serial_write("  FAIL: reply status=");
            print_dec(st as u32);
            serial_write(" expected 0\r\n");
            return 1;
        }
        serial_write("  OK VFS reply from MFS status=OK\r\n");
    }
    0
}

// D: Did VFS resolve the /devices mount point in MFS? (The readsuper
// reply is no longer the last message in VFS's delivermsg — mount_devman's
// lookup of /devices is — so this pins that lookup instead.)

fn test_vfs_reply_devices_lookup() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let msg = (*rp).p_delivermsg.as_ptr();
        let src = rdi(msg, 0);
        if src != MFS_PROC_NR {
            serial_write("  SKIP: no MFS reply\r\n");
            return 0;
        }
        // REQ_LOOKUP reply: inode @ 28, dev @ 24 (req_lookup parsing).
        let ino = rdu(msg, 28);
        let dev = rdu(msg, 24);
        if ino != 7 {
            serial_write("  FAIL: devices inode=");
            print_dec(ino);
            serial_write(" expected 7\r\n");
            return 1;
        }
        serial_write("  OK devices dir inode=7 dev=");
        print_dec(dev);
        serial_write("\r\n");
    }
    0
}

fn test_vfs_reply_devices_size() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let msg = (*rp).p_delivermsg.as_ptr();
        let src = rdi(msg, 0);
        if src != MFS_PROC_NR {
            serial_write("  SKIP: no MFS reply\r\n");
            return 0;
        }
        // REQ_LOOKUP reply: file_size (i64) @ 16.
        let low = rdu(msg, 16);
        let high = rdu(msg, 20);
        let size = (low as u64) | ((high as u64) << 32);
        if size == 0 {
            serial_write("  FAIL: devices dir size=0 (empty?)\r\n");
            return 1;
        }
        serial_write("  OK devices dir size=");
        print_dec(high);
        serial_write(",");
        print_dec(low);
        serial_write("\r\n");
    }
    0
}

// E: Grant table registration

fn test_grant_registered() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let p = (*rp).p_priv;
        if p.is_null() {
            serial_write("  FAIL: VFS no priv\r\n");
            return 1;
        }
        let gt = (*p).s_grant_table;
        let ge = (*p).s_grant_entries;
        if gt == 0 || ge <= 0 {
            serial_write("  FAIL: grant table not registered\r\n");
            return 1;
        }
        serial_write("  OK grant table registered\r\n");
    }
    0
}

fn test_vm_check_range() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(MFS_PROC_NR);
        if rp.is_null() {
            serial_write("  VM: no MFS\r\n");
            return 1;
        }
        let cr3 = (*rp).p_seg.p_cr3;
        if cr3 == 0 {
            serial_write("  VM: no CR3\r\n");
            return 1;
        }
        // All userland binaries link at 0x01000000 (tools/minix-user.ld),
        // so the first code page is identical on every arch.
        if kernel::pagetable::walk(cr3, 0x01000000).is_err() {
            serial_write("  VM: code page FAIL\r\n");
            return 1;
        }
        // The user stack lives at the arch's user_stack_base().
        let stack_va = kernel::hal::user_stack_base() & !0xFFF;
        if kernel::pagetable::walk(cr3, stack_va).is_err() {
            serial_write("  VM: stack page FAIL\r\n");
            return 1;
        }
        serial_write("  OK VM: pages mapped\r\n");
        // Test CR3 switches: switch to each process's CR3 and back.
        // If kernel higher-half isn't mapped in per-process page tables,
        // write_cr3 will cause a triple fault. x86 only: the switch-and-read
        // pattern relies on identity-mapped low memory, which is laid out
        // differently on RISC-V/AArch64.
        #[cfg(target_arch = "x86_64")]
        {
            let saved = kernel::hal::read_cr3();
            for &(ep, _name) in &[
                (MFS_PROC_NR, "MFS"),
                (VFS_PROC_NR, "VFS"),
                (PM_PROC_NR, "PM"),
            ] {
                let rp = kernel::table::proc_addr(ep);
                if rp.is_null() {
                    continue;
                }
                let p_cr3 = (*rp).p_seg.p_cr3;
                if p_cr3 == 0 {
                    continue;
                }
                kernel::hal::write_cr3(p_cr3);
                // Read from user code at 0x01000000 to verify switch
                let _b = core::ptr::read_volatile(0x01000000u64 as *const u8);
                // Read from kernel higher-half (boot_cr3 mapping)
                let _k = core::ptr::read_volatile(saved as *const u8);
                kernel::hal::write_cr3(saved);
            }
            serial_write("  OK VM: CR3 switches\r\n");
        }
    }
    0
}

// H: Physical memory allocator

// Linker symbol: byte just past the end of the kernel binary.
// Same extern as in main.rs — the boot test runs in the same binary.
unsafe extern "C" {
    #[cfg(target_arch = "x86_64")]
    static __kernel_end: u8;
    /// Top of the aarch64 boot stack reservation; the physical allocator starts at
    /// or above it (see `test_allocator_aarch64_clears_boot_stack`).
    #[cfg(target_arch = "aarch64")]
    static __stack_top: u8;
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn test_allocator() -> u32 {
    // Arch-agnostic smoke test: allocate a page and verify it is non-zero
    // and page-aligned. (Not freed — boot tests exit QEMU, and only the
    // x86 allocator exposes a free_phys_page.)
    let page = match unsafe { kernel::hal::alloc_phys_page() } {
        Some(p) => p,
        None => {
            serial_write("  FAIL: alloc_phys_page returned None\r\n");
            return 1;
        }
    };
    if page == 0 || page & 0xFFF != 0 {
        serial_write("  FAIL: allocator returned invalid page 0x");
        print_hex(page);
        serial_write("\r\n");
        return 1;
    }
    serial_write("  OK allocator page=0x");
    print_hex(page);
    serial_write("\r\n");
    0
}

#[cfg(target_arch = "aarch64")]
fn test_allocator() -> u32 {
    test_allocator_aarch64_clears_boot_stack()
}

/// The aarch64 physical allocator starts at `__kernel_end`, and the linker
/// reserves the 64 KiB boot stack *below* that symbol (it grows down from
/// `__stack_top`). If the reservation sat above it instead, the loader's first
/// allocations would land inside the running stack and the loader's own writes
/// would clobber its frame — which is exactly what a kernel image grown by a few
/// KiB did to this arch, silently, as an endless loop in `load_and_prepare_proc`
/// (`PORTING_PLAN.md` finding 19).
///
/// Two notes on what this can and cannot stage. It checks the allocator's *base*,
/// not a fresh allocation: by the time this suite runs the boot loader has taken
/// tens of MiB, so the first free page is clear of the stack either way and would
/// prove nothing. And with the pre-fix layout the boot never reaches this suite
/// at all — the loader dies first — so the regression it guards against is the
/// *latent* one, where the overlap exists but the image is still small enough for
/// the boot to survive it.
#[cfg(target_arch = "aarch64")]
fn test_allocator_aarch64_clears_boot_stack() -> u32 {
    let stack_top = core::ptr::addr_of!(__stack_top) as u64;
    let base = kernel::hal::phys_alloc_base();

    if base < stack_top {
        serial_write("  FAIL: allocator base 0x");
        print_hex(base);
        serial_write(" is below the boot stack top 0x");
        print_hex(stack_top);
        serial_write(" — the loader would overwrite its own stack\r\n");
        return 1;
    }

    serial_write("  OK allocator base 0x");
    print_hex(base);
    serial_write(" is at or above the boot stack top 0x");
    print_hex(stack_top);
    serial_write("\r\n");
    0
}

#[cfg(target_arch = "x86_64")]
fn test_allocator() -> u32 {
    test_allocator_no_kernel_overlap() + test_allocator_has_free_pages()
}

#[cfg(target_arch = "x86_64")]
fn test_allocator_no_kernel_overlap() -> u32 {
    let kernel_end = core::ptr::addr_of!(__kernel_end) as u64;

    // Allocate a single page from the physical allocator.
    let page = match arch_x86_64::alloc::alloc_phys_page() {
        Some(p) => p,
        None => {
            serial_write("  FAIL: alloc_phys_page returned None\r\n");
            return 1;
        }
    };

    // Verify the page is NOT inside the kernel binary range.
    // Kernel occupies [0x200000, kernel_end).
    if page >= 0x20_0000 && page < kernel_end {
        serial_write("  FAIL: allocator page 0x");
        print_hex(page);
        serial_write(" is inside kernel range [0x200000, 0x");
        print_hex(kernel_end);
        serial_write(")\r\n");
        return 1;
    }

    // Free the page back.
    arch_x86_64::alloc::free_phys_page(page);

    serial_write("  OK allocator page 0x");
    print_hex(page);
    serial_write(" outside kernel\r\n");
    0
}

#[cfg(target_arch = "x86_64")]
fn test_allocator_has_free_pages() -> u32 {
    let alloc = arch_x86_64::alloc::global_allocator();
    if alloc.is_null() {
        serial_write("  FAIL: global allocator null\r\n");
        return 1;
    }
    unsafe {
        let free = (*alloc).free_count();
        if free < 10 {
            serial_write("  FAIL: only ");
            print_dec(free as u32);
            serial_write(" free pages (expected >= 10)\r\n");
            return 1;
        }
        serial_write("  OK allocator free pages=");
        print_dec(free as u32);
        serial_write("\r\n");
    }
    0
}

fn test_pm_has_message() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(PM_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let msg = (*rp).p_delivermsg.as_ptr();
        let src = rdi(msg, 0);
        if src < 0 {
            serial_write("  FAIL: PM no message\r\n");
            return 1;
        }
        serial_write("  OK PM msg src=");
        print_dec(src as u32);
        serial_write("\r\n");
    }
    0
}

// I: Process signal manager

fn test_boot_procs_have_sig_mgr() -> u32 {
    unsafe {
        // Check that all boot processes have s_sig_mgr == PM_PROC_NR,
        // so do_getksig_handler can find exited processes.
        let base = kernel::table::proc_table_base();
        let end = kernel::table::end_proc_addr();
        let mut ok = 0u32;
        let mut fail = 0u32;
        let mut rp = base;
        while rp < end {
            let rts = (*rp)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            if rts & kernel::proc::RtsFlags::SLOT_FREE.bits() != 0 {
                rp = rp.add(1);
                continue;
            }
            let priv_ptr = (*rp).p_priv;
            if priv_ptr.is_null() {
                serial_write("  FAIL: ");
                serial_write(core::str::from_utf8(&(*rp).p_name).unwrap_or("?"));
                serial_write(" p_priv is null\r\n");
                fail += 1;
                rp = rp.add(1);
                continue;
            }
            if (*priv_ptr).s_sig_mgr != PM_PROC_NR {
                serial_write("  FAIL: ");
                serial_write(core::str::from_utf8(&(*rp).p_name).unwrap_or("?"));
                serial_write(" s_sig_mgr=");
                print_dec((*priv_ptr).s_sig_mgr as u32);
                serial_write(" expected ");
                print_dec(PM_PROC_NR as u32);
                serial_write("\r\n");
                fail += 1;
            } else {
                ok += 1;
            }
            rp = rp.add(1);
        }
        serial_write("  OK ");
        print_dec(ok);
        serial_write(" processes have s_sig_mgr=PM\r\n");
        fail
    }
}

// J: Exec / initramfs verification

/// Run `f` with the kernel's page tables installed, then restore whatever was there.
///
/// The kernel's embedded blobs end past the user VA base (`0x1000000`), so an address-space that
/// maps a user program there shadows them: a reader on a process's CR3 sees that program's text
/// where the archive should be, which is what made `test_initramfs_boot_files` report the first
/// entry past the boundary as missing (`KNOWN_ISSUES.md` item 38). The kernel's own tables have
/// the identity map the archive lives in, and `boot_cr3` is where `arch` keeps them for exactly
/// this kind of switch (`proc_stacktrace` requires them for the same reason).
///
/// `arch-sim` has no real address translation — its `boot_cr3` is 0 — so there the call is a
/// no-op, which is the same "unavailable" path `proc_stacktrace` takes.
///
/// # Safety
///
/// Must run on a kernel stack in identity-mapped memory: the switch makes the caller's own code
/// and stack reachable only through the kernel's map.
unsafe fn on_kernel_tables<T>(f: impl FnOnce() -> T) -> T {
    let boot = kernel::hal::boot_cr3();
    let saved = unsafe { kernel::hal::read_cr3() };
    let switched = boot != 0 && boot != saved;
    if switched {
        unsafe { kernel::hal::write_cr3(boot) };
    }
    let out = f();
    if switched {
        unsafe { kernel::hal::write_cr3(saved) };
    }
    out
}

/// The physmap maps a known frame, and the two views of that frame are the same memory.
///
/// Runs on the kernel's own tables, for two reasons: the identity view (`phys as *const u64`)
/// is only guaranteed free of a user window there (`KNOWN_ISSUES.md` item 38), and the boot
/// CR3 is the table `install_physmap` is handed in this stage.
unsafe fn test_physmap() -> u32 {
    unsafe {
        on_kernel_tables(|| {
            let cr3 = kernel::hal::boot_cr3();
            if !kernel::hal::install_physmap(cr3, arch_common::PhysAccess::Physmap) {
                serial_write("  FAIL: install_physmap failed\r\n");
                return 1;
            }
            let pa = match kernel::hal::alloc_phys_page() {
                Some(p) => p,
                None => {
                    serial_write("  FAIL: physmap: no free frame\r\n");
                    return 1;
                }
            };
            let va = kernel::hal::phys_to_virt(pa);

            let pattern: u64 = 0x5048_5953_4D41_5000; // "PHYSMAP\0"
            core::ptr::write_volatile(pa as *mut u64, pattern);
            let seen = core::ptr::read_volatile(va as *const u64);
            if seen != pattern {
                serial_write("  FAIL: physmap read ");
                print_hex(seen);
                serial_write(" at ");
                print_hex(va);
                serial_write(", want ");
                print_hex(pattern);
                serial_write("\r\n");
                return 1;
            }

            core::ptr::write_volatile(va as *mut u64, !pattern);
            let back = core::ptr::read_volatile(pa as *const u64);
            if back != !pattern {
                serial_write("  FAIL: identity view missed the physmap write\r\n");
                return 1;
            }

            serial_write("  OK physmap: frame ");
            print_hex(pa);
            serial_write(" at ");
            print_hex(va);
            serial_write("\r\n");
            0
        })
    }
}

/// Every boot process's address space must reach the physmap: the kernel reaches page tables
/// through it and runs on whichever of these tables is current (`PHYSMAP.md` D2/D4).
///
/// The walk is the part that can fail cleanly, so it runs first for every process; the read from
/// the process's own CR3 is the actual proof, and it runs only once all the walks have passed, so
/// a missing window reports as a FAIL rather than as a page fault inside the boot test.
unsafe fn test_physmap_everywhere() -> u32 {
    unsafe {
        let pa = match kernel::hal::alloc_phys_page() {
            Some(p) => p,
            None => {
                serial_write("  FAIL: physmap: no free frame\r\n");
                return 1;
            }
        };
        let va = kernel::hal::phys_to_virt(pa);

        let mut failures = 0u32;
        for &(proc_nr, name) in BOOTED_PROCS {
            let rp = kernel::table::proc_addr(proc_nr);
            let cr3 = if rp.is_null() { 0 } else { (*rp).p_seg.p_cr3 };
            if cr3 == 0 {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" has no page table\r\n");
                failures += 1;
                continue;
            }
            if kernel::pagetable::walk(cr3, va).is_err() {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" cannot reach the physmap\r\n");
                failures += 1;
            }
        }
        if failures != 0 {
            return failures;
        }

        // The proof: one frame, written through the boot tables and read back through every
        // boot process's tables, has to be the same memory.
        const PATTERN: u64 = 0x5048_5953_4D41_5001;
        core::ptr::write_volatile(pa as *mut u64, PATTERN);
        for &(proc_nr, name) in BOOTED_PROCS {
            let rp = kernel::table::proc_addr(proc_nr);
            let cr3 = (*rp).p_seg.p_cr3;
            let saved = kernel::hal::read_cr3();
            if cr3 == saved {
                continue;
            }
            kernel::hal::write_cr3(cr3);
            let seen = core::ptr::read_volatile(va as *const u64);
            kernel::hal::write_cr3(saved);
            if seen != PATTERN {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" physmap read ");
                print_hex(seen);
                serial_write("\r\n");
                failures += 1;
            }
        }

        if failures == 0 {
            serial_write("  OK physmap reachable from every boot process's table\r\n");
        }
        failures
    }
}

/// The kernel's pointer to a frame: the conversion `walk` itself uses, so a test that builds a
/// table by hand and one that walks it agree on where the table is.
fn frame_ptr(pa: u64) -> *mut u64 {
    kernel::hal::phys_to_virt(pa) as *mut u64
}

/// Zero a whole frame, the way a page-table page has to start out.
unsafe fn zero_frame(pa: u64) {
    for i in 0..512 {
        unsafe { core::ptr::write_volatile(frame_ptr(pa).add(i), 0) };
    }
}

/// A walk must reach a page table through the physmap, not through the identity map.
///
/// The construction is the one P2b's gate asks for: build a chain whose *root* is a frame, then make
/// that frame's identity address resolve somewhere else — a zeroed decoy — in the tables the kernel
/// is running on. Anything that reached its root at "the virtual address equal to its physical
/// address" would now read the decoy and report `NotMapped`; a walk that goes through the physmap
/// still finds the real root. The identity mapping is put back before this returns.
///
/// Runs on the kernel's own tables, because that is where the shadow has to be placed for it to be
/// the mapping the walk would otherwise use.
unsafe fn test_walk_through_the_physmap() -> u32 {
    unsafe {
        on_kernel_tables(|| {
            let boot = kernel::hal::boot_cr3();
            if boot == 0 {
                serial_write("  SKIP: physmap walk check needs the boot tables\r\n");
                return 0;
            }

            // The VA the walk is asked about, and the physical address the 1 GiB leaf names. Both sit
            // inside the identity map's span; neither is ever *accessed* — the walk only reads the
            // entries — so the leaf may name an address above RAM.
            let va: u64 = 0x8000_0000;
            let target_pa: u64 = va;
            let expected_level = 3;

            let mut frames = [0u64; 4];
            for f in frames.iter_mut() {
                match kernel::hal::alloc_phys_page() {
                    Some(p) => *f = p,
                    None => {
                        serial_write("  FAIL: physmap walk: no free frame\r\n");
                        return 1;
                    }
                }
                zero_frame(*f);
            }
            let root_pa = frames[0];
            let decoy_pa = frames[1];

            // Chain from the root down to the 1 GiB level, then a 1 GiB block leaf there. On a
            // three-level arch (SV39) the root *is* the 1 GiB level, so no table is chained.
            let mut table_pa = root_pa;
            let mut next = 2usize;
            for level in (3..kernel::hal::pt_levels()).rev() {
                let child_pa = frames[next];
                next += 1;
                core::ptr::write(
                    frame_ptr(table_pa).add(kernel::hal::pt_index(va, level)),
                    kernel::hal::build_pte(child_pa, kernel::hal::pte_nonleaf_flags()),
                );
                table_pa = child_pa;
            }
            let leaf_flags = kernel::hal::pte_present()
                | kernel::hal::pte_writable()
                | kernel::hal::pte_large_page();
            core::ptr::write(
                frame_ptr(table_pa).add(kernel::hal::pt_index(va, 2)),
                kernel::hal::build_pte(target_pa, leaf_flags),
            );

            // Shadow the root's identity address: from here, VA `root_pa` is the (zeroed) decoy.
            let kernel_flags = kernel::pagetable::PG_P | kernel::pagetable::PG_RW;
            if kernel::pagetable::map_page(boot, root_pa, decoy_pa, kernel_flags).is_err() {
                serial_write("  FAIL: physmap walk: could not shadow the root\r\n");
                return 1;
            }

            let outcome = kernel::pagetable::walk(root_pa, va);
            let resolved = matches!(
                &outcome,
                Ok(r) if r.level == expected_level
                    && r.pte_value & kernel::pagetable::PG_P != 0
                    && kernel::hal::pte_to_phys(r.pte_value) == target_pa
            );

            // The control that keeps this honest. Read, through the identity address, the entry the
            // chain wrote at the root's own level: it must now be the decoy's zero. If it still read
            // the real entry, the walk below would prove nothing about which address it reached the
            // table at — the check would pass for a walk that never consulted the physmap.
            let chain_idx = kernel::hal::pt_index(va, kernel::hal::pt_levels() - 1);
            let spoiled = core::ptr::read_volatile((root_pa as *const u64).add(chain_idx));

            // Put the identity mapping back: the frame is kernel RAM either way, and the rest of
            // boot still runs on these tables.
            let _ = kernel::pagetable::map_page(boot, root_pa, root_pa, kernel_flags);

            if spoiled != 0 {
                serial_write("  FAIL: physmap walk: the identity shadow did not take\r\n");
                return 1;
            }

            if resolved {
                serial_write(
                    "  OK physmap walk: a table shadowed by the identity map still resolves\r\n",
                );
                0
            } else {
                serial_write("  FAIL: physmap walk: table ");
                print_hex(root_pa);
                serial_write(" did not resolve from behind the identity map\r\n");
                1
            }
        })
    }
}

/// `vm_memset` with a process argument must write *that* process's memory, not whatever address
/// space the kernel happens to be running on.
///
/// The construction: take a frame the running (boot) tables map identity — one this test owns — and
/// have the named process map the *same* virtual address to a different frame. A write that
/// dereferences the address lands in the first frame; one that loads the named process's tables
/// lands in the second. That is C's shape (`createpde` + `phys_memset`, neither of which
/// dereferences the address) and `KNOWN_ISSUES.md` item 15's failure mode.
unsafe fn test_vm_memset_targets_the_named_process() -> u32 {
    unsafe {
        on_kernel_tables(|| {
            const PATTERN: u8 = 0x5A;
            const LEN: usize = 64;

            let mut frames = [0u64; 2];
            for f in frames.iter_mut() {
                match kernel::hal::alloc_phys_page() {
                    Some(p) => *f = p,
                    None => {
                        serial_write("  FAIL: vm_memset: no free frame\r\n");
                        return 1;
                    }
                }
                zero_frame(*f);
            }
            // `va` is the shadow frame's own address, so on the running tables it *is* the frame.
            let va = frames[0];
            let target_pa = frames[1];

            // A boot process to name: it has a page table of its own, and that table is not the
            // boot one the kernel is running on here.
            let mut named = None;
            for &(proc_nr, name) in BOOTED_PROCS {
                let rp = kernel::table::proc_addr(proc_nr);
                if !rp.is_null() && (*rp).p_seg.p_cr3 != 0 {
                    named = Some((proc_nr, name, (*rp).p_seg.p_cr3));
                    break;
                }
            }
            let (proc_nr, name, cr3) = match named {
                Some(t) => t,
                None => {
                    serial_write("  SKIP: vm_memset: no boot process with its own table\r\n");
                    return 0;
                }
            };

            // In that process's tables the address maps somewhere else. `pte_user_flags` is the
            // port's leaf set for a page a process is accessed through: it carries the Accessed and
            // Dirty bits SV39 needs for a write not to fault.
            let kernel_flags = kernel::pagetable::PG_P | kernel::pagetable::PG_RW;
            if kernel::pagetable::map_page(cr3, va, target_pa, kernel::hal::pte_user_flags())
                .is_err()
            {
                serial_write(
                    "  FAIL: vm_memset: could not map the address in the named process\r\n",
                );
                return 1;
            }

            let r = kernel::vm::vm_memset(proc_nr, va, PATTERN, LEN);

            // Put the named process's identity mapping back before reporting: the frame is RAM
            // either way, and the rest of boot still runs with that table swapped in.
            let _ = kernel::pagetable::map_page(cr3, va, va, kernel_flags);

            if r != 0 {
                serial_write("  FAIL: vm_memset returned an error\r\n");
                return 1;
            }

            let target =
                core::slice::from_raw_parts(kernel::hal::phys_to_virt(target_pa) as *const u8, LEN);
            let shadow =
                core::slice::from_raw_parts(kernel::hal::phys_to_virt(va) as *const u8, LEN);
            if !target.iter().all(|&b| b == PATTERN) {
                serial_write("  FAIL: vm_memset did not reach the named process's frame\r\n");
                return 1;
            }
            if !shadow.iter().all(|&b| b == 0) {
                serial_write("  FAIL: vm_memset wrote the running space instead\r\n");
                return 1;
            }

            serial_write("  OK vm_memset wrote ");
            serial_write(name);
            serial_write("'s own frame, not the running space\r\n");
            0
        })
    }
}

fn test_initramfs_echo_exists() -> u32 {
    match kernel::initramfs::find_initramfs_file("/bin/echo") {
        Some((data, _mode)) => {
            serial_write("  OK /bin/echo exists, size=");
            print_dec(data.len() as u32);
            serial_write("\r\n");
            0
        }
        None => {
            serial_write("  FAIL: /bin/echo not found in initramfs\r\n");
            1
        }
    }
}

fn test_initramfs_sh_exists() -> u32 {
    match kernel::initramfs::find_initramfs_file("/bin/sh") {
        Some((data, _mode)) => {
            serial_write("  OK /bin/sh exists, size=");
            print_dec(data.len() as u32);
            serial_write("\r\n");
            0
        }
        None => {
            serial_write("  FAIL: /bin/sh not found\r\n");
            1
        }
    }
}

fn test_initramfs_boot_files() -> u32 {
    // Verify all boot-critical binaries exist in initramfs
    let files = [
        "/sbin/init",
        "/bin/sh",
        "/bin/echo",
        "/sbin/pm",
        "/sbin/vfs",
        "/sbin/vm",
        "/sbin/rs",
        "/sbin/ds",
        "/sbin/sched",
        "/sbin/tty",
        "/sbin/mfs",
        "/sbin/pfs",
        "/sbin/devman",
        "/sbin/ramdisk",
        "/sbin/virtio_blk",
        // The PFS pipe smoke-test binary (pipe fstat/ftruncate/fchmod +
        // FIFO data plane). Its guest run is /bin/pipetest; the wire
        // formats it exercises are pinned by host tests.
        "/bin/pipetest",
    ];
    let mut failures: u32 = 0;
    for &f in &files {
        if kernel::initramfs::find_initramfs_file(f).is_none() {
            serial_write("  FAIL: missing ");
            serial_write(f);
            serial_write("\r\n");
            failures += 1;
        }
    }
    if failures == 0 {
        serial_write("  OK all boot files present\r\n");
    }
    failures
}

/// The dynamic-linking artifacts an image carries must be there: the loader, the shared
/// C library, and one C program linked against it.
///
/// They are not boot-critical — nothing the boot path execs carries `PT_INTERP` — so
/// they are not in `test_initramfs_boot_files`'s list. They are checked here because
/// they are `BOOT_BINS`: a `crates/boot-image/src/manifest.rs` entry the build did not
/// produce would make an image that boots and cannot run its own dynamic program, which
/// only shows up when something tries. The *behaviour* is `tools/smoke/dyn.tsv`'s
/// `/bin/dynclib` step in the three `just test-dynlink-<arch>` gates.
fn test_dynamic_artifacts_present() -> u32 {
    let files = ["/libexec/ld.so", "/lib/libc.so", "/bin/dynclib"];
    let mut failures: u32 = 0;
    for &f in &files {
        if kernel::initramfs::find_initramfs_file(f).is_none() {
            serial_write("  FAIL: missing ");
            serial_write(f);
            serial_write("\r\n");
            failures += 1;
        }
    }
    if failures == 0 {
        serial_write("  OK dynamic-linking artifacts present\r\n");
    }
    failures
}

/// The PFS pipe smoke test must be a valid ELF in the initramfs (not a
/// truncated entry), mirroring the echo ELF check.
fn test_initramfs_pipetest_elf() -> u32 {
    let (data, _mode) = match kernel::initramfs::find_initramfs_file("/bin/pipetest") {
        Some(d) => d,
        None => return 1,
    };
    match kernel::elf::parse_elf_header(data) {
        Ok(_) => {
            serial_write("  OK /bin/pipetest is a valid ELF, size=");
            print_dec(data.len() as u32);
            serial_write("\r\n");
            0
        }
        Err(_) => {
            serial_write("  FAIL: /bin/pipetest is not a valid ELF\r\n");
            1
        }
    }
}

fn test_initramfs_echo_elf() -> u32 {
    unsafe {
        let (data, _mode) = match kernel::initramfs::find_initramfs_file("/bin/echo") {
            Some(d) => d,
            None => return 1,
        };
        let ehdr = match kernel::elf::parse_elf_header(data) {
            Ok(e) => e,
            Err(_) => {
                serial_write("  FAIL: /bin/echo bad ELF header\r\n");
                return 1;
            }
        };
        serial_write("  OK /bin/echo ELF entry=0x");
        print_hex(ehdr.e_entry);
        serial_write(" phnum=");
        print_dec(ehdr.e_phnum as u32);
        serial_write("\r\n");
        // Check PT_LOAD segments
        let phoff = ehdr.e_phoff as usize;
        let phnum = ehdr.e_phnum as usize;
        let phentsize = ehdr.e_phentsize as usize;
        let mut load_count = 0u32;
        for i in 0..phnum {
            let phdr =
                &*(data.as_ptr().add(phoff + i * phentsize) as *const kernel::elf::Elf64Phdr);
            if phdr.p_type != kernel::elf::PT_LOAD {
                continue;
            }
            load_count += 1;
            serial_write("    LOAD vaddr=0x");
            print_hex(phdr.p_vaddr);
            serial_write(" memsz=");
            print_dec(phdr.p_memsz as u32);
            serial_write(" filesz=");
            print_dec(phdr.p_filesz as u32);
            serial_write("\r\n");
        }
        if load_count == 0 {
            serial_write("  FAIL: no PT_LOAD segments\r\n");
            return 1;
        }
        serial_write("  OK /bin/echo PT_LOAD count=");
        print_dec(load_count);
        serial_write("\r\n");
        0
    }
}

// K: PM page table check for MPROC

fn test_pm_mproc_pt() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(PM_PROC_NR);
        if rp.is_null() {
            serial_write("  FAIL: PM not found\r\n");
            return 1;
        }
        let cr3 = (*rp).p_seg.p_cr3;
        if cr3 == 0 {
            serial_write("  FAIL: PM has no CR3\r\n");
            return 1;
        }
        serial_write("  PM CR3=0x");
        print_hex(cr3);
        serial_write("\r\n");

        // Walk known PM code pages — must be mapped with user permissions.
        // Every userland binary links at 0x01000000 (tools/minix-user.ld),
        // so the entry point page is identical on all three arches.
        let slot0_va = 0x1000000u64 + 0x10;
        match kernel::pagetable::walk(cr3, slot0_va) {
            Ok(r) => {
                let has_user = r.pte_value & kernel::pagetable::PG_U != 0;
                serial_write("  PM slot0 PTE=0x");
                print_hex(r.pte_value);
                if !has_user {
                    serial_write(" FAIL (no PG_U)\r\n");
                    return 1;
                }
                serial_write("\r\n");
            }
            Err(_) => {
                serial_write("  FAIL: PM slot0 not mapped\r\n");
                return 1;
            }
        }

        // Check user stack is mapped.
        // The stack lives at the arch's user_stack_base() (0x0FE00000 on
        // x86, 0x8FE00000 on RISC-V, 0x3FC00000 on AArch64). Walking the
        // page-aligned base must succeed with PG_U.
        let stack_va = kernel::hal::user_stack_base() & !0xFFF;
        match kernel::pagetable::walk(cr3, stack_va) {
            Ok(r) => {
                let has_user = r.pte_value & kernel::pagetable::PG_U != 0;
                serial_write("  PM stack PTE=0x");
                print_hex(r.pte_value);
                if !has_user {
                    serial_write(" FAIL (no PG_U)\r\n");
                    return 1;
                }
                serial_write("\r\n");
            }
            Err(_) => {
                serial_write("  FAIL: PM stack not mapped\r\n");
                return 1;
            }
        }
    }
    0
}

/// Verify every booted process has a non-zero per-process page table
/// and that a walk at the entry point succeeds.
fn test_boot_procs_page_tables() -> u32 {
    unsafe {
        // Only check processes that actually get per-process page tables during boot (see
        // [`BOOTED_PROCS`]).
        let mut failures = 0u32;
        for &(proc_nr, name) in BOOTED_PROCS {
            let rp = kernel::table::proc_addr(proc_nr);
            if rp.is_null() {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" null proc\r\n");
                failures += 1;
                continue;
            }

            let cr3 = (*rp).p_seg.p_cr3;
            if cr3 == 0 {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" CR3=0\r\n");
                failures += 1;
                continue;
            }

            // Walk at the process's entry point (from p_reg).
            // x86_64: RIP at p_reg offset 16.
            #[cfg(target_arch = "x86_64")]
            let entry_va: u64 =
                core::ptr::read_unaligned((*rp).p_reg.as_ptr().add(16) as *const u64);
            #[cfg(not(target_arch = "x86_64"))]
            let entry_va = 0x1000000u64;

            let walk_va = entry_va & !0xFFF;
            match kernel::pagetable::walk(cr3, walk_va) {
                Ok(r) => {
                    let has_user = r.pte_value & kernel::pagetable::PG_U != 0;
                    if !has_user {
                        serial_write("  FAIL: ");
                        serial_write(name);
                        serial_write(" entry missing PG_U\r\n");
                        failures += 1;
                    }
                }
                Err(_) => {
                    serial_write("  FAIL: ");
                    serial_write(name);
                    serial_write(" entry not mapped\r\n");
                    failures += 1;
                }
            }
        }

        if failures == 0 {
            serial_write("  OK all booted procs have walkable page tables\r\n");
        }
        failures
    }
}

/// No EL1-only kernel mapping lies in the user window (`PHYSMAP.md` P4, first half).
///
/// A kernel identity mapping over the user window is what `KNOWN_ISSUES.md` item 38 measured: ring 0
/// reaches a frame at a VA the process also owns, so a frame the allocator hands out inside that
/// window *is* the process's own memory and the kernel writes there. AArch64's window is the low
/// 1 GiB, whose identity entries the shrink removed, so every 2 MiB step of it must now be absent or
/// a user-accessible entry. x86 and RISC-V join this check when their own shrink lands — today their
/// identity maps still cover the window, which is exactly what it would report.
#[cfg(target_arch = "aarch64")]
fn test_no_kernel_mapping_in_user_window() -> u32 {
    unsafe {
        /// 2 MiB steps: the offending mappings were 2 MB / 1 GB kernel blocks, so this granularity
        /// cannot step over one.
        fn check_root(cr3: u64, label: &str) -> u32 {
            let mut va = 0u64;
            while va < kernel::pagetable::MAX_USER_ADDRESS {
                if let Ok(r) = unsafe { kernel::pagetable::walk(cr3, va) } {
                    if r.pte_value & kernel::pagetable::PG_U == 0 {
                        serial_write("  FAIL: ");
                        serial_write(label);
                        serial_write(" has an EL1-only mapping at user MiB ");
                        print_dec((va >> 20) as u32);
                        serial_write("\r\n");
                        return 1;
                    }
                }
                va += 0x20_0000;
            }
            0
        }

        let mut failures = 0u32;
        for &(proc_nr, name) in BOOTED_PROCS {
            let rp = kernel::table::proc_addr(proc_nr);
            if rp.is_null() {
                continue;
            }
            let cr3 = (*rp).p_seg.p_cr3;
            if cr3 == 0 {
                continue;
            }
            failures += check_root(cr3, name);
        }

        // The exec constructor builds PUD[0] through `create_low_gb_pmd_table` rather than from the
        // boot block, and no boot process's table goes through it, so it gets its own check.
        let exec_root = kernel::hal::exec_create_root(kernel::hal::boot_cr3());
        if exec_root != 0 {
            failures += check_root(exec_root, "an exec root");
        }

        if failures == 0 {
            serial_write("  OK no kernel mapping over the user window\r\n");
        }
        failures
    }
}

/// Allocate a fresh page table, map one page, walk it back, verify PA.
///
/// Catches validation-bound regressions (e.g. `map_page` rejecting
/// physical addresses above an arbitrary cutoff like 0x1000_0000 on RISC-V).
/// Also validates huge-page splitting when the inserted VA falls within
/// an existing 1GB/2MB boot-PTE range.
fn test_map_page_walk_roundtrip() -> u32 {
    unsafe {
        // 1. Allocate a root page table page and zero it.
        let root = match kernel::hal::alloc_phys_page() {
            Some(p) => p,
            None => {
                serial_write("  FAIL: alloc root page\r\n");
                return 1;
            }
        };
        core::ptr::write_bytes(root as *mut u8, 0, 4096);

        // 2. Allocate a page to map.
        let test_pa = match kernel::hal::alloc_phys_page() {
            Some(p) => p,
            None => {
                serial_write("  FAIL: alloc data page\r\n");
                return 1;
            }
        };

        // 3. Pick a VA that is NOT backed by any boot-PTE copy.
        //    0x6000_0000 is above the boot identity map on x86_64 (indices 1..511)
        //    and well within the 512-entry root on both arches.
        let test_va = 0x6000_0000u64;

        // 4. Build arch-appropriate user page flags.
        let flags = kernel::hal::pte_user_flags();

        // 5. Map the page — this must allocate intermediate tables and
        //    write the final PTE.  The map_page validation must accept
        //    the physical addresses returned by alloc_phys_page().
        if kernel::pagetable::map_page(root, test_va, test_pa, flags).is_err() {
            serial_write("  FAIL: map_page returned error\r\n");
            return 1;
        }

        // 6. Walk back and verify the physical address matches.
        match kernel::pagetable::walk(root, test_va) {
            Ok(result) => {
                let mapped_pa = kernel::hal::pte_to_phys(result.pte_value);
                let expected_pa = test_pa & kernel::hal::pte_frame_mask();
                if mapped_pa != expected_pa {
                    serial_write("  FAIL: PA mismatch mapped=0x");
                    print_hex(mapped_pa);
                    serial_write(" expected=0x");
                    print_hex(expected_pa);
                    serial_write("\r\n");
                    return 1;
                }
                let has_user = result.pte_value & kernel::pagetable::PG_U != 0;
                if !has_user {
                    serial_write("  FAIL: mapped PTE missing PG_U\r\n");
                    return 1;
                }
                serial_write("  OK map+walk roundtrip PA=0x");
                print_hex(mapped_pa);
                serial_write("\r\n");
            }
            Err(_) => {
                serial_write("  FAIL: walk after map_page\r\n");
                return 1;
            }
        }

        0
    }
}

// N: Every boot process alive (beyond the original VFS/MFS/PM trio).
// The list mirrors the boot_procs arrays in main.rs / riscv64.rs /
// aarch64.rs under the boot-test feature (INIT excluded).

fn test_all_boot_procs_alive() -> u32 {
    let procs: &[(i32, &str)] = &[
        (DS_PROC_NR, "DS"),
        (RS_PROC_NR, "RS"),
        (SCHED_PROC_NR, "SCHED"),
        (VFS_PROC_NR, "VFS"),
        (RAMDISK_PROC_NR, "RAMDISK"),
        (VIRTIO_BLK_PROC_NR, "VIRTIO_BLK"),
        (VIRTIO_NET_PROC_NR, "VIRTIO_NET"),
        (NET_PROC_NR, "NET"),
        (VM_PROC_NR, "VM"),
        (MFS_PROC_NR, "MFS"),
        (PFS_PROC_NR, "PFS"),
        (TTY_PROC_NR, "TTY"),
        (DEVMAN_PROC_NR, "DEVMAN"),
    ];
    let mut failures = 0;
    for &(ep, name) in procs {
        failures += test_alive(ep, name);
    }
    failures
}

/// Verify every boot process has a consistent process-table entry:
/// endpoint encoding, magic number, and a non-null privilege structure.
fn test_boot_procs_consistent() -> u32 {
    let procs: &[(i32, &str)] = &[
        (DS_PROC_NR, "ds"),
        (RS_PROC_NR, "rs"),
        (PM_PROC_NR, "pm"),
        (SCHED_PROC_NR, "sched"),
        (VFS_PROC_NR, "vfs"),
        (RAMDISK_PROC_NR, "ramdisk"),
        (VIRTIO_BLK_PROC_NR, "virtio_blk"),
        (VIRTIO_NET_PROC_NR, "virtio_net"),
        (NET_PROC_NR, "net"),
        (VM_PROC_NR, "vm"),
        (MFS_PROC_NR, "mfs"),
        (PFS_PROC_NR, "pfs"),
        (TTY_PROC_NR, "tty"),
        (DEVMAN_PROC_NR, "devman"),
    ];
    let mut failures = 0;
    for &(nr, name) in procs {
        unsafe {
            let rp = kernel::table::proc_addr(nr);
            if rp.is_null() {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" null proc\r\n");
                failures += 1;
                continue;
            }
            let expected = kernel::table::make_endpoint(0, nr);
            if (*rp).p_endpoint != expected {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" endpoint ");
                print_dec((*rp).p_endpoint as u32);
                serial_write(" expected ");
                print_dec(expected as u32);
                serial_write("\r\n");
                failures += 1;
            }
            if (*rp).p_magic != kernel::proc::PMAGIC {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" bad magic\r\n");
                failures += 1;
            }
            if (*rp).p_priv.is_null() {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" null p_priv\r\n");
                failures += 1;
            }
        }
    }
    if failures == 0 {
        serial_write("  OK all boot procs consistent (endpoint/magic/priv)\r\n");
    }
    failures
}

/// Verify the pre-allocated brk heap window (`hal::user_heap_base()` .. +1 MiB)
/// is mapped in every boot process's page table except VM (VM is absent from
/// the list below, which keeps the check valid on all arches).
fn test_brk_heap_mapped() -> u32 {
    let procs: &[(i32, &str)] = &[
        (DS_PROC_NR, "ds"),
        (RS_PROC_NR, "rs"),
        (PM_PROC_NR, "pm"),
        (SCHED_PROC_NR, "sched"),
        (VFS_PROC_NR, "vfs"),
        (RAMDISK_PROC_NR, "ramdisk"),
        (MFS_PROC_NR, "mfs"),
        (PFS_PROC_NR, "pfs"),
        (TTY_PROC_NR, "tty"),
        (DEVMAN_PROC_NR, "devman"),
    ];
    let mut failures = 0;
    for &(nr, name) in procs {
        unsafe {
            let rp = kernel::table::proc_addr(nr);
            if rp.is_null() {
                failures += 1;
                continue;
            }
            let cr3 = (*rp).p_seg.p_cr3;
            if cr3 == 0 {
                continue;
            }
            if kernel::pagetable::walk(cr3, kernel::hal::user_heap_base()).is_err() {
                serial_write("  FAIL: ");
                serial_write(name);
                serial_write(" brk heap not mapped\r\n");
                failures += 1;
            }
        }
    }
    if failures == 0 {
        serial_write("  OK all boot procs have brk heap mapped\r\n");
    }
    failures
}

/// Verify the boot filesystem image is mapped in the ramdisk server's
/// address space at RAMDISK_IMAGE_VA.
fn test_mfs_ramdisk_mapped() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(RAMDISK_PROC_NR);
        if rp.is_null() {
            serial_write("  FAIL: no ramdisk server\r\n");
            return 1;
        }
        let cr3 = (*rp).p_seg.p_cr3;
        if cr3 == 0 {
            serial_write("  FAIL: ramdisk server no CR3\r\n");
            return 1;
        }
        if kernel::pagetable::walk(cr3, arch_common::com::RAMDISK_IMAGE_VA).is_err() {
            serial_write("  FAIL: ramdisk image VA not mapped\r\n");
            return 1;
        }
        serial_write("  OK ramdisk image mapped\r\n");
    }
    0
}

// Exit helpers

fn exit_qemu_success() -> ! {
    kernel::hal::qemu_exit(0)
}
fn exit_qemu_failure(f: u32) -> ! {
    kernel::hal::qemu_exit(if f == 0 { 1 } else { f })
}
fn serial_write(s: &str) {
    for &b in s.as_bytes() {
        kernel::hal::serial_write_byte(b);
    }
}
fn print_dec(mut n: u32) {
    if n == 0 {
        serial_write("0");
        return;
    }
    let mut buf = [0u8; 12];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    serial_write(core::str::from_utf8(&buf[i..]).unwrap_or(""));
}
fn print_hex(val: u64) {
    let hex = b"0123456789abcdef";
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        kernel::hal::serial_write_byte(hex[nibble]);
    }
}
