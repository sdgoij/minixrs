//! Two Wayland clients, one focus, and the keys that follow it.
//!
//! Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
//! then runs `diag wlx2` and injects keys with QMP `input-send-event`. The second
//! window appears last, so it must take the focus and the first key must reach
//! only it; the first connection then maps another window, the focus must move
//! back, and the second key must follow it. A key that reaches the wrong client
//! fails the gate, which is what turns "input goes to the focused surface" from
//! an inference into a measurement.
//!
//! Unlike the tsv gates this cannot be driven through the shell's stdin: that is
//! the serial console, and the input server never sees it.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{qemu, secs, step};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4471;

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
    guest.send_paced("/bin/diag wlx2", Duration::from_millis(3))?;

    let mut ok = step(
        1,
        "two clients mapped, focus moved to the second",
        guest.wait_for(b"wlx2: ready", secs(40)),
    );
    if ok {
        qmp.press("a")?;
        ok = step(
            2,
            "the first key reached the focused client",
            guest.wait_for(b"wlx2: key 30 to B", secs(40)),
        );
    }
    if ok {
        ok = step(
            3,
            "a second window moved the focus back",
            guest.wait_for(b"wlx2: focus A", secs(40)),
        );
    }
    if ok {
        qmp.press("a")?;
        ok = step(
            4,
            "the second key followed the focus",
            guest.wait_for(b"wlx2: key 30 to A", secs(40)),
        );
    }
    if ok {
        ok = step(
            5,
            "the client finished",
            guest.wait_for(b"wlx2: PASS", secs(20)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(2000));
    }
    println!("wlfocus probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}
