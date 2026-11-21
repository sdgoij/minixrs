//! Typing into a guest whose console drops bytes it is not ready for.
//!
//! A GL display drops console bytes mid-line, and a truncated line still runs,
//! as a shorter command, so a paced send alone is not evidence the command
//! arrived: the guest's own echo is. Every helper here anchors on a transcript
//! offset taken before the bytes went out, because a prompt or echo printed by
//! an earlier step would otherwise satisfy the wait.

use std::thread;
use std::time::Duration;

use crate::guest::Guest;

/// The pause between typed bytes that a console which drops bursts can keep.
const PACE: Duration = Duration::from_millis(20);

/// Type `command` until the guest echoes the whole of it, up to `tries` times.
///
/// A line the guest never echoed whole is typed again; one it echoed is not.
/// The console is paced at [`PACE`], which is `tools/smoke/feed.sh`'s measured
/// value for a guest that mangles bursts.
pub fn type_line(guest: &mut Guest, command: &str, tries: u32) -> bool {
    let want = command.as_bytes();
    for _ in 0..tries {
        let start = guest.transcript_len();
        if guest.send_paced(command, PACE).is_err() {
            return false;
        }
        if guest.wait_for_after(want, start, Duration::from_secs(25)) {
            return true;
        }
        // A shorter line ran; let the shell come back before the next attempt.
        thread::sleep(Duration::from_millis(1500));
    }
    false
}

/// Type one shell command and wait for the shell to come back to its prompt.
///
/// The prompt is printed before the bytes are read, so the echo contains `# `
/// too; the wait starts from the echoed command, and the prompt after *that*
/// is the shell's next one.
pub fn run_line(guest: &mut Guest, command: &str, timeout: Duration) -> bool {
    let sent = guest.transcript_len();
    if guest.send_paced(command, PACE).is_err() {
        return false;
    }
    if !guest.wait_for_after(command.as_bytes(), sent, timeout) {
        return false;
    }
    let Some(at) = guest.rfind(command.as_bytes()) else {
        return false;
    };
    guest.wait_for_after(b"# ", at, timeout)
}

/// Type `command` until `want` appears, retyping a line the guest never echoed.
///
/// The echo is what says whether the line arrived, because this console mangles
/// lines: a truncated `sh: '/bin/wclient' not found` names a path nobody typed.
/// So "the client did not print PASS" alone cannot tell a client failure from a
/// bad send. A line the guest never echoed whole is typed again; one that *was*
/// echoed and did not pass is the client's own failure.
///
/// The echo check reads from one anchor taken before the first attempt, as
/// `tools/smoke/feed.sh` does, so a send that arrives late still counts and is
/// not typed twice. What is anchored per attempt is the prompt: the shell prints
/// it before the bytes, so a prompt seen before this attempt was typed says
/// nothing about where this one has got to.
pub fn type_line_and_wait(
    guest: &mut Guest,
    command: &str,
    want: &[u8],
    timeout: Duration,
    attempts: u32,
) -> bool {
    let anchor = guest.transcript_len();
    for _ in 0..attempts {
        let sent = guest.transcript_len();
        if guest.send_paced(command, PACE).is_err() {
            return false;
        }
        // Either the client answered, or the shell came back after running what
        // it received. The prompt only follows the command's completion, so this
        // returns on a mangled line too instead of spending the whole budget.
        guest.wait_until(
            |out| {
                contains(&out[anchor.min(out.len())..], want)
                    || contains(&out[sent.min(out.len())..], b"# ")
            },
            timeout,
        );
        if guest.wait_for_after(want, anchor, Duration::from_millis(1)) {
            return true;
        }
        if guest.wait_for_after(command.as_bytes(), anchor, Duration::from_millis(1)) {
            return false;
        }
        // The line arrived truncated or not at all, so the shell printed the path
        // it got and came back to a prompt. Type again after that answer rather
        // than into a running one.
        guest.wait_for_after(b"# ", sent, Duration::from_secs(5));
    }
    false
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn a_line_is_accepted_on_its_own_echo() {
        // `cat` echoes whatever it is given, which is the pipe arrangement the
        // probes use: bytes in, transcript out, no QEMU needed.
        let mut command = Command::new("cat");
        let mut guest = Guest::spawn(&mut command).unwrap();
        assert!(type_line(&mut guest, "/bin/diag wlscene", 1));
    }

    #[test]
    fn an_echoed_line_without_the_answer_is_a_failure() {
        let mut command = Command::new("cat");
        let mut guest = Guest::spawn(&mut command).unwrap();
        assert!(!type_line_and_wait(
            &mut guest,
            "hello",
            b"never printed",
            Duration::from_millis(200),
            1
        ));
    }
}
