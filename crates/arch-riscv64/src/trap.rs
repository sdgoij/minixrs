//! RISC-V64 trap handler — dispatches based on `scause`.
//!
//! Called from `trap_asm.S` after saving all registers.
//! Avoids direct dependency on kernel crate to prevent circular deps.
//! The kernel registers a syscall handler callback at init time.

/// Trap cause codes for RISC-V (scause register).
pub mod cause {
    pub const SUP_SW_INTR: u64 = 1;
    pub const SUP_TIMER_INTR: u64 = 5;
    pub const SUP_EXT_INTR: u64 = 9;
    pub const ECALL_UMODE: u64 = 8;
    pub const ECALL_SMODE: u64 = 9;
    pub const INSTR_PAGE_FAULT: u64 = 12;
    pub const LOAD_PAGE_FAULT: u64 = 13;
    pub const STORE_PAGE_FAULT: u64 = 15;
}

/// Check if a trap cause is an interrupt (MSB set).
pub fn is_interrupt(scause: u64) -> bool {
    scause & (1u64 << 63) != 0
}

/// Get the raw trap cause code (without the interrupt MSB).
pub fn cause_code(scause: u64) -> u64 {
    scause & !(1u64 << 63)
}

/// Registered syscall handler (set by kernel at init).
static mut SYSCALL_HANDLER: Option<unsafe fn(usize, &[u64; 6]) -> i64> = None;

/// Registered post-syscall hook (set by kernel at init).
static mut POST_SYSCALL_HOOK: Option<unsafe fn(&mut [u8; 296])> = None;

/// Registered UART input callback (set by kernel at init).
/// Called on each timer tick if a byte is available from UART.
static mut UART_INPUT_CALLBACK: Option<unsafe fn(u8)> = None;

/// Register the basic syscall dispatch function.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_syscall_handler(handler: unsafe fn(usize, &[u64; 6]) -> i64) {
    unsafe {
        SYSCALL_HANDLER = Some(handler);
    }
}

/// Register the post-syscall hook for process switching.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_post_syscall_hook(hook: unsafe fn(&mut [u8; 296])) {
    unsafe {
        POST_SYSCALL_HOOK = Some(hook);
    }
}

/// Register the UART input callback.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_uart_input_callback(cb: unsafe fn(u8)) {
    unsafe {
        UART_INPUT_CALLBACK = Some(cb);
    }
}

/// The main trap handler — called from trap_asm.S.
///
/// # Safety
///
/// Must only be called from the trap vector with interrupts disabled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trap_handler(frame: &mut [u8; 296]) {
    let scause_val = u64::from_ne_bytes(frame[272..280].try_into().unwrap());
    let code = cause_code(scause_val);
    let code = cause_code(scause_val);

    if is_interrupt(scause_val) {
        match code {
            cause::SUP_TIMER_INTR => {
                unsafe {
                    crate::clint::handle_timer_interrupt();
                    // Poll for console input via SBI (doesn't need page tables).
                    // Drain all available bytes.
                    if let Some(cb) = UART_INPUT_CALLBACK {
                        while let Some(byte) = crate::sbi::console_getchar() {
                            cb(byte);
                        }
                    }
                };
            }
            cause::SUP_EXT_INTR => {
                // External interrupt — claim and handle via PLIC
                unsafe {
                    let irq = crate::plic::claim_irq();
                    if irq != 0 {
                        // TODO: dispatch to registered handler based on IRQ
                        crate::plic::complete_irq(irq);
                    }
                }
            }
            _ => {}
        }
    } else {
        match code {
            cause::ECALL_UMODE => {
                let nr = u64::from_ne_bytes(frame[136..144].try_into().unwrap());
                let args = [
                    u64::from_ne_bytes(frame[80..88].try_into().unwrap()),
                    u64::from_ne_bytes(frame[88..96].try_into().unwrap()),
                    u64::from_ne_bytes(frame[96..104].try_into().unwrap()),
                    u64::from_ne_bytes(frame[104..112].try_into().unwrap()),
                    u64::from_ne_bytes(frame[112..120].try_into().unwrap()),
                    u64::from_ne_bytes(frame[120..128].try_into().unwrap()),
                ];
                let ret = match unsafe { SYSCALL_HANDLER } {
                    Some(handler) => unsafe { handler(nr as usize, &args) },
                    None => -38,
                };
                frame[80..88].copy_from_slice(&ret.to_ne_bytes());
                // Increment sepc by 4 to skip the ecall instruction.
                // On RISC-V, ecall sets sepc to the ecall instruction's
                // address (unlike x86_64 syscall which returns to the
                // instruction after syscall).
                let sepc = u64::from_ne_bytes(frame[256..264].try_into().unwrap());
                frame[256..264].copy_from_slice(&(sepc + 4).to_ne_bytes());
                // Post-syscall hook: if current process blocked (IPC), switch.
                if let Some(hook) = unsafe { POST_SYSCALL_HOOK } {
                    unsafe { hook(frame) };
                }
            }
            cause::INSTR_PAGE_FAULT | cause::LOAD_PAGE_FAULT | cause::STORE_PAGE_FAULT => {
                let stval: u64;
                unsafe {
                    core::arch::asm!("csrr {v}, stval", v = out(reg) stval, options(nomem, nostack))
                };
                let sepc = u64::from_ne_bytes(frame[256..264].try_into().unwrap());
                // Use UART MMIO for diagnostics
                unsafe {
                    core::ptr::write_volatile(0x10000000usize as *mut u8, b'!');
                }
                unsafe {
                    core::ptr::write_volatile(0x10000000usize as *mut u8, b'P');
                }
                unsafe {
                    core::ptr::write_volatile(0x10000000usize as *mut u8, b'F');
                }
                unsafe {
                    core::ptr::write_volatile(0x10000000usize as *mut u8, b' ');
                }
                // Print stval and sepc as hex via UART MMIO
                unsafe {
                    let hex = b"0123456789abcdef";
                    for i in (0..16).rev() {
                        let nibble = ((stval >> (i * 4)) & 0xF) as usize;
                        core::ptr::write_volatile(0x10000000usize as *mut u8, hex[nibble]);
                    }
                    core::ptr::write_volatile(0x10000000usize as *mut u8, b' ');
                    for i in (0..16).rev() {
                        let nibble = ((sepc >> (i * 4)) & 0xF) as usize;
                        core::ptr::write_volatile(0x10000000usize as *mut u8, hex[nibble]);
                    }
                    core::ptr::write_volatile(0x10000000usize as *mut u8, b'\r');
                    core::ptr::write_volatile(0x10000000usize as *mut u8, b'\n');
                }
                loop {
                    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                }
            }
            _ => loop {
                unsafe {
                    core::arch::asm!("wfi", options(nomem, nostack));
                }
            },
        }
    }
}
