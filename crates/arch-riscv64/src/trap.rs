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

/// Register the basic syscall dispatch function.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_syscall_handler(handler: unsafe fn(usize, &[u64; 6]) -> i64) {
    SYSCALL_HANDLER = Some(handler);
}

/// The main trap handler — called from trap_asm.S.
///
/// # Safety
///
/// Must only be called from the trap vector with interrupts disabled.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn trap_handler(frame: &mut [u8; 288]) {
    let scause_val = u64::from_ne_bytes(frame[272..280].try_into().unwrap());
    let code = cause_code(scause_val);

    if is_interrupt(scause_val) {
        match code {
            cause::SUP_TIMER_INTR => {
                unsafe { crate::clint::handle_timer_interrupt() };
            }
            cause::SUP_EXT_INTR => {}
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
                let ret = match SYSCALL_HANDLER {
                    Some(handler) => handler(nr as usize, &args),
                    None => -38,
                };
                frame[80..88].copy_from_slice(&ret.to_ne_bytes());
            }
            cause::INSTR_PAGE_FAULT | cause::LOAD_PAGE_FAULT | cause::STORE_PAGE_FAULT => {}
            _ => loop {
                unsafe {
                    core::arch::asm!("wfi", options(nomem, nostack));
                }
            },
        }
    }
}
