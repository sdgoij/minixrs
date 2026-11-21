//! The framebuffer carries the boot test pattern.
//!
//! The fb server paints its first frame as three vertical thirds, red then green
//! then blue (`fill_test_pattern` in `crates/servers/src/fb.rs`), so a QMP
//! screendump is enough to prove the mode set and the BAR mapping end to end
//! with no input and no guest-side reporting. The pattern's own comment names
//! this check, and `/bin/fbmmap paint` keeps `restore` red for the same reason: a
//! probe run that leaves its own block behind would fail here.
//!
//! The console is only read for the prompt. A framebuffer that is never painted
//! still boots and still answers, so the pixels are the verdict.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{fail, qemu, secs, step};
use crate::guest::Guest;
use crate::probe::screen;

/// A port none of the other probes listens on.
const QMP_PORT: u16 = 4445;

pub fn run(root: &Path) -> io::Result<bool> {
    let guest = Guest::spawn(&mut qemu(root, QMP_PORT))?;
    if !guest.wait_for(b"# ", secs(60)) {
        return Ok(fail(&guest, "the guest never reached a shell prompt"));
    }
    // The prompt is printed by the shell, which starts after the fb server has
    // mapped the device; the wait is for the mode set to have reached the host.
    thread::sleep(Duration::from_secs(2));

    let mut qmp = crate::qmp::Qmp::connect(QMP_PORT)?;
    let frame = screen::screendump(&mut qmp, &root.join("target/fb.ppm"))?;
    println!("screen: {}x{}", frame.width, frame.height);

    let (w, h) = (frame.width, frame.height);
    let mut ok = true;
    for (name, got, want) in [
        ("left third", frame.pixel(0, 0), [255u8, 0, 0]),
        ("middle third", frame.pixel(w / 2, h / 2), [0, 255, 0]),
        ("right third", frame.pixel(w - 1, h - 1), [0, 0, 255]),
    ] {
        println!("  {name}: {got:?}");
        if got != Some(want) {
            eprintln!("error: the {name} is {got:?}, not {want:?}");
            ok = false;
        }
    }

    step(1, "the frame is the red/green/blue boot pattern", ok);
    Ok(ok)
}
