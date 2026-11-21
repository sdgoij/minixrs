//! A host key reaches a Wayland client, and the client gets its keymap.
//!
//! Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
//! then runs `diag wlkey` and injects `a` with QMP `input-send-event`. 30 is
//! evdev's `KEY_A`, so a match proves the whole path: the PS/2 controller queues
//! an HID usage, the input server's ring hands it to a reader of its own
//! (wlserver, alongside wserver), wlserver translates the usage to a keycode, and
//! `wl_keyboard.key` carries it to the client. The keymap line before it proves
//! the keymap event's fd carried the committed XKB text.
//!
//! Unlike the tsv gates this cannot be driven through the shell's stdin: that is
//! the serial console, and the input server never sees it. The key has to be a
//! real device event, which is what QMP injects.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{qemu, secs, step};
use crate::guest::Guest;
use crate::qmp::Qmp;

pub fn run(root: &Path) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut qemu(root, 4470))?;

    if !guest.wait_for(b"wlserver: ready", secs(150)) {
        eprintln!("WLSERVER NOT READY\n{}", guest.tail(800));
        return Ok(false);
    }
    if !guest.wait_for(b"# ", secs(30)) {
        eprintln!("NO SHELL PROMPT");
        return Ok(false);
    }
    thread::sleep(Duration::from_millis(500));

    let mut qmp = Qmp::connect(4470)?;
    guest.send_paced("/bin/diag wlkey", Duration::from_millis(3))?;

    let mut ok = true;
    if ok {
        ok = step(
            1,
            "wlkey keymap 64757 ok (Phase 2a)",
            guest.wait_for(b"wlkey: keymap 64757 ok", secs(30)),
        );
    }
    if ok {
        ok = step(2, "wlkey ready", guest.wait_for(b"wlkey: ready", secs(30)));
    }
    if ok {
        // Press then release, the way a keyboard sends a key.
        qmp.key("a", true)?;
        thread::sleep(Duration::from_millis(200));
        qmp.key("a", false)?;
        ok = step(
            3,
            "key 30 pressed reached the client",
            guest.wait_for(b"wlkey: key 30 pressed", secs(30)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(1500));
    }
    println!("wlkey probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}
