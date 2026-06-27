//! x86_64-specific kernel code.
//! Registers, interrupts, page tables, and assembly routines.

#![no_std]

/// Initialize x86_64 architecture subsystem.
pub fn init() {
    // No-op for now
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert!(true);
    }
}
