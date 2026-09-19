//! wasm32 platform layer — the analogue of `kernel-boot` at the host boundary.
//!
//! `kernel-boot` owns multiboot parsing, the trampoline, BSS clearing, and ELF
//! process loading. None of that survives here: the host owns memory and module
//! loading, and M1 has no processes at all. What remains is the same division of
//! labour as every other arch — this layer owns the entry points and the panic
//! handler, and the kernel below it is unchanged.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kernel::panic::handle(info)
}

/// The kernel has no printing facility of its own — every arch prints from its
/// platform layer — so a banner goes out through the HAL, which here is a host
/// import.
fn print(s: &str) {
    for byte in s.bytes() {
        arch_wasm32::hal::serial_write_byte(byte);
    }
}

/// Bring the kernel up. The host calls this once, after instantiation.
#[unsafe(no_mangle)]
pub extern "C" fn minix_kernel_init() {
    kernel::init();
    // `panic::handle` inspects per-CPU state, which is only safe to read once
    // `init_cpulocals` has run; without this it must skip that part.
    kernel::panic::mark_cpulocals_ready();

    print("Hello MINIX!\r\n");
    print("kernel: wasm32 instance initialised\r\n");
}

/// Deliberately panic, so the host can verify the diagnostic path end to end:
/// the message and location reach the console, and the halt path traps.
#[unsafe(no_mangle)]
pub extern "C" fn minix_kernel_trigger_panic() {
    panic!("deliberate M1 panic");
}
