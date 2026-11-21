//! The session chain: `/etc/rc`, the shell's prompt, `/etc/session`.
//!
//! Boots x86 with the bochs display and types nothing at all. Three things have
//! to happen on their own, in order: `/bin/rc` runs `/etc/rc`, which names the
//! session manager; the shell prints its prompt as a line of its own, and the
//! manager reports what it started without either landing inside the other; and
//! the panel from `/etc/session` reaches its ready line.
//!
//! The prompt check is the one that matters most: a prompt with text after it is
//! not a prompt to anything reading the console, and the shell's own terminator
//! is what keeps that true however many programs share the console.

use std::io;
use std::path::Path;

use super::{qemu, secs, step};
use crate::guest::Guest;

pub fn run(root: &Path) -> io::Result<bool> {
    let guest = Guest::spawn(&mut qemu(root, 4476))?;

    let mut ok = true;
    if ok {
        ok = step(
            1,
            "the rc list started the session manager",
            guest.wait_for(b"session: started 1", secs(150)),
        );
    }
    if ok {
        ok = step(
            2,
            "the prompt is a line of its own",
            guest.wait_until(has_prompt, secs(30)),
        );
    }
    if ok {
        ok = step(
            3,
            "the session's panel came up",
            guest.wait_for(b"wlpanel: ready", secs(60)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(3000));
    }
    println!("rc probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}

/// A prompt on a line of its own: `#`, spaces, and the session manager's
/// newline. The guest's tty turns `\n` into `\r\n`, so a trailing `\r` is part of
/// the line, not of the prompt.
fn has_prompt(out: &[u8]) -> bool {
    out.split(|byte| *byte == b'\n').any(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        line.first() == Some(&b'#') && line[1..].iter().all(|byte| *byte == b' ')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_is_a_line_of_its_own() {
        assert!(has_prompt(b"boot\n# \r\nnext\n"));
        assert!(has_prompt(b"#\n"));
        assert!(has_prompt(b"#   \r\n"));
        assert!(!has_prompt(b"# wlpanel: ready\n"));
        assert!(!has_prompt(b"a # b\n"));
        assert!(!has_prompt(b""));
    }
}
