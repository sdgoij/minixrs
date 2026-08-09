//! Kernel boot library — shared boot logic.
//!
//! Provides ELF loading, process initialization, serial output, and
//! integration testing infrastructure shared across architectures.
//!
//! Architecture-specific entry points (kmain, _start) live in `main.rs`
//! (x86_64) or future riscv64-specific binary crates.

#![no_std]

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

/// Print macro for boot-time serial output.
#[macro_export]
macro_rules! print {
    ($s:expr) => {
        $crate::serial_write($s);
    };
}
