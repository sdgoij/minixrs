//! Kernel boot library — shared boot logic.
//!
//! Provides ELF loading, process initialization, serial output, and
//! integration testing infrastructure shared across architectures.
//!
//! Architecture-specific entry points (kmain, _start) live in `main.rs`
//! (x86_64) or future riscv64-specific binary crates.

#![no_std]

#[cfg(target_arch = "x86_64")]
pub mod boot_init;

#[cfg(all(feature = "integration-tests", target_arch = "x86_64"))]
pub mod test_runner;

// ── Boot-time serial output (x86_64 COM1 via port I/O) ────────────────

/// Write a string to COM1 serial port. No-op in test mode.
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
    #[cfg(not(all(not(test), target_arch = "x86_64")))]
    let _ = s;
}

/// Write a single byte to COM1 serial port. No-op in test mode.
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
    #[cfg(not(all(not(test), target_arch = "x86_64")))]
    let _ = c;
}

/// Print macro for boot-time serial output.
#[macro_export]
macro_rules! print {
    ($s:expr) => {
        $crate::serial_write($s);
    };
}
