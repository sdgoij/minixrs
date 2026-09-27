#![no_std]
#![no_main]

/// On host builds, link `std` to provide the global allocator and panic
/// handler.  On `target_os = "minix"`, `minix-rt` provides both instead.
#[cfg(not(target_os = "minix"))]
extern crate std;

#[unsafe(no_mangle)]
pub fn main() -> i32 {
    // The server main is target-only: on host builds (cargo test bins) it
    // would spin in its accept loop forever, so it is not called there. On wasm
    // the callee is a no-op (wlserver has no display path there), but the call is
    // still made so the crate — and with it `minix-rt`'s panic handler — is linked.
    #[cfg(target_os = "minix")]
    servers::wlserver::wlserver_main();
    0
}
