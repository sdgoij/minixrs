//! QEMU isa-debug-exit device integration for integration tests.
//!
//! Port: 0x501 (default for `-device isa-debug-exit`).
//! QEMU computes `exit_code = (val << 1) | 1` internally (see debugexit.c).
#[allow(dead_code)]
const QEMU_EXIT_PORT: u16 = 0x501;

/// Write 0 → QEMU exits with code 1 → runner interprets as success.
#[allow(dead_code)]
pub fn qemu_exit_success() -> ! {
    unsafe {
        arch_x86_64::common::outw(QEMU_EXIT_PORT, 0u16);
    }
    loop {
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

/// Write `failures` → QEMU exits with `(failures << 1) | 1`.
#[allow(dead_code)]
pub fn qemu_exit_fail(failures: u16) -> ! {
    unsafe {
        arch_x86_64::common::outw(QEMU_EXIT_PORT, failures);
    }
    loop {
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}
