//! Drive an interactive bash over a QEMU guest's serial console.
//!
//! `cargo xtask smoke` types into the minix shell and waits for its `#` prompt,
//! so it cannot reach an interactive bash (whose `bash-5.3#` prompt replaces it,
//! and whose readline polls the terminal instead of blocking on a line read).
//! This driver waits for each prompt and proves the part a `bash -c` scenario
//! cannot: that readline's own terminal setup and its poll-then-read path accept
//! a typed line.
//!
//! The marker is searched for at the start of a line, so the *echo* of the
//! command (`bash-5.3# echo _IBASH_OK_`) does not satisfy it; only the command's
//! output does.

use std::io;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use super::secs;
use crate::guest::Guest;

const MARKER: &str = "_IBASH_OK_";

pub fn run(log: &Path, seconds: u64, command: &mut Command) -> io::Result<bool> {
    let mut guest = Guest::spawn_tee(command, log)?;
    let timeout = secs(seconds);
    let program = command.get_program().to_string_lossy().into_owned();

    let ok = if !guest.wait_for(b"# ", timeout) {
        fail(&guest, log, "the guest never reached a shell prompt")
    } else if guest.send_line("bash").is_err() || !guest.wait_for(b"bash-5.3#", timeout) {
        fail(&guest, log, "bash never printed its prompt")
    } else if guest.send_line(&format!("echo {MARKER}")).is_err() {
        fail(&guest, log, "could not type the marker")
    } else if !guest.wait_for(format!("\n{MARKER}").as_bytes(), timeout) {
        fail(&guest, log, "bash ignored the typed line")
    } else {
        // The command is done; the echo of `exit` is what lets the log end
        // cleanly before the child is killed.
        let _ = guest.send_line("exit");
        true
    };

    thread::sleep(Duration::from_millis(200));
    if ok {
        println!("interactive bash ran a typed line ({program})");
    }
    Ok(ok)
}

/// Report and show what the guest said, as the script did.
fn fail(guest: &Guest, log: &Path, reason: &str) -> bool {
    eprintln!("!! interactive bash: {reason}. Tail of {}:", log.display());
    let tail = guest.with_transcript(|out| {
        let start = out.len().saturating_sub(400);
        String::from_utf8_lossy(&out[start..]).into_owned()
    });
    eprint!("{tail}");
    false
}
