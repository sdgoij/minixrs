//! A window of widgets, driven by the keyboard.
//!
//! Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
//! runs `diag wldemo` (a text field above a list above a button, drawn by the
//! software backend) and types into it.
//!
//! The client prints its state after every change, so the probe asserts the
//! interaction step by step instead of guessing at sleeps: two letters reach the
//! field, a held shift uppercases the next key, Tab moves the focus to the list,
//! two Downs select the third row, Tab moves it to the button, Enter fires it,
//! and Escape ends the client.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{qemu, secs, step};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4474;

pub fn run(root: &Path) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut qemu(root, QMP_PORT))?;
    if !guest.wait_for(b"wlserver: ready", secs(150)) {
        eprintln!("WLSERVER NOT READY\n{}", guest.tail(800));
        return Ok(false);
    }
    if !guest.wait_for(b"# ", secs(30)) {
        eprintln!("NO SHELL PROMPT");
        return Ok(false);
    }
    thread::sleep(Duration::from_millis(500));

    let mut qmp = Qmp::connect(QMP_PORT)?;
    guest.send_paced("/bin/diag wldemo", Duration::from_millis(3))?;

    let mut ok = step(1, "the window is up", guest.wait_for(b"wldemo: ready", secs(40)));
    if ok {
        ok = step(
            2,
            "the field starts focused and empty",
            guest.wait_for(report(0, "", 0, 0).as_bytes(), secs(40)),
        );
    }

    // The field has the focus first, so the letters go straight in.
    if ok {
        tap(&mut qmp, "h")?;
        tap(&mut qmp, "i")?;
        ok = step(
            3,
            "the letters reached the field",
            guest.wait_for(report(0, "hi", 0, 0).as_bytes(), secs(40)),
        );
    }

    // Shift comes from the seat's modifiers event, so a held shift has to change
    // the byte the next key produces.
    if ok {
        qmp.key("shift", true)?;
        tap(&mut qmp, "a")?;
        qmp.key("shift", false)?;
        ok = step(
            4,
            "shift uppercased the letter it was held for",
            guest.wait_for(report(0, "hiA", 0, 0).as_bytes(), secs(40)),
        );
    }

    if ok {
        tap(&mut qmp, "tab")?;
        ok = step(
            5,
            "Tab moved the focus to the list",
            guest.wait_for(report(1, "hiA", 0, 0).as_bytes(), secs(40)),
        );
    }

    if ok {
        tap(&mut qmp, "down")?;
        tap(&mut qmp, "down")?;
        ok = step(
            6,
            "two Downs moved the selection to the third row",
            guest.wait_for(report(1, "hiA", 2, 0).as_bytes(), secs(40)),
        );
    }

    if ok {
        tap(&mut qmp, "tab")?;
        ok = step(
            7,
            "Tab moved the focus to the button",
            guest.wait_for(report(2, "hiA", 2, 0).as_bytes(), secs(40)),
        );
    }

    if ok {
        tap(&mut qmp, "ret")?;
        ok = step(
            8,
            "Enter fired the button",
            guest.wait_for(report(2, "hiA", 2, 1).as_bytes(), secs(40)),
        );
    }

    if ok {
        tap(&mut qmp, "esc")?;
        ok = step(
            9,
            "the client finished",
            guest.wait_for(b"wldemo: PASS", secs(20)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(3000));
    }
    println!("wldemo probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}

/// One key press and release, the way a keyboard sends a key.
fn tap(qmp: &mut Qmp, name: &str) -> io::Result<()> {
    qmp.key(name, true)?;
    thread::sleep(Duration::from_millis(40));
    qmp.key(name, false)
}

/// The client's state line. No trailing newline: the guest's tty turns `\n`
/// into `\r\n`, so a predicate that ended with one would never match.
fn report(focus: u32, field: &str, selected: u32, button: u32) -> String {
    format!("wldemo: focus={focus} field=\"{field}\" list={selected} button={button}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_line_is_reported_without_a_newline() {
        assert_eq!(
            report(0, "hiA", 2, 1),
            "wldemo: focus=0 field=\"hiA\" list=2 button=1"
        );
        assert!(!report(0, "", 0, 0).ends_with('\n'));
    }
}
