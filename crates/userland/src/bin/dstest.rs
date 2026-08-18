//! DS_GETSYSINFO smoke test — `/bin/dstest`.
//!
//! Exercises the DS wire protocol through the `minix_util::ds` client: a
//! DS_PUBLISH from a plain user process must be rejected with EPERM (no
//! DS label — matching C `do_publish`), and DS_GETSYSINFO (SI_DATA_STORE)
//! must return OK with a well-sized buffer and EINVAL for a wrong size.
//! The store is empty at boot (no service publishes in this port yet), so
//! a successful copy is all zeros. Expect `dstest: OK` and exit 0.

#![no_std]
#![no_main]

use minix_util::ds;

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

// C struct data_store (ds/store.h): flags(4) + key[80] + owner[80] +
// union(24), 8-aligned → 192 bytes.
const DS_ENTRY_SIZE: usize = 192;
const NR_DS_KEYS: usize = 64;

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // A plain user process has no DS label → publish is rejected (EPERM).
    match ds::ds_publish_u32(b"dstest.key", 0x12345678) {
        Err(e) if e.0 == 1 => {}
        _ => {
            userland::write_err(b"dstest: publish not rejected\n");
            return 1;
        }
    }

    // DS_GETSYSINFO with a wrong size → EINVAL.
    let mut buf = [0u8; DS_ENTRY_SIZE * NR_DS_KEYS];
    let wrong_len = buf.len() - 1;
    match ds::ds_getsysinfo(&mut buf[..wrong_len]) {
        Err(e) if e.0 == 22 => {}
        _ => {
            userland::write_err(b"dstest: bad size accepted\n");
            return 2;
        }
    }

    // Correct size → OK; the store is empty at boot → all zeros.
    if ds::ds_getsysinfo(&mut buf).is_err() {
        userland::write_err(b"dstest: getsysinfo failed\n");
        return 3;
    }
    if buf.iter().any(|&b| b != 0) {
        userland::write_err(b"dstest: store not empty\n");
        return 4;
    }

    userland::write_out(b"dstest: OK\n");
    0
}
