//! The focus follows the window under the pointer.
//!
//! Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
//! runs `diag wlpointer` (a full-output window below, a 64x64 window above it at
//! the origin) and moves the absolute pointer. Near the top-left it is over both,
//! so the small window on top must take the focus; further out it is over only
//! the big one, so the focus must come back. A key is injected last, and the
//! connection that receives it is the window that held the focus at the pointer's
//! final place.
//!
//! Neither connection's slot number is known up front, because the session's
//! panel is a client too and takes whichever slot it connected on. So the gate
//! tracks the slot each move ends on, asserts they differ, then that moving back
//! returns to the first.

use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use regex_lite::Regex;

use super::{qemu, secs, step};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4473;

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
    guest.send_paced("/bin/diag wlpointer", Duration::from_millis(3))?;

    let mut ok = step(
        1,
        "the two windows are up",
        guest.wait_for(b"wlpointer: ready", secs(40)),
    );

    // Out to the full-output window, into the 64x64 one, then out again. The
    // pointer starts at the centre, over the big window only, and the small one
    // holds the focus it asked for when it committed last, so the first move is
    // also the one that proves the handover away from it.
    if ok {
        qmp.absolute("fb0", 400, 300)?;
        let below = settle(&guest);
        ok = step(2, "the big window alone holds the focus", below.is_some());
        if ok {
            qmp.absolute("fb0", 16, 16)?;
            let top = settle(&guest);
            ok = step(
                3,
                "the small window took the focus",
                top.is_some() && top != below,
            );
            if ok {
                qmp.absolute("fb0", 400, 300)?;
                let back = settle(&guest);
                ok = step(4, "the big window took it back", back == below);
            }
        }
    }

    // A key injected right after a move is dropped by the guest's input ring, so
    // it comes last and proves only where the focus ended up.
    if ok {
        qmp.press("a")?;
        ok = step(
            5,
            "the key followed the pointer",
            guest.wait_for(b"wlpointer: key to A", secs(40)),
        );
    }
    if ok {
        ok = step(
            6,
            "the client finished",
            guest.wait_for(b"wlpointer: PASS", secs(20)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(2000));
    }
    println!("wlpointer probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}

/// The slot a move ended on, once its interpolation has finished.
///
/// A move arrives as one target position but is drawn as a short sweep, one step
/// per tick, so the handovers it causes arrive over about half a second and the
/// last of them names the window under the pointer's final place. Reading the
/// first handover would catch the sweep passing over an earlier window.
fn settle(guest: &Guest) -> Option<u32> {
    thread::sleep(Duration::from_secs(2));
    guest.with_transcript(|out| focus_slots(out).last().copied())
}

/// Every focus handover reported so far, in order.
fn focus_slots(out: &[u8]) -> Vec<u32> {
    let text = String::from_utf8_lossy(out);
    focus_pattern()
        .captures_iter(&text)
        .filter_map(|captures| captures[1].parse().ok())
        .collect()
}

fn focus_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"wlserver: focus -> slot (\d+)").expect("the focus pattern is a constant")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handovers_are_read_in_order() {
        let out = b"boot\nwlserver: focus -> slot 3\nnoise\n\
                    wlserver: focus -> slot 5\nwlserver: focus -> slot 3\n";
        assert_eq!(focus_slots(out), vec![3, 5, 3]);
        assert!(focus_slots(b"nothing here").is_empty());
    }
}
