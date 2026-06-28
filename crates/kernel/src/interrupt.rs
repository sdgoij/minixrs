//! Hardware interrupt management — adapted from `minix/kernel/interrupt.c`
//!
//! Provides a generic per-IRQ handler framework. Interrupt handlers
//! are registered via [`put_irq_handler`] and managed through linked
//! lists of [`IrqHook`] structures (one list per IRQ line).
//!
//! The hardware-level enable/disable functions (`hw_intr_*`) are
//! stubs here; the arch layer will provide real implementations.

use crate::system::{IRQ_ACTIDS, IrqHook};

/// Maximum number of IRQ vectors.
pub const NR_IRQ_VECTORS: usize = 64;

/// Per-IRQ linked list heads — `NULL` means no handler for that IRQ.
static mut IRQ_HANDLERS: [*mut IrqHook; NR_IRQ_VECTORS] = [core::ptr::null_mut(); NR_IRQ_VECTORS];

// ─────────────────────────────────────────────────────────────────────────
// put_irq_handler
// ─────────────────────────────────────────────────────────────────────────

/// Register an interrupt handler.
///
/// Inserts `hook` into the per-IRQ linked list at `irq`, assigns a
/// unique bitmap ID (power of two), and enables the IRQ at the
/// hardware level if this is the first handler for this line.
///
/// # Safety
///
/// - `hook` must point to a valid, stable `IrqHook` (typically from
///   [`IRQ_HOOKS`](crate::system::IRQ_HOOKS)).
/// - `irq` must be in the range `0 .. NR_IRQ_VECTORS`.
/// - `handler` must be a function that can safely be called with
///   `hook` during interrupt context.
pub unsafe fn put_irq_handler(
    hook: *mut IrqHook,
    irq: i32,
    handler: unsafe fn(*mut IrqHook) -> i32,
) {
    if irq < 0 || irq >= NR_IRQ_VECTORS as i32 {
        panic!("invalid call to put_irq_handler: {}", irq);
    }

    // Walk the per-IRQ linked list, building a bitmap of used IDs.
    let mut line: *mut *mut IrqHook;
    unsafe {
        line = core::ptr::addr_of_mut!(IRQ_HANDLERS[irq as usize]);
    }
    let mut bitmap: i32 = 0;

    unsafe {
        while !(*line).is_null() {
            if *line == hook {
                return;
            }
            bitmap |= (**line).id;
            line = &mut (**line).next;
        }
    }

    // Find the lowest unused bitmap ID (powers of two: 1, 2, 4, 8, …).
    let mut id: i32 = 1;
    while id != 0 && (bitmap & id) != 0 {
        id <<= 1;
    }
    if id == 0 {
        panic!("Too many handlers for irq: {}", irq);
    }

    // Initialise the hook.
    unsafe {
        (*hook).next = core::ptr::null_mut();
        (*hook).handler = Some(handler);
        (*hook).irq = irq;
        (*hook).id = id;
        *line = hook;
    }

    // If no handlers are currently active for this IRQ line, enable the
    // hardware interrupt.
    unsafe {
        if IRQ_ACTIDS[irq as usize] == 0 {
            hw_intr_used(irq);
            hw_intr_unmask(irq);
        }
        IRQ_ACTIDS[irq as usize] |= id;
    }
}

// ─────────────────────────────────────────────────────────────────────────
// rm_irq_handler
// ─────────────────────────────────────────────────────────────────────────

/// Unregister an interrupt handler.
///
/// Removes `hook` from its per-IRQ linked list and disables the
/// hardware IRQ if no handlers remain.
///
/// # Safety
///
/// `hook` must have been registered with [`put_irq_handler`] and
/// must still point to valid memory.
pub unsafe fn rm_irq_handler(hook: *const IrqHook) {
    let irq: i32;
    let id: i32;
    unsafe {
        irq = (*hook).irq;
        id = (*hook).id;
    }

    if irq < 0 || irq >= NR_IRQ_VECTORS as i32 {
        panic!("invalid call to rm_irq_handler: {}", irq);
    }

    // Walk the list and remove the matching node.
    let mut line: *mut *mut IrqHook;
    unsafe {
        line = core::ptr::addr_of_mut!(IRQ_HANDLERS[irq as usize]);
    }
    unsafe {
        while !(*line).is_null() {
            if (**line).id == id {
                *line = (**line).next;
                if IRQ_ACTIDS[irq as usize] & id != 0 {
                    IRQ_ACTIDS[irq as usize] &= !id;
                }
            } else {
                line = &mut (**line).next;
            }
        }
    }

    // Disable the hardware IRQ if no handlers remain; otherwise
    // re-enable it if no handler is currently active.
    unsafe {
        if IRQ_HANDLERS[irq as usize].is_null() {
            hw_intr_mask(irq);
            hw_intr_not_used(irq);
        } else if IRQ_ACTIDS[irq as usize] == 0 {
            hw_intr_unmask(irq);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// irq_handle
// ─────────────────────────────────────────────────────────────────────────

/// Handle a hardware interrupt.
///
/// Called by the architecture-dependent interrupt dispatcher.
/// Masks the IRQ, walks every registered handler, clears active
/// bits for handlers that claim the interrupt, and re-enables the
/// IRQ only when no handler remains active.
///
/// # Safety
///
/// Must be called from interrupt context with `irq` in range.
pub unsafe fn irq_handle(irq: i32) {
    assert!(irq >= 0 && irq < NR_IRQ_VECTORS as i32);

    // Prevent re-entry for this IRQ.
    hw_intr_mask(irq);

    let mut hook: *mut IrqHook;
    unsafe {
        hook = IRQ_HANDLERS[irq as usize];
    }

    // Spurious interrupt — no registered handler.
    if hook.is_null() {
        return;
    }

    unsafe {
        while !hook.is_null() {
            // Mark this handler as active.
            IRQ_ACTIDS[irq as usize] |= (*hook).id;

            // Call the handler.  If it returns non-zero the interrupt is
            // considered handled and the active bit is cleared.
            if let Some(handler) = (*hook).handler
                && handler(hook) != 0
            {
                IRQ_ACTIDS[(*hook).irq as usize] &= !(*hook).id;
            }

            hook = (*hook).next;
        }
    }

    // Re-enable the IRQ only when no handler is still active.
    unsafe {
        if IRQ_ACTIDS[irq as usize] == 0 {
            hw_intr_unmask(irq);
        }
    }

    hw_intr_ack(irq);
}

// ─────────────────────────────────────────────────────────────────────────
// enable_irq / disable_irq
// ─────────────────────────────────────────────────────────────────────────

/// Enable (unmask) a specific IRQ hook.
///
/// Clears the active bit for `hook` and unmasks the hardware IRQ if
/// no other handler for this line is still active.
///
/// # Safety
///
/// `hook` must point to a registered [`IrqHook`].
pub unsafe fn enable_irq(hook: *const IrqHook) {
    let irq: i32;
    let id: i32;
    unsafe {
        irq = (*hook).irq;
        id = (*hook).id;
        IRQ_ACTIDS[irq as usize] |= id;
        if IRQ_ACTIDS[irq as usize] == 0 {
            hw_intr_unmask(irq);
        }
    }
}

/// Disable (mask) a specific IRQ hook.
///
/// Returns `true` if the IRQ was actually disabled by this call, or
/// `false` if it was already disabled.
///
/// # Safety
///
/// `hook` must point to a registered [`IrqHook`].
pub unsafe fn disable_irq(hook: *const IrqHook) -> bool {
    let irq: i32;
    let id: i32;
    unsafe {
        irq = (*hook).irq;
        id = (*hook).id;

        if IRQ_ACTIDS[irq as usize] & id == 0 {
            return false; // already disabled
        }

        IRQ_ACTIDS[irq as usize] &= !id;

        if IRQ_ACTIDS[irq as usize] == 0 {
            hw_intr_mask(irq);
        }
    }
    true
}

// ─────────────────────────────────────────────────────────────────────────
// intr_init
// ─────────────────────────────────────────────────────────────────────────

/// Initialize the interrupt subsystem.
///
/// Must be called once during boot, after the hardware interrupt
/// controller has been set up.
///
/// # Safety
///
/// Must be called exactly once, before any interrupt handlers are
/// registered.
pub unsafe fn intr_init() {
    unsafe {
        let handlers = core::ptr::addr_of_mut!(IRQ_HANDLERS);
        for i in 0..NR_IRQ_VECTORS {
            (*handlers)[i] = core::ptr::null_mut();
        }
        for irq in 0..NR_IRQ_VECTORS {
            hw_intr_mask(irq as i32);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Hardware stubs (replaced by the arch layer)
// ─────────────────────────────────────────────────────────────────────────

/// Called when an IRQ line is first used.
pub fn hw_intr_used(_irq: i32) {}

/// Called when an IRQ line is no longer used.
pub fn hw_intr_not_used(_irq: i32) {}

/// Mask (disable) an IRQ at the interrupt controller.
pub fn hw_intr_mask(_irq: i32) {}

/// Unmask (enable) an IRQ at the interrupt controller.
pub fn hw_intr_unmask(_irq: i32) {}

/// Acknowledge (send EOI for) an IRQ.
pub fn hw_intr_ack(_irq: i32) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::IrqHook;

    unsafe fn dummy_handler(hook: *mut IrqHook) -> i32 {
        unsafe { (*hook).id }
    }

    unsafe fn call_counter_handler(hook: *mut IrqHook) -> i32 {
        let ptr = unsafe { (*hook).notify_id as *mut u32 };
        unsafe {
            *ptr += 1;
        }
        1
    }

    #[test]
    fn test_intr_init_masks_all_vectors() {
        unsafe {
            intr_init();
            for i in 0..NR_IRQ_VECTORS {
                assert!(IRQ_HANDLERS[i].is_null(), "vector {} should be null", i);
            }
        }
    }

    #[test]
    fn test_put_irq_handler_registers() {
        unsafe {
            intr_init();
            let mut hook = IrqHook::default();
            let hook_ptr = &mut hook as *mut IrqHook;
            put_irq_handler(hook_ptr, 3, dummy_handler);
            assert_eq!((*hook_ptr).irq, 3);
            assert!((*hook_ptr).id != 0);
            assert!((*hook_ptr).handler.is_some());
        }
    }

    #[test]
    fn test_put_irq_handler_twice_is_noop() {
        unsafe {
            intr_init();
            let mut hook = IrqHook::default();
            let hook_ptr = &mut hook as *mut IrqHook;
            put_irq_handler(hook_ptr, 3, dummy_handler);
            let id1 = (*hook_ptr).id;
            put_irq_handler(hook_ptr, 3, dummy_handler);
            assert_eq!((*hook_ptr).id, id1, "re-init should not change id");
        }
    }

    #[test]
    fn test_put_multiple_handlers_different_ids() {
        unsafe {
            intr_init();
            let mut hook1 = IrqHook::default();
            let mut hook2 = IrqHook::default();
            put_irq_handler(&mut hook1 as *mut IrqHook, 5, dummy_handler);
            put_irq_handler(&mut hook2 as *mut IrqHook, 5, dummy_handler);
            assert!(
                hook1.id != hook2.id,
                "handlers on same IRQ must have different IDs"
            );
        }
    }

    #[test]
    fn test_rm_irq_handler_removes() {
        unsafe {
            intr_init();
            let mut hook = IrqHook::default();
            let hook_ptr = &mut hook as *mut IrqHook;
            put_irq_handler(hook_ptr, 7, dummy_handler);
            assert!(!IRQ_HANDLERS[7].is_null());
            rm_irq_handler(hook_ptr as *const IrqHook);
            assert!(IRQ_HANDLERS[7].is_null(), "handler should be removed");
        }
    }

    #[test]
    fn test_irq_handle_calls_handler() {
        unsafe {
            intr_init();
            let mut count: u32 = 0;
            let mut hook = IrqHook::default();
            hook.handler = Some(call_counter_handler);
            hook.notify_id = &mut count as *mut u32 as u64;
            let hook_ptr = &mut hook as *mut IrqHook;
            put_irq_handler(hook_ptr, 9, call_counter_handler);
            irq_handle(9);
            assert_eq!(count, 1, "handler should have been called");
        }
    }

    #[test]
    fn test_enable_disable_irq_toggles() {
        unsafe {
            intr_init();
            let mut hook = IrqHook::default();
            let hook_ptr = &mut hook as *mut IrqHook;
            put_irq_handler(hook_ptr, 4, dummy_handler);
            let id = (*hook_ptr).id;
            assert!(IRQ_ACTIDS[4] & id != 0, "should be active after put");
            disable_irq(hook_ptr as *const IrqHook);
            assert!(IRQ_ACTIDS[4] & id == 0, "should be inactive after disable");
            enable_irq(hook_ptr as *const IrqHook);
            assert!(IRQ_ACTIDS[4] & id != 0, "should be active after enable");
        }
    }

    #[test]
    fn test_hw_stubs_are_callable() {
        hw_intr_used(0);
        hw_intr_not_used(0);
        hw_intr_mask(0);
        hw_intr_unmask(0);
        hw_intr_ack(0);
    }
}
