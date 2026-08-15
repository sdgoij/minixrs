//! Kernel boot library — shared boot logic.
//!
//! Provides ELF loading, process initialization, serial output, and
//! integration testing infrastructure shared across architectures.
//!
//! Architecture-specific entry points (kmain, _start) live in `main.rs`
//! (x86_64) or future riscv64-specific binary crates.

#![no_std]

use arch_common::com::{
    DEVMAN_PROC_NR, DS_PROC_NR, FB_PROC_NR, INIT_PROC_NR, INPUT_PROC_NR, MFS_PROC_NR, NET_PROC_NR,
    PFS_PROC_NR, PM_PROC_NR, RAMDISK_PROC_NR, RS_PROC_NR, SCHED_PROC_NR, TTY_PROC_NR, VFS_PROC_NR,
    VIRTIO_BLK_PROC_NR, VIRTIO_NET_PROC_NR, VM_PROC_NR,
};

pub mod boot_init;

#[cfg(all(feature = "integration-tests", target_arch = "x86_64"))]
pub mod test_runner;

#[cfg(feature = "boot-test")]
pub mod boot_test;

#[cfg(feature = "boot-test")]
pub unsafe fn boot_test_syscall_handler(_caller: *mut kernel::proc::Proc, _args: &[u64; 6]) -> i64 {
    unsafe { boot_test::run_boot_tests() }
}

/// Non-boot-test fallback for SYS_BOOT_COMPLETE: VFS signals boot finished.
///
/// # Safety
///
/// Must only be invoked from the kernel's syscall dispatch path; the
/// caller/args are ignored.
#[cfg(not(feature = "boot-test"))]
pub unsafe fn boot_test_syscall_handler(_caller: *mut kernel::proc::Proc, _args: &[u64; 6]) -> i64 {
    0 // OK — VFS calls SYS_BOOT_COMPLETE to signal boot finished
}

/// Write a string to the boot console.
///
/// On x86_64: COM1 serial port via port I/O.
/// On RISC-V: SBI debug console.
/// On AArch64: PL011 UART (QEMU virt, 0x09000000).
pub fn serial_write(s: &str) {
    #[cfg(all(not(test), target_arch = "x86_64"))]
    {
        let port = 0x3F8u16;
        for &b in s.as_bytes() {
            unsafe {
                loop {
                    let lsr: u8;
                    core::arch::asm!(
                        "in al, dx",
                        out("al") lsr,
                        in("dx") port + 5,
                        options(nomem, nostack),
                    );
                    if lsr & 0x20 != 0 {
                        break;
                    }
                }
                core::arch::asm!("out dx, al", in("dx") port, in("al") b, options(nomem, nostack));
            }
        }
    }
    #[cfg(all(not(test), target_arch = "riscv64"))]
    {
        for &b in s.as_bytes() {
            arch_riscv64::sbi::console_putchar(b);
        }
    }
    #[cfg(all(not(test), target_arch = "aarch64"))]
    {
        const UART_DR: usize = 0x0900_0000;
        const UART_FR: usize = 0x0900_0000 + 0x18;
        const FR_TXFF: u32 = 1 << 5;
        for &b in s.as_bytes() {
            unsafe {
                while (core::ptr::read_volatile(UART_FR as *const u32) & FR_TXFF) != 0 {
                    core::hint::spin_loop();
                }
                core::ptr::write_volatile(UART_DR as *mut u32, b as u32);
            }
        }
    }
    #[cfg(any(
        test,
        not(any(
            target_arch = "x86_64",
            target_arch = "riscv64",
            target_arch = "aarch64"
        ))
    ))]
    let _ = s;
}

/// Write an unsigned integer in decimal to the boot console.
pub fn serial_write_u64_dec(mut v: u64) {
    let mut digits = [0u8; 20];
    let mut n = 0;
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        v /= 10;
        n += 1;
        if v == 0 {
            break;
        }
    }
    let mut s = [0u8; 20];
    for i in 0..n {
        s[i] = digits[n - 1 - i];
    }
    // SAFETY: all bytes are ASCII digits.
    serial_write(unsafe { core::str::from_utf8_unchecked(&s[..n]) });
}

/// Print the boot memory banner: the detected guest RAM total and the
/// usable (allocatable) amount, both in MiB.
pub fn print_memory_banner(detected: u64, usable: u64) {
    serial_write("memory: ");
    serial_write_u64_dec(detected / (1024 * 1024));
    serial_write(" MiB detected (");
    serial_write_u64_dec(usable / (1024 * 1024));
    serial_write(" MiB usable)\r\n");
}

/// Print a fatal boot error to the boot console, then halt the CPU forever.
///
/// The halt mechanism is arch-specific: `hlt` on x86_64, and the arch
/// `hal::halt()` (a `wfi` loop) on AArch64 and RISC-V. Never returns.
pub fn boot_abort(msg: &str) -> ! {
    serial_write("  FAILED: ");
    serial_write(msg);
    serial_write("\r\n");
    #[cfg(all(not(test), target_arch = "x86_64"))]
    loop {
        unsafe {
            core::arch::asm!("hlt", options(nomem, nostack));
        }
    }
    #[cfg(all(not(test), target_arch = "aarch64"))]
    {
        arch_aarch64::hal::halt()
    }
    #[cfg(all(not(test), target_arch = "riscv64"))]
    {
        arch_riscv64::hal::halt()
    }
    #[cfg(any(
        test,
        not(any(
            target_arch = "x86_64",
            target_arch = "aarch64",
            target_arch = "riscv64"
        ))
    ))]
    loop {
        core::hint::spin_loop();
    }
}

/// Write a single byte to the boot console.
///
/// On x86_64: COM1 serial port via port I/O.
/// On RISC-V: SBI debug console.
/// On AArch64: PL011 UART (QEMU virt, 0x09000000).
pub fn serial_putc(c: u8) {
    #[cfg(all(not(test), target_arch = "x86_64"))]
    {
        let port = 0x3F8u16;
        unsafe {
            loop {
                let lsr: u8;
                core::arch::asm!(
                    "in al, dx",
                    out("al") lsr,
                    in("dx") port + 5,
                    options(nomem, nostack),
                );
                if lsr & 0x20 != 0 {
                    break;
                }
            }
            core::arch::asm!("out dx, al", in("dx") port, in("al") c, options(nomem, nostack));
        }
    }
    #[cfg(all(not(test), target_arch = "riscv64"))]
    {
        arch_riscv64::sbi::console_putchar(c);
    }
    #[cfg(all(not(test), target_arch = "aarch64"))]
    {
        const UART_DR: usize = 0x0900_0000;
        const UART_FR: usize = 0x0900_0000 + 0x18;
        const FR_TXFF: u32 = 1 << 5;
        unsafe {
            while (core::ptr::read_volatile(UART_FR as *const u32) & FR_TXFF) != 0 {
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(UART_DR as *mut u32, c as u32);
        }
    }
    #[cfg(any(
        test,
        not(any(
            target_arch = "x86_64",
            target_arch = "riscv64",
            target_arch = "aarch64"
        ))
    ))]
    let _ = c;
}

/// Boot processes in startup order: (initramfs path, endpoint number).
///
/// Order matters: VFS must come before MFS so VFS's SENDREC is queued and
/// processed when MFS later runs, and VM must come before any process
/// that calls brk() (MFS, PFS, TTY, INIT). VM is loaded right after VFS,
/// ahead of the device drivers and the brk()-using servers; all three
/// arches share this single list.
static BOOT_PROCS_ALL: &[(&str, i32)] = &[
    ("/sbin/ds", DS_PROC_NR),           // Data Store (first, matches C order)
    ("/sbin/rs", RS_PROC_NR),           // Reincarnation Server
    ("/sbin/pm", PM_PROC_NR),           // Process Manager
    ("/sbin/sched", SCHED_PROC_NR),     // Scheduler
    ("/sbin/vfs", VFS_PROC_NR),         // Virtual File System
    ("/sbin/vm", VM_PROC_NR),           // Virtual Memory
    ("/sbin/ramdisk", RAMDISK_PROC_NR), // RAM disk block driver
    ("/sbin/virtio_blk", VIRTIO_BLK_PROC_NR), // virtio-blk disk driver
    ("/sbin/virtio_net", VIRTIO_NET_PROC_NR), // virtio-net NIC driver
    ("/sbin/net", NET_PROC_NR),         // network server (ARP/ICMP)
    ("/sbin/mfs", MFS_PROC_NR),         // Minix File System
    ("/sbin/pfs", PFS_PROC_NR),         // Pipe File System
    ("/sbin/devman", DEVMAN_PROC_NR),   // device manager (VTreeFS tree)
    ("/sbin/tty", TTY_PROC_NR),         // Terminal driver
    ("/sbin/fb", FB_PROC_NR),           // framebuffer driver
    ("/sbin/input", INPUT_PROC_NR),     // PS/2 keyboard driver
    ("/sbin/init", INIT_PROC_NR),       // init
];

/// The boot process list for this build: the full list, or — under the
/// boot-test feature — the full list minus the trailing INIT entry, so
/// the boot-complete test runs before any user process starts.
pub fn boot_procs() -> &'static [(&'static str, i32)] {
    #[cfg(feature = "boot-test")]
    {
        &BOOT_PROCS_ALL[..BOOT_PROCS_ALL.len() - 1]
    }
    #[cfg(not(feature = "boot-test"))]
    {
        BOOT_PROCS_ALL
    }
}

/// Print macro for boot-time serial output.
#[macro_export]
macro_rules! print {
    ($s:expr) => {
        $crate::serial_write($s);
    };
}
