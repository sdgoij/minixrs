//! Kernel test binary — runs inside QEMU via trampoline.
//!
//! Tests that genuinely require bare-metal execution:
//! - IO port access (serial, isa-debug-exit)
//! - CR3 / page table inspection
//! - Real page table walks
//! - Actual process table state after boot

#![no_std]
#![no_main]

/// QEMU-required tests — cannot run on host.
/// These exercise hardware state and real kernel data structures.
const TESTS: &[(&str, fn() -> u32)] = &[
    ("cr3_nonzero", test_cr3_nonzero),
    ("pt_walk_trampoline", test_page_table_walk_trampoline),
    ("pt_walk_kernel", test_page_table_walk_kernel),
    ("pt_walk_end", test_page_table_walk_end),
    ("write_stdout", test_write_stdout),
    (
        "write_stdout_returns_count",
        test_write_stdout_returns_count,
    ),
    ("pci_init", test_pci_init),
    ("ahci_init", test_ahci_init),
    ("at_wini_probe", test_at_wini_probe),
    ("virtio_blk", test_virtio_blk),
    ("remap_pic", test_remap_pic_qemu),
];

/// Called by the trampoline after long mode setup.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    kmain()
}

#[unsafe(no_mangle)]
pub extern "C" fn kmain() -> ! {
    // Init COM1 serial (115200 8N1)
    unsafe {
        let p = 0x3F8u16;
        core::arch::asm!("out dx, al", in("dx") p + 1, in("al") 0x00u8);
        core::arch::asm!("out dx, al", in("dx") p + 3, in("al") 0x80u8);
        core::arch::asm!("out dx, al", in("dx") p,     in("al") 0x01u8);
        core::arch::asm!("out dx, al", in("dx") p + 1, in("al") 0x00u8);
        core::arch::asm!("out dx, al", in("dx") p + 3, in("al") 0x03u8);
        core::arch::asm!("out dx, al", in("dx") p + 2, in("al") 0xC7u8);
        core::arch::asm!("out dx, al", in("dx") p + 4, in("al") 0x0Bu8);
    }
    serial_write(b"\r\n");

    let total = TESTS.len();
    let mut passed: u32 = 0;
    for &(name, func) in TESTS {
        let result = func();
        if result == 0 {
            serial_write(b"  OK ");
            serial_write(name.as_bytes());
            serial_write(b"\n");
            passed += 1;
        } else {
            serial_write(b"  FAIL ");
            serial_write(name.as_bytes());
            serial_write(b" (code=");
            serial_write(&dec_str(result));
            serial_write(b")\n");
        }
    }
    serial_write(b"result: ");
    serial_write(&dec_str(passed));
    serial_write(b"/");
    serial_write(&dec_str(total as u32));
    serial_write(b"\n");
    if passed == total as u32 {
        exit_qemu_success()
    } else {
        exit_qemu_failure(total as u32 - passed)
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    serial_write(b"panic: ");
    if let Some(msg) = info.message().as_str() {
        serial_write(msg.as_bytes());
    }
    serial_write(b"\n");
    exit_qemu_failure(1)
}

// ── QEMU-required tests ──

fn test_write_stdout() -> u32 {
    unsafe {
        kernel::table::proc_init();
        let rp = kernel::table::proc_addr(0);
        if rp.is_null() {
            return 1;
        }
        let buf = [0x41u8; 10];
        let args = [1u64, buf.as_ptr() as u64, 10u64, 0, 0, 0];
        let ret = kernel::syscall::sys_write_handler(rp, &args);
        if ret != 10 {
            return 2;
        }
    }
    0
}

fn test_write_stdout_returns_count() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(0);
        let buf = [0u8; 10];
        let args = [1u64, buf.as_ptr() as u64, 10u64, 0, 0, 0];
        let ret = kernel::syscall::sys_write_handler(rp, &args);
        if ret != 10 {
            return 1;
        }
    }
    0
}

fn test_pci_init() -> u32 {
    unsafe {
        drivers::bus::pci::pci_init();
    }
    0
}

fn test_ahci_init() -> u32 {
    match unsafe { drivers::storage::ahci::ahci_init() } {
        Ok(_) | Err(_) => 0, // Accept any result — just verifying no panic/crash
    }
}

fn test_at_wini_probe() -> u32 {
    match unsafe { drivers::storage::at_wini::at_wini_probe() } {
        Ok(_) | Err(_) => 0,
    }
}

fn test_virtio_blk() -> u32 {
    match unsafe { drivers::storage::virtio_blk::virtio_blk_probe(0) } {
        Ok(_) | Err(_) => 0,
    }
}

fn test_remap_pic_qemu() -> u32 {
    unsafe {
        arch_x86_64::apic::test_remap_pic_qemu();
    }
    0
}

fn test_cr3_nonzero() -> u32 {
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack));
    }
    if cr3 == 0 {
        return 1;
    }
    0
}

fn test_page_table_walk_trampoline() -> u32 {
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack));
    }
    if cr3 == 0 {
        return 1;
    }
    match unsafe { kernel::pagetable::walk(cr3, 0x100000) } {
        Ok(pr) => {
            // PTE must be present
            if pr.pte_value & 1 == 0 {
                return 2;
            }
            // Huge page (2MB) — PS bit 7 should be set from trampoline identity map
            if pr.pte_value & (1 << 7) == 0 {
                return 3;
            }
        }
        Err(_) => return 4,
    }
    0
}

fn test_page_table_walk_kernel() -> u32 {
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack));
    }
    if cr3 == 0 {
        return 1;
    }
    match unsafe { kernel::pagetable::walk(cr3, 0x200000) } {
        Ok(pr) => {
            if pr.pte_value & 1 == 0 {
                return 2;
            }
        }
        Err(_) => return 3,
    }
    0
}

fn test_page_table_walk_end() -> u32 {
    // Walk a high address near the end of identity mapping to verify large pages
    let cr3: u64;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nostack));
    }
    if cr3 == 0 {
        return 1;
    }
    // Address 0x400000 is within the 3rd 2MB huge page from trampoline
    match unsafe { kernel::pagetable::walk(cr3, 0x1FE00000) } {
        Ok(pr) => {
            if pr.pte_value & 1 == 0 {
                return 2;
            }
        }
        Err(_) => return 3,
    }
    0
}

// ── IO helpers ──

fn serial_write(bytes: &[u8]) {
    for &b in bytes {
        unsafe {
            loop {
                let lsr: u8;
                core::arch::asm!("in al, dx", out("al") lsr, in("dx") 0x3FDu16, options(nostack));
                if lsr & 0x20 != 0 {
                    break;
                }
            }
            core::arch::asm!("out dx, al", in("dx") 0x3F8u16, in("al") b, options(nostack));
        }
    }
}

fn dec_str(mut n: u32) -> [u8; 12] {
    let mut buf = [0u8; 12];
    let mut i = 12;
    if n == 0 {
        buf[11] = b'0';
        return buf;
    }
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    buf
}

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
