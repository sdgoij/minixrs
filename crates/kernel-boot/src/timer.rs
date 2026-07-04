//! Timer interrupt support — PIT (8253) programming and handler.

use core::arch::naked_asm;

// PIT (8253/8254) constants

/// PIT base frequency (1.193182 MHz).
const PIT_FREQ: u32 = 1_193_182;

// State

/// Timer interrupt handler called from the assembly stub.
///
/// Increments the kernel's tick counter, advances the clock,
/// drives preemptive scheduling via `scheduler_tick()`, and
/// sends EOI to the APIC (or PIC fallback).
extern "C" fn timer_handler(_vector: u64) {
    unsafe {
        kernel::clock::TICK_COUNT = kernel::clock::TICK_COUNT.wrapping_add(1);
        // Advance the kernel's monotonic/realtime clock
        kernel::clock::tick();

        // Drive preemptive scheduling if there's a current process.
        let current = arch_x86_64::arch_syscall::current_proc();
        if !current.is_null() {
            let next = kernel::sched::schedule::scheduler_tick(current);
            if next != current && !next.is_null() {
                arch_x86_64::arch_syscall::set_current_proc(next);
                // Context-switch to the next process.
                arch_x86_64::asm::switch(current, next);
            }
        }

        // Acknowledge APIC (or PIC fallback)
        arch_x86_64::apic::eoi();
    }
}

/// Initialize the PIT channel 0 for the given frequency.
///
/// # Arguments
/// * `hz` — desired frequency in Hz (typically 100 for scheduler).
pub fn init_pit(hz: u32) {
    let divisor = (PIT_FREQ + hz / 2) / hz;
    let low = (divisor & 0xFF) as u8;
    let high = ((divisor >> 8) & 0xFF) as u8;
    unsafe {
        core::arch::asm!(
            "mov al, 0x36",
            "out 0x43, al",
            options(nomem, nostack, preserves_flags),
        );
        core::arch::asm!(
            "out 0x40, al",
            in("al") low,
            options(nomem, nostack, preserves_flags),
        );
        core::arch::asm!(
            "out 0x40, al",
            in("al") high,
            options(nomem, nostack, preserves_flags),
        );
    }

    // Unmask the I/O APIC redirection entry for the PIT (IRQ 0).
    // The entry was created masked by ioapic_redirect_irq() to prevent
    // premature interrupts before page tables are ready.
    unsafe {
        arch_x86_64::apic::ioapic_unmask(0);
    }
}

/// Assembly interrupt stub for the timer (IRQ0 → vector 32).
///
/// Saves all general-purpose registers, calls `timer_handler`,
/// acknowledges the PIC, restores registers, and returns via `iretq`.
#[unsafe(no_mangle)]
#[unsafe(naked)]
pub unsafe extern "C" fn timer_interrupt_entry() {
    naked_asm!(
        ".code64",
        // Save all GPRs
        "push rax",
        "push rcx",
        "push rdx",
        "push rbx",
        "push rbp",
        "push rsi",
        "push rdi",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        // Call timer_handler
        "mov edi, 32",
        "call {handler}",
        // Restore all GPRs
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rdi",
        "pop rsi",
        "pop rbp",
        "pop rbx",
        "pop rdx",
        "pop rcx",
        "pop rax",
        // Return from interrupt
        "iretq",
        handler = sym timer_handler,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pit_freq_constant() {
        assert_eq!(PIT_FREQ, 1_193_182);
    }

    #[test]
    fn pit_divisor_at_100hz() {
        // At 100 Hz: (1193182 + 50) / 100 = 11932
        // This tests the rounding logic in init_pit's divisor calculation.
        let hz = 100;
        let divisor = (PIT_FREQ + hz / 2) / hz;
        assert_eq!(divisor, 11932);
        let low = (divisor & 0xFF) as u8;
        let high = ((divisor >> 8) & 0xFF) as u8;
        assert_eq!(low, (11932 & 0xFF) as u8);
        assert_eq!(high, ((11932 >> 8) & 0xFF) as u8);
    }

    #[test]
    fn pit_divisor_at_1000hz() {
        // At 1000 Hz: (1193182 + 500) / 1000 = 1193
        let hz = 1000;
        let divisor = (PIT_FREQ + hz / 2) / hz;
        assert_eq!(divisor, 1193);
    }

    #[test]
    fn pit_divisor_at_50hz() {
        // At 50 Hz: (1193182 + 25) / 50 = 23864
        let hz = 50;
        let divisor = (PIT_FREQ + hz / 2) / hz;
        assert_eq!(divisor, 23864);
    }

    #[test]
    fn timer_handler_signature() {
        // Compile-time check: timer_handler must be callable as extern "C" with u64 arg
        let _: extern "C" fn(u64) = timer_handler;
    }

    #[test]
    fn timer_interrupt_entry_exists() {
        // Verify the naked function symbol is present (compile-time check)
        let _: unsafe extern "C" fn() = timer_interrupt_entry;
    }
}
