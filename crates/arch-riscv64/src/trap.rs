//! RISC-V64 trap handler — dispatches based on `scause`.
//!
//! Called from `trap_asm.S` after saving all registers.
//! Avoids direct dependency on kernel crate to prevent circular deps.
//! The kernel registers a syscall handler callback at init time.

use core::cell::UnsafeCell;

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

struct SyscallHandlerCell(UnsafeCell<Option<unsafe fn(usize, &[u64; 6]) -> i64>>);
unsafe impl Sync for SyscallHandlerCell {}
impl SyscallHandlerCell {
    const fn new(val: Option<unsafe fn(usize, &[u64; 6]) -> i64>) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut Option<unsafe fn(usize, &[u64; 6]) -> i64> {
        self.0.get()
    }
}

struct PostSyscallHookCell(UnsafeCell<Option<unsafe fn(&mut [u8; 296])>>);
unsafe impl Sync for PostSyscallHookCell {}
impl PostSyscallHookCell {
    const fn new(val: Option<unsafe fn(&mut [u8; 296])>) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut Option<unsafe fn(&mut [u8; 296])> {
        self.0.get()
    }
}

struct UartInputCallbackCell(UnsafeCell<Option<unsafe fn(u8)>>);
unsafe impl Sync for UartInputCallbackCell {}
impl UartInputCallbackCell {
    const fn new(val: Option<unsafe fn(u8)>) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut Option<unsafe fn(u8)> {
        self.0.get()
    }
}

struct TimerCallbackCell(UnsafeCell<Option<unsafe fn(&mut [u8; 296])>>);
unsafe impl Sync for TimerCallbackCell {}
impl TimerCallbackCell {
    const fn new(val: Option<unsafe fn(&mut [u8; 296])>) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut Option<unsafe fn(&mut [u8; 296])> {
        self.0.get()
    }
}

struct PfHandlerCell(UnsafeCell<Option<unsafe fn(u64, u32) -> i32>>);
unsafe impl Sync for PfHandlerCell {}
impl PfHandlerCell {
    const fn new(val: Option<unsafe fn(u64, u32) -> i32>) -> Self {
        Self(UnsafeCell::new(val))
    }
    fn get(&self) -> *mut Option<unsafe fn(u64, u32) -> i32> {
        self.0.get()
    }
}

/// Registered syscall handler (set by kernel at init).
#[used]
static SYSCALL_HANDLER: SyscallHandlerCell = SyscallHandlerCell::new(None);

/// Registered post-syscall hook (set by kernel at init).
#[used]
static POST_SYSCALL_HOOK: PostSyscallHookCell = PostSyscallHookCell::new(None);

/// Registered UART input callback (set by kernel at init).
/// Called on each timer tick if a byte is available from UART.
#[used]
static UART_INPUT_CALLBACK: UartInputCallbackCell = UartInputCallbackCell::new(None);

/// Registered timer callback (set by kernel-boot at init).
/// Called on each timer interrupt with the trap frame.
#[used]
static TIMER_CALLBACK: TimerCallbackCell = TimerCallbackCell::new(None);

/// Registered page fault handler (set by kernel-boot at init).
/// Called on user-mode page faults with (fault_addr, error_code).
/// Returns 0 if handled (process blocked, VM notified), -1 if fatal.
#[used]
static PF_HANDLER: PfHandlerCell = PfHandlerCell::new(None);

/// Register the basic syscall dispatch function.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_syscall_handler(handler: unsafe fn(usize, &[u64; 6]) -> i64) {
    unsafe {
        core::ptr::write(SYSCALL_HANDLER.get(), Some(handler));
    }
}

/// Register the post-syscall hook for process switching.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_post_syscall_hook(hook: unsafe fn(&mut [u8; 296])) {
    unsafe {
        core::ptr::write(POST_SYSCALL_HOOK.get(), Some(hook));
    }
}

/// Register the UART input callback.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_uart_input_callback(cb: unsafe fn(u8)) {
    unsafe {
        core::ptr::write(UART_INPUT_CALLBACK.get(), Some(cb));
    }
}

/// Register the timer callback for preemptive scheduling.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_timer_callback(cb: unsafe fn(&mut [u8; 296])) {
    unsafe {
        core::ptr::write(TIMER_CALLBACK.get(), Some(cb));
    }
}

/// Register the page fault handler for VM forwarding.
///
/// # Safety
///
/// Must be called once during kernel init, before any userspace execution.
pub unsafe fn register_page_fault_handler(handler: unsafe fn(u64, u32) -> i32) {
    unsafe {
        core::ptr::write(PF_HANDLER.get(), Some(handler));
    }
}

/// One byte to the M-mode console (`sbi_legacy_console_putchar`): the only
/// output that works without page tables, a UART driver or a runnable process.
unsafe fn sbi_putc(c: u8) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 1u64,
            in("a6") 0u64,
            in("a0") c as u64,
            in("a1") 0u64,
            in("a2") 0u64,
            options(nomem, nostack),
        );
    }
}

unsafe fn sbi_puts(s: &str) {
    for &b in s.as_bytes() {
        unsafe {
            sbi_putc(b);
        }
    }
}

unsafe fn sbi_hex(val: u64) {
    let hex = b"0123456789abcdef";
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        unsafe {
            sbi_putc(hex[nibble]);
        }
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

    if is_interrupt(scause_val) {
        match code {
            cause::SUP_TIMER_INTR => {
                unsafe {
                    crate::clint::handle_timer_interrupt();
                    if let Some(cb) = *UART_INPUT_CALLBACK.get() {
                        // Mask SIE so the drain is atomic w.r.t. a nested
                        // drain (an external UART IRQ can fire while this
                        // trap handler runs with SIE=1). ser_input ops are
                        // individually masked, but the pop→push loop must
                        // not interleave with another drain or bytes
                        // reorder.
                        let saved = crate::hal::irq_save();
                        while let Some(byte) = crate::uart::try_getchar() {
                            cb(byte);
                        }
                        crate::hal::irq_restore(saved);
                    }
                    if let Some(cb) = *TIMER_CALLBACK.get() {
                        cb(frame);
                    }
                };
            }
            cause::SUP_EXT_INTR => {
                // External interrupt — claim and handle via PLIC
                unsafe {
                    let irq = crate::plic::claim_irq();
                    if irq != 0 {
                        if irq == crate::plic::UART_IRQ {
                            // Drain the 16550 RX FIFO into the ser_input
                            // ring so piped bursts don't overrun the
                            // 16-byte FIFO while the shell is busy between
                            // timer ticks. Mask SIE so the drain is atomic
                            // w.r.t. a nested timer-tick drain (see the
                            // timer branch).
                            if let Some(cb) = *UART_INPUT_CALLBACK.get() {
                                let saved = crate::hal::irq_save();
                                while let Some(byte) = crate::uart::try_getchar() {
                                    cb(byte);
                                }
                                crate::hal::irq_restore(saved);
                            }
                        }
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
                let ret = match unsafe { *SYSCALL_HANDLER.get() } {
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
                if let Some(hook) = unsafe { *POST_SYSCALL_HOOK.get() } {
                    unsafe { hook(frame) };
                }
            }
            cause::INSTR_PAGE_FAULT | cause::LOAD_PAGE_FAULT | cause::STORE_PAGE_FAULT => {
                // The fault address comes from the frame, where the trap entry
                // saved `stval` — not from the live CSR, which a nested trap can
                // have overwritten by the time this handler runs (see the note in
                // `trap_asm.rs`). A load/store to an address another trap faulted
                // on, or an instruction fetch whose fetch address was replaced by
                // 0, is a `SIGSEGV` for the wrong process.
                let stval = u64::from_ne_bytes(frame[288..296].try_into().unwrap());
                let sepc = u64::from_ne_bytes(frame[256..264].try_into().unwrap());

                // Check the mode the fault was taken in (SPP bit in the saved
                // sstatus) to synthesize the correct error code — the fault
                // itself is forwarded to VM regardless of mode (the kernel's
                // handle_page_fault gate is address-based).
                let saved_sstatus = u64::from_ne_bytes(frame[264..272].try_into().unwrap());
                let spp = (saved_sstatus >> 8) & 1;

                // Build error_code matching x86_64 format:
                //   bit 0: present (1 = page-protection violation)
                //   bit 1: write
                //   bit 2: user
                //   bit 4: instruction fetch
                // Kernel-mode faults omit the user bit; the write bit is kept
                // so VM's COW/demand-paging makes the page writable and the
                // retried store succeeds.
                let error_code = match (code, spp) {
                    (cause::INSTR_PAGE_FAULT, 0) => 0x14, // user | instruction
                    (cause::INSTR_PAGE_FAULT, _) => 0x10, // instruction
                    (cause::STORE_PAGE_FAULT, 0) => 0x07, // present | write | user
                    (cause::STORE_PAGE_FAULT, _) => 0x03, // present | write
                    (_, 0) => 0x05,                       // load: present | user
                    _ => 0x01,                            // load: present
                };

                match unsafe { *PF_HANDLER.get() } {
                    Some(handler) => {
                        let ret = unsafe { handler(stval, error_code) };
                        if ret == 0 {
                            // Handled: process blocked with RTS_PAGEFAULT.
                            // Switch to another process via post-syscall hook,
                            // which saves the fault context (sepc/sstatus/SP)
                            // into the process's p_reg — sret later resumes it
                            // in the same mode (U or S).
                            if let Some(hook) = unsafe { *POST_SYSCALL_HOOK.get() } {
                                unsafe { hook(frame) };
                            }
                            // Frame now holds the next process's state.
                            // sret in trap_asm returns to that process.
                            return;
                        }
                        // Fatal: handler returned -1.
                    }
                    None => {
                        // No handler registered — fall through to halt.
                    }
                }

                // Fatal page fault: print diagnostics and halt.
                // Use SBI console for diagnostics (no page table dependency).
                unsafe {
                    let sstatus: u64;
                    core::arch::asm!("csrr {v}, sstatus", v = out(reg) sstatus, options(nomem, nostack));
                    sbi_puts("!PF ");
                    sbi_hex(stval);
                    sbi_putc(b' ');
                    sbi_hex(sepc);
                    sbi_putc(b' ');
                    sbi_hex(scause_val);
                    sbi_putc(b' ');
                    sbi_hex(sstatus);
                    sbi_putc(b'\r');
                    sbi_putc(b'\n');
                }
                loop {
                    unsafe { core::arch::asm!("wfi", options(nomem, nostack)) }
                }
            }
            // Anything else — an illegal instruction, a misaligned or
            // access-faulting load or store, a breakpoint — has no handler here:
            // nothing in this port turns one of those into a signal for the
            // process that caused it, so there is no "carry on" to return to.
            //
            // What it must not be is silence, which is what this arm used to be
            // (a bare `wfi` loop). That turned a single wrong bit in an `exec`ed
            // process's `sstatus` — the FP unit left disabled, `PSL_USERSET`'s
            // `FS` field written as bit 9 by `hal::exec_init_regs` — into a
            // machine that stopped with no output at all: the first
            // floating-point instruction a C program executes trapped, the trap
            // handler slept, and the only symptom was a shell that never came
            // back, which reads as a hung guest rather than as one instruction.
            // Naming the cause, the faulting instruction's `stval`, the program
            // counter and the mode is what makes the next one a two-minute
            // diagnosis instead.
            unhandled => {
                unsafe {
                    let stval = u64::from_ne_bytes(frame[288..296].try_into().unwrap());
                    let sepc = u64::from_ne_bytes(frame[256..264].try_into().unwrap());
                    let sstatus = u64::from_ne_bytes(frame[264..272].try_into().unwrap());
                    sbi_puts("!trap cause=");
                    sbi_hex(unhandled);
                    sbi_puts(" stval=");
                    sbi_hex(stval);
                    sbi_puts(" sepc=");
                    sbi_hex(sepc);
                    sbi_puts(" sstatus=");
                    sbi_hex(sstatus);
                    sbi_puts(if (sstatus >> 8) & 1 == 0 {
                        " mode=user\r\n"
                    } else {
                        " mode=kernel\r\n"
                    });
                }
                loop {
                    unsafe {
                        core::arch::asm!("wfi", options(nomem, nostack));
                    }
                }
            }
        }
    }
}
