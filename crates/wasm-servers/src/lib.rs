//! Real MINIX servers, one exported entry point each.
//!
//! Every export here is a thin `extern "C"` shim over the server's own
//! `*_server_main` — there is no stand-in behaviour and nothing is skipped. The
//! shims exist for two mechanical reasons: a wasm module can only export a
//! function, and the servers' `src/bin/*.rs` are `#![no_main]` binaries, which a
//! `cdylib`-only target cannot build.
//!
//! # Why reaching a main loop is the thing to check
//!
//! DS, RS and PM each run their own init and then enter `loop { RECEIVE }`, and
//! none of them needs another server to get there. So the first syscall an
//! instance issues is its first receive, which means "it finished init and
//! reached its main loop" is readable from the host as a syscall trace — without
//! asking the servers to print anything on this port's behalf, and without
//! adding a hook to them.
//!
//! What that does *not* prove is that they can talk to each other, which is what
//! the DS client at the end of this file is for: it gives DS a real request to
//! answer, so the client-server path (`ds_publish`/`ds_retrieve`) runs for the
//! first time on this port. It needs `sys_vircopy`, because DS reads the key out
//! of the client's memory and on this port only the host can reach it (§5.1).
//!
//! And DS can only *accept* that request from a process it can name, which is
//! where RS's part comes in: RS hands DS its public process table as a grant in
//! an `RS_INIT` message (`servers/rs.rs`), DS maps every entry in use, and a
//! process that then announces itself with `rs_up` gets a label of its own. The
//! second client here is the control — same key, same protocol, no announcement —
//! so the refusal an unlabelled publisher gets stays measured rather than assumed.

#![no_std]
#![no_main]

/// Region the host uses for the Asyncify data buffer and its stack.
///
/// The buffer has to live somewhere the program will never touch, and this is
/// that place by construction: a reserved static nothing else refers to. The M2
/// guest instead took the address from the linker's `__heap_base`, which this
/// module does not export — and a region the host can name directly is the
/// sturdier arrangement of the two, given that an Asyncify overflow corrupts
/// memory silently rather than trapping.
///
/// 64 KiB of stack, sized the same way the fork spike sized it (36.1 bytes per
/// frame, so ~1800 frames). A server blocking in its receive loop is nowhere
/// near that; the number is generous on purpose, because the failure mode is
/// silent.
const ASYNCIFY_BUF_SIZE: usize = 65536;

#[repr(C, align(16))]
struct AsyncifyScratch([u8; ASYNCIFY_BUF_SIZE + 16]);

static mut ASYNCIFY_SCRATCH: AsyncifyScratch = AsyncifyScratch([0; ASYNCIFY_BUF_SIZE + 16]);

/// Address of the scratch region, so the host does not have to guess.
#[unsafe(no_mangle)]
pub extern "C" fn asyncify_scratch_ptr() -> u32 {
    core::ptr::addr_of_mut!(ASYNCIFY_SCRATCH) as u32
}

#[unsafe(no_mangle)]
pub extern "C" fn minix_server_ds() -> i32 {
    servers::ds::ds_server_main();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn minix_server_rs() -> i32 {
    servers::rs::rs_server_main();
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn minix_server_pm() -> i32 {
    servers::pm::pm_server_main();
    0
}

// ------------------------------------------------------------------ DS client

/// What the client observed, so the host can read it without a console.
///
/// `[publish status, retrieve status, retrieved value, rs_up status]` — the first
/// two are errnos (negated), the third the value the store handed back, and the
/// fourth the status of announcing this process to RS, which is what makes the
/// first one possible. Nothing in this module declares a console import, so a
/// report in memory is the only way the client's result can reach the host; the
/// M2 harness established the pattern.
static mut DS_REPORT: [i64; 4] = [0; 4];

/// Address of the report, so the host does not have to parse the module layout.
#[unsafe(no_mangle)]
pub extern "C" fn ds_report_ptr() -> u32 {
    core::ptr::addr_of_mut!(DS_REPORT) as u32
}

fn ds_report(idx: usize, value: i64) {
    // SAFETY: single-threaded instance, and the host reads the report only after
    // the entry point has returned or blocked. `addr_of_mut!` avoids taking a
    // reference to the mutable static.
    unsafe {
        core::ptr::write_volatile(
            core::ptr::addr_of_mut!(DS_REPORT).cast::<i64>().add(idx),
            value,
        )
    };
}

/// A real DS client: announce itself to RS, publish a value, then read it back.
///
/// Deliberately `minix_util`'s clients rather than hand-rolled messages, so what
/// runs is the real protocol against the real servers. The key is a `const` slice
/// in this instance's rodata, and its address is what DS is handed — so the value
/// can only come back if DS's `sys_vircopy` read it out of *this* instance, which
/// is a copy no part of the kernel can make here.
///
/// The `rs_up` call is what makes the publish possible at all: DS seeds its label
/// table from RS's public process table and RS publishes each service it
/// registers, so a process DS cannot name is refused every write. Announcing
/// first, with the label RS then publishes, is the whole of what authorises this
/// client.
#[unsafe(no_mangle)]
pub extern "C" fn minix_ds_client() -> i32 {
    const KEY: &[u8] = b"seam.key";
    const VALUE: u32 = 0x2a;

    ds_report(
        3,
        match minix_util::rs::rs_up(b"seamclient") {
            Ok(()) => 0,
            Err(e) => -(e.0 as i64),
        },
    );

    ds_report(
        0,
        match minix_util::ds::ds_publish_u32(KEY, VALUE) {
            Ok(()) => 0,
            Err(e) => -(e.0 as i64),
        },
    );

    match minix_util::ds::ds_retrieve_u32(KEY) {
        Ok(value) => {
            ds_report(1, 0);
            ds_report(2, value as i64);
        }
        Err(e) => {
            ds_report(1, -(e.0 as i64));
            ds_report(2, -1);
        }
    }

    0
}

/// A client that never announces itself to RS, so DS has no label for it.
///
/// This is the behaviour the seeding must *not* change: the label table is what
/// authorises a writer, and a process that skipped `rs_up` is not in it, so its
/// publish is refused as EPERM while a labelled one's succeeds. It writes the
/// same key, so a mistaken acceptance would be visible as a changed value rather
/// than as a missing entry.
#[unsafe(no_mangle)]
pub extern "C" fn minix_ds_client_unregistered() -> i32 {
    const KEY: &[u8] = b"seam.key";

    ds_report(
        0,
        match minix_util::ds::ds_publish_u32(KEY, 0x99) {
            Ok(()) => 0,
            Err(e) => -(e.0 as i64),
        },
    );

    0
}
