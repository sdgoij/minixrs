//! RISC-V64 kernel boot binary entry point.
//!
//! Build with: `cargo build -p kernel-boot --bin kernel-boot-riscv64 --target riscv64gc-unknown-none-elf`

#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(test), no_main)]
#![allow(static_mut_refs)]
#![cfg(target_arch = "riscv64")]

#[cfg(not(test))]
use core::panic::PanicInfo;

use core::arch::global_asm;

// _start entry point — called by QEMU/OpenSBI.
// a0 = hart ID, a1 = DTB pointer.
global_asm!(
    r#"
.section .text.boot, "ax"
.globl _start

_start:
    # Set up a temporary stack
    la      sp, _start
    li      t0, 0x10000
    add     sp, sp, t0

    # Clear BSS
    la      t0, __bss_start
    la      t1, __bss_end
    bge     t0, t1, 2f
1:
    sd      zero, 0(t0)
    addi    t0, t0, 8
    blt     t0, t1, 1b
2:

    # Call kmain(hart_id, dtb_ptr)
    mv      a0, a0
    mv      a1, a1
    call    kmain

    # Should never reach here
    wfi
    j       _start
"#
);

// BSS and initramfs symbols are defined by the custom linker script
// (tools/minix-raw-riscv64.ld).

/// Serial output helper.
fn serial_write(s: &str) {
    for &b in s.as_bytes() {
        arch_riscv64::sbi::console_putchar(b);
    }
}

/// RISC-V64 kernel main entry.
///
/// # Safety
///
/// Must be called once on the boot hart in S-mode, with a0=hart_id and a1=dtb_ptr.
#[cfg(not(test))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kmain(hart_id: u64, dtb_ptr: u64) -> ! {
    // Only hart 0 proceeds
    if hart_id != 0 {
        loop {
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack));
            }
        }
    }

    // Parse FDT for memory information
    // SAFETY: dtb_ptr points to a valid FDT provided by OpenSBI
    let (mem_base, mem_size) =
        if let Some(info) = unsafe { arch_riscv64::boot::parse_fdt_memory(dtb_ptr as *const u8) } {
            info
        } else {
            // Fallback: assume standard QEMU virt layout with 256MB RAM
            (0x80000000u64, 256 * 1024 * 1024)
        };

    let kernel_end = 0x80200000u64 + 0x800000u64; // 8MB kernel estimate

    let mut mmap = arch_riscv64::alloc::PhysicalMemoryMap::new();
    if kernel_end < mem_base + mem_size {
        mmap.add(kernel_end, mem_base + mem_size);
    }
    // SAFETY: Called once during early boot with valid memory info
    unsafe {
        arch_riscv64::alloc::init_allocator(&mmap);
    }

    // Set up STVEC to point to the trap vector
    let trap_vec = arch_riscv64::trap_asm::trap_vector_addr();
    unsafe {
        core::arch::asm!("csrw stvec, {addr}", addr = in(reg) trap_vec, options(nomem, nostack));
    }

    // Initialize per-CPU data (tp register)
    // SAFETY: Called once on the boot hart
    unsafe {
        arch_riscv64::cpulocals::init_cpulocals();
    }

    // Print banner via SBI
    serial_write("\r\nHello MINIX/RISC-V!\r\n");

    // Initialize kernel subsystems
    kernel::init();

    // Initialize the process table
    unsafe {
        kernel::table::proc_init();
    }

    // Initialize basic userspace syscall handlers
    unsafe {
        kernel::syscall::init_basic_syscalls();
    }
    unsafe {
        // Wrap the kernel dispatcher to supply the caller from CPU locals.
        unsafe fn riscv_syscall_handler(nr: usize, args: &[u64; 6]) -> i64 {
            let caller = arch_riscv64::hal::current_proc();
            unsafe {
                kernel::syscall::dispatch_basic_syscall(caller as *mut kernel::proc::Proc, nr, args)
            }
        }
        arch_riscv64::trap::register_syscall_handler(riscv_syscall_handler);
    }
    unsafe {
        // Post-syscall hook: if current process is blocked (e.g., on IPC),
        // pick a new runnable process and overwrite the trap frame.
        unsafe fn riscv_post_syscall(frame: &mut [u8; 296]) {
            let caller = arch_riscv64::hal::current_proc() as *mut kernel::proc::Proc;
            if caller.is_null() {
                return;
            }
            let rts = unsafe {
                (*caller)
                    .p_rts_flags
                    .load(core::sync::atomic::Ordering::Relaxed)
            };
            if rts != 0 {
                // Current process blocked — pick a new one.
                if let Some(next_proc) = unsafe { kernel::sched::pick_proc() } {
                    unsafe {
                        // Copy new process's p_reg into frame[0..256]
                        core::ptr::copy_nonoverlapping(
                            &raw const (*next_proc).p_reg as *const u8,
                            frame.as_mut_ptr(),
                            256,
                        );
                        // Copy sepc from p_reg[0..8] to frame[256..264]
                        let p_reg = &raw const (*next_proc).p_reg;
                        let sepc_bytes = core::ptr::read(p_reg as *const [u8; 8]);
                        frame[256..264].copy_from_slice(&sepc_bytes);
                        // Copy sstatus from p_reg[248..256] to frame[264..272]
                        let sst_bytes = core::ptr::read(p_reg.add(248) as *const [u8; 8]);
                        frame[264..272].copy_from_slice(&sst_bytes);
                        // Load new process's page table
                        let new_cr3 = (*next_proc).p_seg.p_cr3;
                        if new_cr3 != 0 {
                            kernel::hal::write_cr3(new_cr3);
                        }
                        // Update current process pointer
                        arch_riscv64::cpulocals::set_current_proc(next_proc as u64);
                    }
                }
            }
        }
        arch_riscv64::trap::register_post_syscall_hook(riscv_post_syscall);
    }

    // Initialize timer (100 Hz)
    unsafe {
        arch_riscv64::clint::init_timer(100);
    }

    // Enable S-mode interrupts (timer + external)
    unsafe {
        let mut sie_val: u64;
        core::arch::asm!("csrr {val}, sie", val = out(reg) sie_val, options(nomem, nostack));
        sie_val |= (1u64 << 5) | (1u64 << 9); // STIE | SEIE
        core::arch::asm!("csrw sie, {val}", val = in(reg) sie_val, options(nomem, nostack));
    }

    // Initialize PLIC
    unsafe {
        arch_riscv64::plic::init_plic();
    }

    // ── Create boot SV39 page table and enable MMU ────────────────────
    serial_write("  enabling SV39 paging...\r\n");
    unsafe {
        if let Some(boot_pt) = create_boot_page_table() {
            kernel::hal::write_cr3(boot_pt);
            serial_write("  SV39 enabled\r\n");
        } else {
            serial_write("  FAILED: boot page table\r\n");
            loop {
                core::arch::asm!("wfi", options(nomem, nostack));
            }
        }
    }

    // ── Load boot processes from initramfs ────────────────────────────
    use arch_common::com::*;

    serial_write("  loading boot processes...\r\n");

    // Define all boot processes: (path, proc_nr)
    let boot_procs: &[(&str, i32)] = &[
        ("/sbin/pm", PM_PROC_NR),     // Process Manager
        ("/sbin/rs", RS_PROC_NR),     // Reincarnation Server
        ("/sbin/vfs", VFS_PROC_NR),   // Virtual File System
        ("/sbin/init", INIT_PROC_NR), // init
    ];

    // Load each boot process from initramfs, storing InitInfo for
    // per-process page table creation.
    // Note: boot_init's internal error messages use `print!` which is
    // a no-op on RISC-V (x86_64 COM1 only), so we add our own diagnostics.
    let mut boot_infos: [core::mem::MaybeUninit<kernel_boot::boot_init::InitInfo>; 8] =
        unsafe { core::mem::zeroed() };
    for (i, &(path, proc_nr)) in boot_procs.iter().enumerate() {
        let info = match unsafe {
            kernel_boot::boot_init::load_and_prepare_proc(path, proc_nr, &[path])
        } {
            Some(info) => info,
            None => {
                serial_write("  FAILED loading ");
                serial_write(path);
                serial_write("\r\n");
                serial_write("  Check: initramfs contains binary? Allocator has free pages?\r\n");
                // Dump allocator state
                serial_write("  Allocator may be out of contiguous memory\r\n");
                loop {
                    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                }
            }
        };
        boot_infos[i] = core::mem::MaybeUninit::new(info);
    }

    serial_write("  creating per-process page tables...\r\n");

    // Create per-process (restricted) page tables and enqueue each process.
    let mut first_proc: *mut kernel::proc::Proc = core::ptr::null_mut();
    for (i, &(path, proc_nr)) in boot_procs.iter().enumerate() {
        let rp = kernel::table::proc_addr(proc_nr);
        if i == 0 {
            first_proc = rp;
        }

        let info = unsafe { boot_infos[i].assume_init_ref() };

        // Create a restricted page table that maps only this process's
        // code and stack, not the entire identity-mapped 1GB region.
        let pt_phys = unsafe {
            kernel_boot::boot_init::boot_create_restricted_page_table(
                info.code_start,
                info.code_end,
                info.phys_code_base,
                info.stack_start,
                info.stack_end,
                info.phys_stack_base,
            )
        };
        let pt_phys = match pt_phys {
            Some(p) => p,
            None => {
                serial_write("  FAILED: page table for ");
                serial_write(path);
                serial_write("\r\n");
                loop {
                    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                }
            }
        };

        unsafe {
            core::ptr::write_volatile(&raw mut (*rp).p_seg.p_cr3, pt_phys);
            // Set scheduling parameters.
            core::ptr::write_volatile(&raw mut (*rp).p_priority, 5i8);
            core::ptr::write_volatile(&raw mut (*rp).p_quantum_size_ms, 50u32);
            core::ptr::write_volatile(&raw mut (*rp).p_cpu_time_left, 50_000_000);
        }
    }

    if first_proc.is_null() {
        serial_write("  FAILED: no boot processes found\r\n");
        loop {
            unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
        }
    }

    // Send a boot notification to PM before starting the scheduler.
    // This notification will be pending when PM calls RECEIVE, causing
    // mini_receive to build a notification message and deliver it.
    unsafe {
        kernel::ipc::mini_notify(arch_common::com::RS_PROC_NR, arch_common::com::PM_PROC_NR);
    }

    serial_write("  enqueuing processes...\r\n");

    // Enqueue each process that is runnable.
    for &(_, proc_nr) in boot_procs {
        let rp = kernel::table::proc_addr(proc_nr);
        unsafe {
            let old_flags = (*rp)
                .p_rts_flags
                .load(core::sync::atomic::Ordering::Relaxed);
            let cleared = old_flags
                & !(kernel::proc::RtsFlags::BOOTINHIBIT.bits()
                    | kernel::proc::RtsFlags::SLOT_FREE.bits());
            if cleared == 0 {
                kernel::sched::enqueue(rp);
            }
        }
    }

    // Set the current process pointer to the first one.
    unsafe {
        arch_riscv64::cpulocals::set_current_proc(first_proc as u64);
    }

    serial_write("  scheduler starting...\r\n");

    // Dump page table entries for the first process
    unsafe {
        let rp = kernel::table::proc_addr(boot_procs[0].1);
        let cr3 = (*rp).p_seg.p_cr3;
        serial_write("  PT root: 0x");
        for i in (0..16).rev() {
            let nibble = ((cr3 >> (i * 4)) & 0xF) as u8;
            let hex = b"0123456789abcdef";
            arch_riscv64::sbi::console_putchar(hex[nibble as usize]);
        }
        arch_riscv64::sbi::console_putchar(b'\r');
        arch_riscv64::sbi::console_putchar(b'\n');
        // Dump root entries
        let root = cr3 as *const u64;
        for i in 0..4 {
            let entry = core::ptr::read_volatile(root.add(i));
            serial_write("    L2[");
            let hex = b"0123456789abcdef";
            arch_riscv64::sbi::console_putchar(hex[(i >> 4) as usize]);
            arch_riscv64::sbi::console_putchar(hex[(i & 0xF) as usize]);
            serial_write("]=0x");
            for j in (0..16).rev() {
                let nibble = ((entry >> (j * 4)) & 0xF) as u8;
                arch_riscv64::sbi::console_putchar(hex[nibble as usize]);
            }
            // Check if leaf (has R/W/X) or branch
            if entry & 0x0E != 0 {
                serial_write(" LEAF");
            } else if entry & 0x01 != 0 {
                serial_write(" BRANCH");
                // Dump L1 entries for this branch
                let pd_phys = ((entry & 0x003FFFFFFFFFFC00) >> 10) << 12;
                let pd = pd_phys as *const u64;
                for j in 0..8 {
                    let l1 = core::ptr::read_volatile(pd.add(j));
                    if l1 != 0 {
                        serial_write("\r\n      L1[");
                        arch_riscv64::sbi::console_putchar(hex[(j >> 4) as usize]);
                        arch_riscv64::sbi::console_putchar(hex[(j & 0xF) as usize]);
                        serial_write("]=0x");
                        for k in (0..16).rev() {
                            let nibble = ((l1 >> (k * 4)) & 0xF) as u8;
                            arch_riscv64::sbi::console_putchar(hex[nibble as usize]);
                        }
                        if l1 & 0x0E != 0 {
                            serial_write(" LEAF");
                        } else if l1 & 0x01 != 0 {
                            serial_write(" BRANCH");
                        }
                    }
                }
            }
            arch_riscv64::sbi::console_putchar(b'\r');
            arch_riscv64::sbi::console_putchar(b'\n');
        }
    }

    // TEST: Manually trigger a Supervisor Software Interrupt to test trap handler
    // BEFORE switching to userspace. If trap handler works, we'll see 'VPick the first process and switch to userspace.
    let next_proc = unsafe { kernel::sched::pick_proc() };
    let next_ptr = match next_proc {
        Some(p) => p,
        None => {
            serial_write("  FAILED: no runnable processes\r\n");
            loop {
                unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
            }
        }
    };

    // Write diagnostic character to confirm UART MMIO works
    unsafe {
        core::ptr::write_volatile(0x10000000usize as *mut u8, b'!');
    }

    serial_write("  switching to userspace...\r\n");
    unsafe {
        arch_riscv64::switch::switch_to_user(next_ptr as *const u8);
    }
}

/// Create an identity-mapped boot page table for SV39 paging.
///
/// Maps the full 4GB physical address space with 1GB huge pages.
/// This covers kernel code at 0x80200000, device memory (UART at 0x10000000,
/// PLIC at 0x0C000000, CLINT at 0x02000000), and all RAM.
///
/// Returns the physical address of the root page table.
///
/// # Safety
///
/// Must be called after the physical allocator is initialized, before any
/// virtual memory is active. The kernel must be running in Bare mode.
#[cfg(target_arch = "riscv64")]
unsafe fn create_boot_page_table() -> Option<u64> {
    unsafe {
        // Try to allocate from the physical allocator first.
        // If it fails (e.g., allocator not yet initialized), use a
        // hardcoded page in the gap between kernel data end and
        // the allocator-managed region.
        let root_phys = match arch_riscv64::alloc::alloc_phys_page() {
            Some(pa) => pa,
            None => {
                // Fallback: page at 0x8FF00000 is in RAM but outside
                // PMP protected ranges (0x80000000-0x8004FFFF).
                0x8FF00000u64
            }
        };
        core::ptr::write_bytes(root_phys as *mut u8, 0, 4096);

        // SV39 PTE flags for supervisor identity-mapped 1GB pages:
        // - V=1 (valid), R=1 (read), W=1 (write), X=1 (execute)
        // - No U bit (supervisor-only), no G bit
        let flags = arch_riscv64::pte::PTE_V
            | arch_riscv64::pte::PTE_R
            | arch_riscv64::pte::PTE_W
            | arch_riscv64::pte::PTE_X;

        // Map using 1GB huge pages at L2 level:
        // - L2[0]: VA 0x00000000-0x3FFFFFFF → PA 0x00000000 (covers devices, CLINT)
        // - L2[1]: VA 0x40000000-0x7FFFFFFF → PA 0x40000000
        // - L2[2]: VA 0x80000000-0xBFFFFFFF → PA 0x80000000 (covers RAM, kernel)
        // - L2[3]: VA 0xC0000000-0xFFFFFFFF → PA 0xC0000000
        let root = root_phys as *mut u64;
        for (i, base) in [0x00000000u64, 0x40000000, 0x80000000, 0xC0000000]
            .iter()
            .enumerate()
        {
            // build_pte encodes PPN = pa >> 12 correctly for SV39
            let pte = arch_riscv64::hal::build_pte(*base, flags);
            core::ptr::write(root.add(i), pte);
        }

        Some(root_phys)
    }
}

/// Panic handler.
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}
