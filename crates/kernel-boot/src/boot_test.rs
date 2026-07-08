//! QEMU boot integration test — verifies boot sequence and server IPC.
//!
//! VFS calls SYS_BOOT_COMPLETE (syscall 60) after mount_root succeeds.
//! The kernel handler runs assertions, then exits QEMU via isa-debug-exit.
//!
//! Gated behind cfg(feature = "boot-test") — no impact on normal builds.

use arch_common::com::{MFS_PROC_NR, PM_PROC_NR, VFS_PROC_NR};

const FS_BASE: i32 = 0xA00;
const REQ_READSUPER: i32 = FS_BASE + 28;

/// Run all boot tests, then exit QEMU with success/failure.
///
/// # Safety
///
/// Must be called from the SYS_BOOT_COMPLETE syscall handler after VFS
/// has finished mount_root and all boot processes are initialized.
/// Requires that the kernel-allocator, process table, IPC, and per-process
/// page tables are fully set up. The function never returns — it exits QEMU.
pub unsafe fn run_boot_tests() -> ! {
    serial_write("\r\n=== BOOT TEST ===\r\n");
    let mut failures: u32 = 0;

    // A: Server liveness
    failures += test_alive(VFS_PROC_NR, "VFS");
    failures += test_alive(MFS_PROC_NR, "MFS");
    failures += test_alive(PM_PROC_NR, "PM");

    // B: Process state
    failures += test_vfs_runnable();
    failures += test_mfs_post_readsuper();
    failures += test_pm_idle();

    // C: VFS→MFS IPC (request in VFS's sendmsg)
    failures += test_vfs_sent_readsuper();

    // D: MFS→VFS IPC (reply in VFS's delivermsg)
    failures += test_vfs_reply_from_mfs();
    failures += test_vfs_reply_root_inode();
    failures += test_vfs_reply_file_size();

    // E: Grant table registration
    failures += test_grant_registered();

    // F: VM page table walk (required for safe copy)
    failures += test_vm_check_range();

    // G: PM notification
    failures += test_pm_has_message();

    // H: Physical memory allocator — kernel binary excluded from free pool
    failures += test_allocator_no_kernel_overlap();
    failures += test_allocator_has_free_pages();

    if failures == 0 {
        serial_write("ALL TESTS PASSED\r\n");
        exit_qemu_success();
    } else {
        serial_write("FAILURES: ");
        print_dec(failures);
        serial_write("\r\n");
        exit_qemu_failure(failures);
    }
}

fn rdi(msg: *const u8, off: usize) -> i32 {
    unsafe { core::ptr::read_unaligned(msg.add(off) as *const i32) }
}
fn rdu(msg: *const u8, off: usize) -> u32 {
    unsafe { core::ptr::read_unaligned(msg.add(off) as *const u32) }
}

// A: Liveness

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
            serial_write("  FAIL: MFS not on RECEIVE\r\n");
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
            serial_write("  FAIL: PM not on RECEIVE\r\n");
            return 1;
        }
        serial_write("  OK PM idle\r\n");
    }
    0
}

// C: Did VFS send REQ_READSUPER to MFS?
// We can't read MFS's user buffer (it's in MFS's address space),
// but we CAN read VFS's p_sendmsg which held the outgoing request.
// After the SENDREC completed, p_sendmsg was NOT cleared.

fn test_vfs_sent_readsuper() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        // VFS's outgoing request is in p_sendmsg.
        // m_type at offset 4 should be REQ_READSUPER = 0xA00 + 28 = 2588
        let ty = rdi((*rp).p_sendmsg.as_ptr(), 4);
        if ty != REQ_READSUPER {
            serial_write("  FAIL: VFS send type=");
            print_dec(ty as u32);
            serial_write(" expected 2588\r\n");
            return 1;
        }
        serial_write("  OK VFS sent REQ_READSUPER to MFS\r\n");
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
        let st = rdi(msg, 4); // m_type (status)
        if src != MFS_PROC_NR {
            serial_write("  FAIL: reply src=");
            print_dec(src as u32);
            serial_write(" expected 7\r\n");
            return 1;
        }
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

fn test_vfs_reply_root_inode() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let msg = (*rp).p_delivermsg.as_ptr();
        // fs_readsuper reply payload (+8 from msg base):
        //   +12: inode_nr (u32) @ byte 20
        //   +8:  dev (u32)      @ byte 16
        let ino = rdu(msg, 20);
        let dev = rdu(msg, 16);
        if ino != 1 {
            serial_write("  FAIL: inode_nr=");
            print_dec(ino);
            serial_write(" expected 1\r\n");
            return 1;
        }
        serial_write("  OK root inode=1 dev=");
        print_dec(dev);
        serial_write("\r\n");
    }
    0
}

fn test_vfs_reply_file_size() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(VFS_PROC_NR);
        if rp.is_null() {
            return 1;
        }
        let msg = (*rp).p_delivermsg.as_ptr();
        // file_size (i64) at payload+0 = msg byte 8, spans bytes 8-15
        let low = rdu(msg, 8);
        let high = rdu(msg, 12);
        let size = (low as u64) | ((high as u64) << 32);
        if size == 0 {
            serial_write("  FAIL: root file_size=0 (empty root?)\r\n");
            return 1;
        }
        serial_write("  OK root dir size=");
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
        if kernel::pagetable::walk(cr3, 0x01000000).is_err() {
            serial_write("  VM: code page FAIL\r\n");
            return 1;
        }
        if kernel::pagetable::walk(cr3, 0x01010000).is_err() {
            serial_write("  VM: BSS page FAIL\r\n");
            return 1;
        }
        if kernel::pagetable::walk(cr3, 0x0FE00000).is_err() {
            serial_write("  VM: stack page FAIL\r\n");
            return 1;
        }
        serial_write("  OK VM: pages mapped\r\n");
        // Test CR3 switches: switch to each process's CR3 and back.
        // If kernel higher-half isn't mapped in per-process page tables,
        // write_cr3 will cause a triple fault.
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
    0
}

// H: Physical memory allocator

// Linker symbol: byte just past the end of the kernel binary.
// Same extern as in main.rs — the boot test runs in the same binary.
unsafe extern "C" {
    static __kernel_end: u8;
}

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

// Exit helpers

fn exit_qemu_success() -> ! {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0x501u16, in("eax") 0u32);
    }
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nostack));
        }
    }
}
fn exit_qemu_failure(f: u32) -> ! {
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") 0x501u16, in("eax") (f << 1 | 1));
    }
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nostack));
        }
    }
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
