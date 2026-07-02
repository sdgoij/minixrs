#![no_std]
#![no_main]

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "none")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(argc: i32, argv: *const *const u8) -> i32 {
    let mut buf = [""; 64];
    let args = unsafe { userland::parse_args(argc, argv, &mut buf) };
    userland::ls(args)
}
