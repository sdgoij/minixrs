#![no_std]
#![no_main]

//! `waitid` smoke test: a child's exit status comes back decoded.
//!
//! `waitid` is the one caller of PM's third waitpid reply field and of the
//! `w_exitcode` status encoding, so what this pins is the *decoding*: a child
//! that calls `exit(42)` must report `si_code == CLD_EXITED` and
//! `si_status == 42`, not "killed by signal 42". Before PM encoded the status,
//! `WIFEXITED` was false for every non-zero exit, and a probe written against
//! the raw byte could not have told the two apart.
//!
//! Every wait here is on a child that has already been asked to exit and is
//! reaped with a blocking `waitid`, so the outcome does not depend on timing.
//! The option checks never reach PM at all: they are rejected while building
//! the request, so they cannot block either.

/// Host-only panic handler — required for clippy/lint compilation.
#[cfg(all(not(test), not(target_os = "minix")))]
#[panic_handler]
fn host_panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

use minix_std::process::{
    CLD_EXITED, P_ALL, P_PID, WEXITED, WNOWAIT, WSTOPPED, exit, fork, getuid, waitid,
};

fn fail(line: &[u8]) -> ! {
    userland::write_err(line);
    exit(1);
}

/// Fork a child that exits with `code` and return its pid.
fn child_exiting(code: i32) -> i32 {
    let pid = match unsafe { fork() } {
        Ok(pid) => pid,
        Err(_) => fail(b"waittest: fork failed\n"),
    };
    if pid == 0 {
        exit(code);
    }
    pid
}

#[allow(clippy::missing_safety_doc)]
#[unsafe(no_mangle)]
pub unsafe fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let (ruid, _) = match getuid() {
        Ok(ids) => ids,
        Err(_) => fail(b"waittest: getuid failed\n"),
    };

    // 1. `P_PID` selects one child, and its status decodes as a normal exit.
    //    `si_uid` is the child's real uid, which PM reports from the slot it
    //    reaps before the slot is reused.
    let child = child_exiting(42);
    match waitid(P_PID, child, WEXITED) {
        Ok(Some(info)) => {
            if info.pid != child {
                fail(b"waittest: si_pid is not the child\n");
            }
            if info.code != CLD_EXITED {
                fail(b"waittest: si_code is not CLD_EXITED\n");
            }
            if info.status != 42 {
                fail(b"waittest: si_status is not 42\n");
            }
            if info.uid != ruid {
                fail(b"waittest: si_uid is not our real uid\n");
            }
        }
        Ok(None) => fail(b"waittest: waitid(P_PID) reported no child\n"),
        Err(_) => fail(b"waittest: waitid(P_PID) failed\n"),
    }

    // 2. `P_ALL` takes any child and says which one it took.
    let child = child_exiting(7);
    match waitid(P_ALL, 0, WEXITED) {
        Ok(Some(info)) => {
            if info.pid != child {
                fail(b"waittest: P_ALL picked the wrong child\n");
            }
            if info.status != 7 {
                fail(b"waittest: P_ALL status is not 7\n");
            }
        }
        Ok(None) => fail(b"waittest: waitid(P_ALL) reported no child\n"),
        Err(_) => fail(b"waittest: waitid(P_ALL) failed\n"),
    }

    // 3. The states PM cannot report are refused, not silently ignored: there
    //    is no stop/continue encoding, and a reported child is always reaped.
    if waitid(P_PID, 0, WSTOPPED).is_ok() {
        fail(b"waittest: WSTOPPED was accepted\n");
    }
    if waitid(P_PID, 0, WEXITED | WNOWAIT).is_ok() {
        fail(b"waittest: WNOWAIT was accepted\n");
    }
    if waitid(P_PID, 0, 0).is_ok() {
        fail(b"waittest: an empty option set was accepted\n");
    }
    if waitid(99, 0, WEXITED).is_ok() {
        fail(b"waittest: an unknown id type was accepted\n");
    }

    userland::write_out(b"waittest: all phases verified\n");
    0
}
