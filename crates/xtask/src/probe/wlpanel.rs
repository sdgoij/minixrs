//! The panel is a layer surface, and its launcher takes a click.
//!
//! Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`.
//! The panel is started by the session: `/etc/rc` starts `/bin/session`, which
//! waits for the shell's prompt and then starts `/etc/session`'s list, so
//! nothing is typed here. The panel is a `zwlr_layer_shell_v1` strip at the
//! output's right edge with one button; the probe moves the absolute pointer
//! into that button, clicks it, and expects the client's launch line and then
//! the app's.
//!
//! The app's window covers the left of the output and the panel is on the right,
//! so the second click is a plain repeat. The pointer also owns the keyboard
//! focus, so leaving it over the panel and pressing Escape ends the panel.
//!
//! The panel's placement is checked on the display, not only through the
//! protocol: a QMP screendump has to show the strip at the right edge with the
//! boot pattern's red still at the origin.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::screen;
use super::{qemu, secs, step};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4475;
// The output and the panel's strip at its right edge (see `diag wlpanel`).
const OUT_W: usize = 1024;
const PANEL_W: usize = 200;

const BOOT_RED: [u8; 3] = [255, 0, 0];
const BOOT_BLUE: [u8; 3] = [0, 0, 255];

pub fn run(root: &Path) -> io::Result<bool> {
    let guest = Guest::spawn(&mut qemu(root, QMP_PORT))?;
    if !guest.wait_for(b"wlserver: ready", secs(150)) {
        eprintln!("WLSERVER NOT READY\n{}", guest.tail(800));
        return Ok(false);
    }
    if !guest.wait_for(b"# ", secs(30)) {
        eprintln!("NO SHELL PROMPT");
        return Ok(false);
    }
    thread::sleep(Duration::from_secs(1));

    let mut qmp = Qmp::connect(QMP_PORT)?;
    let shot_path = root.join("target/wlpanel.ppm");

    let mut ok = step(1, "the panel is up", guest.wait_for(b"wlpanel: ready", secs(40)));

    // Where the panel's frame lands, checked on the display rather than through
    // the protocol: the server blits a commit at the surface's place in the
    // scene, so the strip has to appear at the output's right edge. A server
    // that painted every commit at (0, 0) put the panel in the top-left corner,
    // where the pointer gates could not see it and the boot pattern's red was
    // gone from the origin.
    let mut painted = false;
    for _ in 0..10 {
        if !ok {
            break;
        }
        if let Ok(screen) = screen::screendump(&mut qmp, &shot_path) {
            painted = screen.pixel(0, 0) == Some(BOOT_RED)
                && screen.pixel(OUT_W - PANEL_W + 150, 16) != Some(BOOT_BLUE);
            if painted {
                break;
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
    ok = step(2, "the panel is drawn at the output's right edge", painted);

    // The session starts the panel, so nothing is typed here. Into the launcher
    // button, which is 4..76 by 4..28 in panel coordinates: the panel is 200
    // wide at the output's right edge, so the click lands at 1024 - 200 + 20.
    if ok {
        click(&mut qmp, OUT_W - PANEL_W + 20, 16)?;
    }
    ok = step(
        3,
        "the launcher took the click",
        wait_for_count(&guest, b"wlpanel: launch", 1, secs(40)),
    );

    if ok {
        ok = step(
            4,
            "the launcher started the app",
            guest.wait_for(b"wldemo: ready", secs(40)),
        );
    }

    // The app's window covers the left of the output; the panel is on the right.
    // Moving a little and clicking again must still reach the launcher.
    if ok {
        click(&mut qmp, OUT_W - PANEL_W + 30, 16)?;
    }
    if ok {
        ok = step(
            5,
            "a second click reached the panel again",
            wait_for_count(&guest, b"wlpanel: launch", 2, secs(40)),
        );
    }
    if ok {
        ok = step(
            6,
            "the second app started",
            wait_for_count(&guest, b"wldemo: ready", 2, secs(40)),
        );
    }

    // The app the second click started took the focus when it mapped.
    if ok {
        tap_key(&mut qmp, "esc")?;
        ok = step(
            7,
            "the second app finished",
            wait_for_count(&guest, b"wldemo: PASS", 1, secs(20)),
        );
    }

    // Nothing is focused now; a small move over the panel is what hands it back.
    if ok {
        qmp.absolute("fb0", OUT_W as u32 - PANEL_W as u32 + 40, 16)?;
        thread::sleep(Duration::from_secs(1));
        tap_key(&mut qmp, "esc")?;
        ok = step(
            8,
            "the panel finished",
            guest.wait_for(b"wlpanel: PASS", secs(20)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(3000));
    }
    println!("wlpanel probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}

/// Move onto a pixel, let the sweep settle, then click once.
fn click(qmp: &mut Qmp, x: usize, y: usize) -> io::Result<()> {
    qmp.absolute("fb0", x as u32, y as u32)?;
    thread::sleep(Duration::from_secs(1));
    qmp.button("left", true)?;
    thread::sleep(Duration::from_millis(100));
    qmp.button("left", false)
}

fn tap_key(qmp: &mut Qmp, name: &str) -> io::Result<()> {
    qmp.key(name, true)?;
    thread::sleep(Duration::from_millis(100));
    qmp.key(name, false)
}

/// Wait until `needle` has been printed `at_least` times.
fn wait_for_count(guest: &Guest, needle: &[u8], at_least: usize, timeout: Duration) -> bool {
    guest.wait_until(
        |out| out.windows(needle.len()).filter(|window| *window == needle).count() >= at_least,
        timeout,
    )
}
