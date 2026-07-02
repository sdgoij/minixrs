#![no_std]
#![no_main]

/// On host builds, link `std` to provide the global allocator and panic
/// handler.  On `target_os = "none"`, `minix-rt` provides both instead.
#[cfg(not(target_os = "none"))]
extern crate std;

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    servers::ds::ds_server_main();
    0
}
