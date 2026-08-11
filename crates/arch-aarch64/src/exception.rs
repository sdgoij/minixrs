//! AArch64 exception vector table and handlers.
//!
//! Implements the standard ARMv8 exception model with separate
//! handlers for synchronous exceptions, IRQs, FIQs, and SErrors
//! at each exception level and stack pointer selection.
//!
//! We handle:
//! - EL1h (kernel, SP=SP_EL1): timer IRQ, UART IRQ
//! - EL0 sync (user): syscalls (SVC #0), page faults
//! - EL1 sync (kernel): page faults

use core::arch::global_asm;

global_asm!(
    r#"
.section .text.vectors, "ax"
.balign 0x800
.globl exception_vector_table
exception_vector_table:

.balign 0x80
    b   .           // Synchronous
.balign 0x80
    b   .           // IRQ
.balign 0x80
    b   .           // FIQ
.balign 0x80
    b   .           // SError

.balign 0x80
    b   el1_sync_vector         // Synchronous (page faults in kernel)
.balign 0x80
    b   el1_irq_handler        // IRQ (timer, UART)
.balign 0x80
    b   .                      // FIQ (not used)
.balign 0x80
    b   .                      // SError

.balign 0x80
    b   el0_sync_handler       // Synchronous (syscalls, page faults)
.balign 0x80
    b   el1_irq_handler        // IRQ from user mode
.balign 0x80
    b   .                      // FIQ
.balign 0x80
    b   .                      // SError

.balign 0x80
    b   .
.balign 0x80
    b   .
.balign 0x80
    b   .
.balign 0x80
    b   .
"#
);

/// Return the address of the exception vector table.
pub fn vector_table_addr() -> u64 {
    unsafe extern "C" {
        fn exception_vector_table();
    }
    exception_vector_table as *const () as usize as u64
}

/// Callback for syscall dispatch. Takes the syscall number and argument
/// array, returns the result.
pub type SyscallHandler = unsafe fn(usize, &[u64; 6]) -> i64;

/// Callback after syscall dispatch, for scheduler context switch.
/// Takes the trap frame (raw byte array).
pub type PostSyscallHook = unsafe fn(&mut [u8; 288]);

/// Callback for UART input (byte received).
pub type UartInputCallback = unsafe fn(u8);

/// Callback for timer ticks.
pub type TimerCallback = unsafe fn(&mut [u8; 288]);

/// Callback for page faults. Returns 0 on success (handled), -1 on failure.
pub type PageFaultHandler = unsafe fn(u64, u32) -> i32;

static mut SYSCALL_HANDLER: Option<SyscallHandler> = None;
static mut POST_SYSCALL_HOOK: Option<PostSyscallHook> = None;
static mut UART_INPUT_CB: Option<UartInputCallback> = None;
static mut TIMER_CB: Option<TimerCallback> = None;
static mut PF_HANDLER: Option<PageFaultHandler> = None;

pub fn register_syscall_handler(handler: SyscallHandler) {
    unsafe {
        SYSCALL_HANDLER = Some(handler);
    }
}

pub fn register_post_syscall_hook(hook: PostSyscallHook) {
    unsafe {
        POST_SYSCALL_HOOK = Some(hook);
    }
}

pub fn register_uart_input_callback(cb: UartInputCallback) {
    unsafe {
        UART_INPUT_CB = Some(cb);
    }
}

pub fn register_timer_callback(cb: TimerCallback) {
    unsafe {
        TIMER_CB = Some(cb);
    }
}

pub fn register_page_fault_handler(handler: PageFaultHandler) {
    unsafe {
        PF_HANDLER = Some(handler);
    }
}

/// ESR_EL1 Exception Class values.
#[allow(dead_code)]
mod esr {
    pub const EC_SVC64: u64 = 0x15; // SVC from AArch64
    pub const EC_INSTR_ABT_EL0: u64 = 0x20; // Instruction abort from EL0
    pub const EC_INSTR_ABT_EL1: u64 = 0x21; // Instruction abort from EL1
    pub const EC_DATA_ABT_EL0: u64 = 0x24; // Data abort from EL0
    pub const EC_DATA_ABT_EL1: u64 = 0x25; // Data abort from EL1
}

fn esr_ec(esr: u64) -> u64 {
    (esr >> 26) & 0x3F
}

/// Synthesize an x86-format page-fault error code from ESR_EL1.
///
/// x86 format: bit 0 = present (1 = protection violation), bit 1 = write,
/// bit 2 = user, bit 4 = instruction fetch. ESR_EL1 encodes faults
/// differently: DFSC[5:0] (12-15 = permission fault, 4-7 = translation
/// fault), WnR (bit 6 = write for data aborts), and the exception class
/// selects instruction vs data. The raw ESR's bit 1 does NOT mean "write",
/// so it cannot be passed through — VM's COW/demand-paging logic keys on
/// the x86 bits.
fn synth_pf_error_code(esr: u64, el0: bool) -> u32 {
    let ec = esr_ec(esr);
    let is_instr = ec == esr::EC_INSTR_ABT_EL0 || ec == esr::EC_INSTR_ABT_EL1;
    let dfsc = esr & 0x3F;
    let mut code = 0u32;
    if (12..=15).contains(&dfsc) {
        code |= 0x1; // permission fault → page was present
    }
    if !is_instr && esr & (1 << 6) != 0 {
        code |= 0x2; // data abort store (WnR)
    }
    if el0 {
        code |= 0x4;
    }
    if is_instr {
        code |= 0x10;
    }
    code
}

// Assembly entry for EL1 synchronous exceptions (kernel-mode faults).
// Saves a full context frame on the kernel stack (mirroring
// el0_sync_handler), dispatches data/instr aborts to el1_pf_handler, and
// returns via the common el1_sync_return. Frame layout identical to
// el0_sync_handler, plus the interrupted kernel SP captured at [272..288)
// (a free slot between SPSR@264 and the SIMD block@288) so a kernel-mode
// fault can be retried with the original stack.

global_asm!(
    r#"
.globl el1_sync_vector
el1_sync_vector:
    // On entry from EL1: SP = SP_EL1 (kernel stack). The frame is
    // allocated BELOW the interrupted context (free stack), so no
    // exception-stack switch is needed.
    sub     sp, sp, #672

    // Save GPRs x0-x30
    stp     x0,  x1,  [sp, #0]
    stp     x2,  x3,  [sp, #16]
    stp     x4,  x5,  [sp, #32]
    stp     x6,  x7,  [sp, #48]
    stp     x8,  x9,  [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30,      [sp, #240]

    // Capture the interrupted kernel SP (frame base + 672) at frame[272]
    // so a kernel-mode resume can restore it before eret. Done after the
    // GPR stores (x10's interrupted value is already in the frame).
    add     x10, sp, #672
    str     x10, [sp, #272]

    // Save SP_EL0 (the syscalling process's user SP)
    mrs     x0, sp_el0
    str     x0, [sp, #248]

    // Save ELR_EL1 and SPSR_EL1
    mrs     x0, elr_el1
    mrs     x1, spsr_el1
    stp     x0, x1, [sp, #256]

    // Save the caller-saved SIMD/FP registers (q0-q7, q16-q31).
    stp     q0, q1,   [sp, #288]
    stp     q2, q3,   [sp, #320]
    stp     q4, q5,   [sp, #352]
    stp     q6, q7,   [sp, #384]
    stp     q16, q17, [sp, #416]
    stp     q18, q19, [sp, #448]
    stp     q20, q21, [sp, #480]
    stp     q22, q23, [sp, #512]
    stp     q24, q25, [sp, #544]
    stp     q26, q27, [sp, #576]
    stp     q28, q29, [sp, #608]
    stp     q30, q31, [sp, #640]

    // Read ESR_EL1 to determine the exception class.
    mrs     x0, esr_el1
    lsr     x0, x0, #26       // EC = ESR[31:26]

    cmp     x0, #0x21          // EC_INSTR_ABT_EL1
    b.eq    1f
    cmp     x0, #0x25          // EC_DATA_ABT_EL1
    b.eq    1f

    // Unknown EL1 exception — print 'X' and hang.
    mov     x9, #0x09000000
    mov     w10, #'X'
    str     w10, [x9]
    b       .

1:  // Data/instr abort from EL1
    mov     x0, sp
    bl      el1_pf_handler
    b       el1_sync_return

// Common return path from EL1 exception.
el1_sync_return:
    // Restore caller-saved SIMD/FP registers first.
    ldp     q0, q1,   [sp, #288]
    ldp     q2, q3,   [sp, #320]
    ldp     q4, q5,   [sp, #352]
    ldp     q6, q7,   [sp, #384]
    ldp     q16, q17, [sp, #416]
    ldp     q18, q19, [sp, #448]
    ldp     q20, q21, [sp, #480]
    ldp     q22, q23, [sp, #512]
    ldp     q24, q25, [sp, #544]
    ldp     q26, q27, [sp, #576]
    ldp     q28, q29, [sp, #608]
    ldp     q30, q31, [sp, #640]

    // Restore GPRs (x10-x11 loaded after the MSRs).
    ldp     x0,  x1,  [sp, #0]
    ldp     x2,  x3,  [sp, #16]
    ldp     x4,  x5,  [sp, #32]
    ldp     x6,  x7,  [sp, #48]
    ldp     x8,  x9,  [sp, #64]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30,      [sp, #240]

    // Restore SP_EL0
    ldr     x10, [sp, #248]
    msr     sp_el0, x10

    // Restore ELR_EL1 and SPSR_EL1
    ldr     x10, [sp, #256]
    msr     elr_el1, x10
    ldr     x10, [sp, #264]
    msr     spsr_el1, x10

    // Resume-mode SP selection (x10 is scratch — x10/x11 are restored
    // in each branch below, so the decision cannot clobber them).
    // A kernel-mode resume (SPSR M = EL1h) restores SP_EL1 from
    // frame[272] (the interrupted kernel stack); a user-mode resume
    // unwinds the frame on the shared kernel stack.
    ldr     x10, [sp, #264]
    and     x10, x10, #0xF
    cmp     x10, #4
    b.ne    8f
    // Kernel resume: SP_EL1 = frame[272], then reload x10/x11 from the
    // frame (now 672 bytes below the restored SP_EL1).
    ldr     x10, [sp, #272]
    msr     sp_el1, x10
    sub     x10, sp, #592
    ldr     x10, [x10]
    sub     x11, sp, #584
    ldr     x11, [x11]
    eret
8:  // User resume: restore x10/x11, then unwind the frame.
    ldp     x10, x11, [sp, #80]
    add     sp, sp, #672
    eret
"#
);

/// C-level EL1 page fault handler.
///
/// # Safety
///
/// Called from assembly with frame pointer in x0.
#[unsafe(no_mangle)]
unsafe extern "C" fn el1_pf_handler(_frame: *mut u8) {
    let far: u64;
    let esr: u64;
    unsafe {
        core::arch::asm!(
            "mrs {far}, far_el1",
            "mrs {esr}, esr_el1",
            far = out(reg) far,
            esr = out(reg) esr,
        );
    }
    let error_code = synth_pf_error_code(esr, false); // EL1: no user bit
    if let Some(handler) = unsafe { PF_HANDLER } {
        let status = unsafe { handler(far, error_code) };
        if status != 0 {
            crate::hal::halt();
        } else {
            // Handled: process blocked with RTS_PAGEFAULT. Switch to
            // another process via the post-syscall hook, which saves the
            // fault context (ELR/SPSR/SP_EL0/SP_EL1) into the process's
            // p_reg — the resume erets the same mode.
            let frame_arr: &mut [u8; 288] = unsafe { &mut *(_frame as *mut [u8; 288]) };
            if let Some(hook) = unsafe { POST_SYSCALL_HOOK } {
                unsafe { hook(frame_arr) };
            }
        }
    }
}

// Assembly entry for IRQ — saves all registers on kernel stack,
// calls C handler, then restores and eret.

global_asm!(
    r#"
.globl el1_irq_handler
el1_irq_handler:
    // Build a full context frame on the kernel stack.
    // We were in either EL0 or EL1.  Save the interrupted context.
    sub     sp, sp, #672

    // Save GPRs x0-x30
    stp     x0,  x1,  [sp, #0]
    stp     x2,  x3,  [sp, #16]
    stp     x4,  x5,  [sp, #32]
    stp     x6,  x7,  [sp, #48]
    stp     x8,  x9,  [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30,      [sp, #240]

    // Capture the interrupted kernel SP (frame base + 672) at frame[272]
    // so a kernel-mode resume can restore it before eret. Done after the
    // GPR stores (x10's interrupted value is already in the frame).
    add     x10, sp, #672
    str     x10, [sp, #272]

    // Read ELR_EL1 and SPSR_EL1
    mrs     x0, elr_el1
    mrs     x1, spsr_el1
    stp     x0, x1, [sp, #256]   // elr_el1 at +256, spsr_el1 at +264

    // Read SP_EL0 (if we came from EL0, this is the user stack pointer)
    // On EL1→EL1 IRQ, SP_EL0 is not automatically saved; but we read
    // it anyway to have a consistent frame.
    mrs     x0, sp_el0
    str     x0, [sp, #248]

    // Save the caller-saved SIMD/FP registers (q0-q7, q16-q31). The
    // kernel's C code (memcpy/memset for IPC message copies) uses SIMD
    // registers, which would otherwise clobber the interrupted user's
    // FP state. q8-q15 are callee-saved (preserved by the C ABI), so
    // they need no save here.
    stp     q0, q1,   [sp, #288]
    stp     q2, q3,   [sp, #320]
    stp     q4, q5,   [sp, #352]
    stp     q6, q7,   [sp, #384]
    stp     q16, q17, [sp, #416]
    stp     q18, q19, [sp, #448]
    stp     q20, q21, [sp, #480]
    stp     q22, q23, [sp, #512]
    stp     q24, q25, [sp, #544]
    stp     q26, q27, [sp, #576]
    stp     q28, q29, [sp, #608]
    stp     q30, q31, [sp, #640]

    // Call the C-level IRQ handler with frame pointer in x0.
    mov     x0, sp
    bl      el1_irq_handler_c

    // Restore caller-saved SIMD/FP registers before the GPRs.
    ldp     q0, q1,   [sp, #288]
    ldp     q2, q3,   [sp, #320]
    ldp     q4, q5,   [sp, #352]
    ldp     q6, q7,   [sp, #384]
    ldp     q16, q17, [sp, #416]
    ldp     q18, q19, [sp, #448]
    ldp     q20, q21, [sp, #480]
    ldp     q22, q23, [sp, #512]
    ldp     q24, q25, [sp, #544]
    ldp     q26, q27, [sp, #576]
    ldp     q28, q29, [sp, #608]
    ldp     q30, q31, [sp, #640]

    // Return from IRQ: the C handler may have modified the frame
    // (in particular: ELR_EL1, SPSR_EL1, and SP_EL0).
    // Restore GPRs first.
    ldp     x0,  x1,  [sp, #0]
    ldp     x2,  x3,  [sp, #16]
    ldp     x4,  x5,  [sp, #32]
    ldp     x6,  x7,  [sp, #48]
    ldp     x8,  x9,  [sp, #64]
    // x10-x11 loaded below after MSR
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30,      [sp, #240]

    // Restore SP_EL0
    ldr     x10, [sp, #248]
    msr     sp_el0, x10

    // Restore ELR_EL1 and SPSR_EL1
    ldr     x10, [sp, #256]
    msr     elr_el1, x10
    ldr     x10, [sp, #264]
    msr     spsr_el1, x10

    // Load x10, x11 last (they may have been used as scratch)
    ldp     x10, x11, [sp, #80]

    // Resume-mode SP selection (x10 is scratch — the user path below
    // reloads x10/x11 from the frame, so this decision cannot clobber
    // them). A kernel-mode resume (SPSR M = EL1h — an IRQ taken in the
    // idle loop) restores SP_EL1 from frame[272] (the interrupted kernel
    // stack); a user-mode resume unwinds the frame on the shared stack.
    ldr     x10, [sp, #264]
    and     x10, x10, #0xF
    cmp     x10, #4
    b.ne    8f
    // Kernel resume: SP_EL1 = frame[272], then reload x10/x11 from the
    // frame (now 672 bytes below the restored SP_EL1).
    ldr     x10, [sp, #272]
    msr     sp_el1, x10
    sub     x10, sp, #592
    ldr     x10, [x10]
    sub     x11, sp, #584
    ldr     x11, [x11]
    eret
8:  // User resume: reload x10/x11, then unwind the frame.
    ldp     x10, x11, [sp, #80]
    add     sp, sp, #672
    eret
"#
);

/// C-level IRQ handler called from assembly.
///
/// # Safety
///
/// Called from assembly IRQ handler with frame pointer in x0.
/// C-level IRQ handler called from assembly.
#[unsafe(no_mangle)]
unsafe extern "C" fn el1_irq_handler_c(_frame: *mut u8) {
    // Timer IRQ: acknowledge via system registers (no GIC MMIO needed).
    // This avoids page table issues with device MMIO at 0x08000000.
    unsafe {
        crate::timer::timer_irq_ack();
    }

    // Poll the UART on every IRQ (the timer fires continuously, so this
    // effectively feeds the ser_input ring on each tick). The ring is the
    // single input source for read_blocking; polling the UART there instead
    // would race with this producer and reorder/drop bytes under burst
    // (piped) input. Mirrors the RISC-V timer-tick input poll.
    if let Some(cb) = unsafe { UART_INPUT_CB } {
        while let Some(byte) = crate::hal::poll_console() {
            unsafe { cb(byte) };
        }
    }

    // Notify the scheduler.
    if let Some(cb) = unsafe { TIMER_CB } {
        let frame_slice = unsafe { core::slice::from_raw_parts_mut(_frame, 288) };
        unsafe { cb(frame_slice.try_into().unwrap()) };
    }
}

global_asm!(
    r#"
.globl el0_sync_handler
el0_sync_handler:
    // On entry from EL0: SP = SP_EL1 (kernel stack).
    // Save full context on kernel stack.
    sub     sp, sp, #672

    // Save GPRs x0-x30
    stp     x0,  x1,  [sp, #0]
    stp     x2,  x3,  [sp, #16]
    stp     x4,  x5,  [sp, #32]
    stp     x6,  x7,  [sp, #48]
    stp     x8,  x9,  [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30,      [sp, #240]

    // Save SP_EL0
    mrs     x0, sp_el0
    str     x0, [sp, #248]

    // Save ELR_EL1 and SPSR_EL1
    mrs     x0, elr_el1
    mrs     x1, spsr_el1
    stp     x0, x1, [sp, #256]

    // Save the caller-saved SIMD/FP registers (q0-q7, q16-q31). The
    // kernel's C code (memcpy/memset for IPC message copies) uses SIMD
    // registers, which would otherwise clobber the user's FP state
    // across the syscall (observed: the virtio_net driver's memset
    // wrote garbage into its safecopy message after a RECEIVE).
    // q8-q15 are callee-saved (preserved by the C ABI), so they need
    // no save here.
    stp     q0, q1,   [sp, #288]
    stp     q2, q3,   [sp, #320]
    stp     q4, q5,   [sp, #352]
    stp     q6, q7,   [sp, #384]
    stp     q16, q17, [sp, #416]
    stp     q18, q19, [sp, #448]
    stp     q20, q21, [sp, #480]
    stp     q22, q23, [sp, #512]
    stp     q24, q25, [sp, #544]
    stp     q26, q27, [sp, #576]
    stp     q28, q29, [sp, #608]
    stp     q30, q31, [sp, #640]

    // Read ESR_EL1 to determine exception class
    mrs     x0, esr_el1
    lsr     x0, x0, #26       // EC = ESR[31:26]

    cmp     x0, #0x15          // EC_SVC64 (syscall from AArch64)
    b.eq    1f

    cmp     x0, #0x20          // EC_INSTR_ABT_EL0
    b.eq    2f
    cmp     x0, #0x24          // EC_DATA_ABT_EL0
    b.eq    2f

    cmp     x0, #0x07          // EC_FP_SIMD (trapped SIMD/FP access)
    b.eq    3f

    // Unknown exception — print 'X' and hang.
    mov     x9, #0x09000000
    mov     w10, #'X'
    str     w10, [x9]
    b       .

1:  // Syscall
    mov     x0, sp
    bl      el0_svc_handler
    b       el0_sync_return

2:  // Page fault (instruction or data abort from EL0)
    mov     x0, sp
    bl      el0_pf_handler
    b       el0_sync_return

3:  // FP/SIMD trap — enable SIMD at EL0 and EL1, then retry.
    mov     x9, #(3 << 20)
    msr     cpacr_el1, x9
    isb
    b       el0_sync_return

// Common return path from EL0 exception.
el0_sync_return:
    // Restore caller-saved SIMD/FP registers first (sp still points at
    // the frame base; only GPRs are used below).
    ldp     q0, q1,   [sp, #288]
    ldp     q2, q3,   [sp, #320]
    ldp     q4, q5,   [sp, #352]
    ldp     q6, q7,   [sp, #384]
    ldp     q16, q17, [sp, #416]
    ldp     q18, q19, [sp, #448]
    ldp     q20, q21, [sp, #480]
    ldp     q22, q23, [sp, #512]
    ldp     q24, q25, [sp, #544]
    ldp     q26, q27, [sp, #576]
    ldp     q28, q29, [sp, #608]
    ldp     q30, q31, [sp, #640]

    // Restore GPRs (except scratch)
    ldp     x0,  x1,  [sp, #0]
    ldp     x2,  x3,  [sp, #16]
    ldp     x4,  x5,  [sp, #32]
    ldp     x6,  x7,  [sp, #48]
    ldp     x8,  x9,  [sp, #64]
    // x10-x11 loaded after MSR
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30,      [sp, #240]

    // Restore SP_EL0
    ldr     x10, [sp, #248]
    msr     sp_el0, x10

    // Restore ELR_EL1 and SPSR_EL1
    ldr     x10, [sp, #256]
    msr     elr_el1, x10
    ldr     x10, [sp, #264]
    msr     spsr_el1, x10

    // Resume-mode SP selection (x10 is scratch — x10/x11 are restored
    // in each branch below, so the decision cannot clobber them).
    // A kernel-mode resume (SPSR M = EL1h — a frame swapped in by the
    // post-syscall hook for a process that faulted in kernel mode)
    // restores SP_EL1 from frame[272] (the interrupted kernel stack); a
    // user-mode resume unwinds the frame on the shared kernel stack.
    ldr     x10, [sp, #264]
    and     x10, x10, #0xF
    cmp     x10, #4
    b.ne    8f
    // Kernel resume: SP_EL1 = frame[272], then reload x10/x11 from the
    // frame (now 672 bytes below the restored SP_EL1).
    ldr     x10, [sp, #272]
    msr     sp_el1, x10
    sub     x10, sp, #592
    ldr     x10, [x10]
    sub     x11, sp, #584
    ldr     x11, [x11]
    eret
8:  // User resume: restore x10/x11, then unwind the frame.
    ldp     x10, x11, [sp, #80]
    add     sp, sp, #672
    eret
"#
);

/// C-level EL0 syscall handler.
///
/// # Safety
///
/// Called from assembly with frame pointer in x0.
#[unsafe(no_mangle)]
unsafe extern "C" fn el0_svc_handler(frame: *mut u8) {
    let frame_slice = unsafe { core::slice::from_raw_parts_mut(frame, 288) };
    let frame_arr: &mut [u8; 288] = frame_slice.try_into().unwrap();

    // Syscall number in x8 (offset 64)
    let nr = unsafe { crate::hal::read_syscall_nr(frame_arr) } as usize;

    // Arguments in x0-x5 (offsets 0, 8, 16, 24, 32, 40)
    let mut args = [0u64; 6];
    for i in 0..6 {
        args[i] = unsafe { crate::hal::read_syscall_arg(frame_arr, i) };
    }

    // Save the current frame to the caller's p_reg BEFORE dispatch so that
    // syscalls which clone the caller's state (fork) copy the exact
    // syscall-entry registers. Without this, p_reg holds a stale frame from
    // the last timer tick or blocking syscall, and a fork child resumes at
    // the wrong instruction. Exec (61) is skipped — its handler replaces
    // p_reg with the new process image. Mirrors x86's save_proc_regs.
    // p_reg is the first field of Proc (offset 0, asserted in proc.rs), so
    // the caller pointer doubles as the p_reg pointer.
    if nr != 61 {
        let caller = crate::hal::current_proc() as *mut u8;
        if !caller.is_null() {
            unsafe {
                core::ptr::copy_nonoverlapping(frame_arr.as_ptr(), caller, 288);
            }
        }
    }

    // Dispatch
    if let Some(handler) = unsafe { SYSCALL_HANDLER } {
        let ret = unsafe { handler(nr, &args) };
        unsafe { crate::hal::write_retval(frame_arr, ret as u64) };
    }

    // No ELR advancement is needed here: unlike RISC-V (where ecall
    // sets sepc to the ecall instruction itself), AArch64 hardware
    // already sets ELR_EL1 to the instruction following the SVC
    // (svc + 4).  Advancing again would skip the next instruction and
    // corrupt every syscall return.

    // Post-syscall hook for scheduler context switch.
    if let Some(hook) = unsafe { POST_SYSCALL_HOOK } {
        unsafe {
            hook(frame_arr);
        }
    }
}

/// C-level EL0 page fault handler.
///
/// # Safety
///
/// Called from assembly with frame pointer in x0.
#[unsafe(no_mangle)]
unsafe extern "C" fn el0_pf_handler(_frame: *mut u8) {
    let far: u64;
    let esr: u64;
    unsafe {
        core::arch::asm!(
            "mrs {far}, far_el1",
            "mrs {esr}, esr_el1",
            far = out(reg) far,
            esr = out(reg) esr,
        );
    }
    if let Some(handler) = unsafe { PF_HANDLER } {
        let status = unsafe { handler(far, synth_pf_error_code(esr, true)) };
        if status != 0 {
            crate::hal::halt();
        } else {
            let frame_arr: &mut [u8; 288] = unsafe { &mut *(_frame as *mut [u8; 288]) };
            if let Some(hook) = unsafe { POST_SYSCALL_HOOK } {
                unsafe { hook(frame_arr) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synth_pf_error_code_present_write_user() {
        // DFSC 15 (permission fault level 3) + WnR + EL0.
        let esr = (0x24u64 << 26) | (1 << 6) | 0b1111;
        assert_eq!(synth_pf_error_code(esr, true), 0x7);
    }

    #[test]
    fn test_synth_pf_error_code_translation_fault() {
        // DFSC 7 (translation fault level 3), no WnR, EL0.
        let esr = (0x24u64 << 26) | 0b111;
        assert_eq!(synth_pf_error_code(esr, true), 0x4);
    }

    #[test]
    fn test_synth_pf_error_code_kernel_store() {
        // EL1 data abort, WnR set.
        let esr = (0x25u64 << 26) | (1 << 6) | 0b1111;
        assert_eq!(synth_pf_error_code(esr, false), 0x3);
    }

    #[test]
    fn test_synth_pf_error_code_instr() {
        // EL1 instruction abort, DFSC 7.
        let esr = (0x21u64 << 26) | 0b111;
        assert_eq!(synth_pf_error_code(esr, false), 0x10);
    }
}
