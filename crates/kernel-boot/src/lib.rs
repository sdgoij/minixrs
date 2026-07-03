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

// ── Boot-time serial output (x86_64 COM1 via port I/O) ────────────────

/// Write a string to the boot console.
///
/// On x86_64: COM1 serial port via port I/O.
/// On RISC-V: SBI debug console.
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
    #[cfg(any(test, not(any(target_arch = "x86_64", target_arch = "riscv64"))))]
    let _ = s;
}

/// Write a single byte to the boot console.
///
/// On x86_64: COM1 serial port via port I/O.
/// On RISC-V: SBI debug console.
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
    #[cfg(any(test, not(any(target_arch = "x86_64", target_arch = "riscv64"))))]
    let _ = c;
}

/// Print macro for boot-time serial output.
#[macro_export]
macro_rules! print {
    ($s:expr) => {
        $crate::serial_write($s);
    };
}
