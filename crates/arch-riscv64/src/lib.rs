//! RISC-V64-specific kernel code (bonus architecture).

#![no_std]

pub mod hal;

/// Initialize RISC-V64 architecture subsystem.
pub fn init() {
    // No-op for now
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let _ = 0;
    }
}
