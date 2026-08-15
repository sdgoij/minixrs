#![no_std]
#![no_main]

/// On host builds, link `std` to provide the global allocator and panic
/// handler.  On `target_os = "minix"`, `minix-rt` provides both instead.
#[cfg(not(target_os = "minix"))]
extern crate std;

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    // The server main is target-only: on host builds (cargo test bins) it
    // would spin in the receive loop forever, so it is not called there.
    #[cfg(target_os = "minix")]
    servers::wserver::wserver_main();
    0
}
