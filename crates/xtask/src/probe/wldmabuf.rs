//! A linux-dmabuf client's render-node object scanned out with no copy.
//!
//! `diag wldmabuf` makes a full-output (1024x768 B8G8R8X8) object on the render
//! node, fills it green, exports it to a dma-buf, and commits it as a
//! `zwp_linux_dmabuf_v1` buffer. `/sbin/wlserver` imports the object on the same
//! device and hands its frames to the fb server (`FBIO_SCANOUT_SRC`), so the
//! scanout *is* the client's object and the compositor copies nothing
//! (`.notes/wayland.md` 6.10, 3d-b-4). The frame is then the object, so
//! `/dev/fb` no longer holds it; this probe checks the display from the host
//! instead: a QMP screendump must be the client's colour, and the serial log
//! must carry the fb server's `fb: scanout src` line proving the path was taken.
//!
//! The QEMU line uses `virtio-gpu-pci` as the display and no `bochs-display`:
//! the render node is served from the output device itself (D11), so the client's
//! render resource and the scanout are one device, which is what lets the scanout
//! be re-pointed at it. The recipe passes that line.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{command_from, lines_with, screen, secs};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4447;
const GREEN: [u8; 3] = [0, 255, 0];
const RED: [u8; 3] = [255, 0, 0];
const BLUE: [u8; 3] = [0, 0, 255];

pub fn run(root: &Path, words: &[String]) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut command_from(words))?;
    if !guest.wait_for(b"# ", secs(240)) {
        eprintln!("NO PROMPT");
        return Ok(false);
    }
    thread::sleep(Duration::from_secs(1));

    let mut qmp = Qmp::connect(QMP_PORT)?;
    let shot_path = root.join("target/wldmabuf.ppm");

    // Baseline: the fb server's boot RGB pattern, so we know the display is live
    // before the client re-points it.
    let Some(base) = screendump(&mut qmp, &shot_path) else {
        eprintln!("BASELINE SCREENDUMP FAILED");
        return Ok(false);
    };
    let base_ok = base.pixel(0, 0) == Some(RED)
        && base.pixel(512, 100) == Some(GREEN)
        && base.pixel(1023, 767) == Some(BLUE);
    println!("baseline rgb: {base_ok}");
    if !base_ok {
        eprintln!("BASELINE RGB MISMATCH");
        return Ok(false);
    }

    guest.send_paced("/bin/diag wldmabuf", Duration::from_millis(3))?;
    if !guest.wait_for(b"wldmabuf: PASS", secs(60)) {
        eprintln!("wldmabuf did not reach PASS; tail:\n{}", guest.tail(1500));
        return Ok(false);
    }
    let took_path = guest.count(b"fb: scanout src") > 0 && guest.count(b"fb: scanout src refused") == 0;
    for line in guest.with_transcript(|out| {
        let mut lines = lines_with(out, b"fb: scanout");
        lines.extend(lines_with(out, b"wlserver:"));
        lines
    }) {
        println!("serial: {line}");
    }
    println!("fb took the scanout path: {took_path}");

    thread::sleep(Duration::from_millis(500));
    let Some(shot) = screendump(&mut qmp, &shot_path) else {
        eprintln!("FINAL SCREENDUMP FAILED");
        return Ok(false);
    };
    let centre = shot.pixel(512, 384) == Some(GREEN);
    let corner = shot.pixel(0, 0) == Some(GREEN);
    println!("after commit: centre green={centre}, corner green={corner}");

    let ok = took_path && centre && corner;
    if !ok {
        eprintln!("{}", guest.tail(1500));
    }
    Ok(ok)
}

fn screendump(qmp: &mut Qmp, path: &Path) -> Option<screen::Screen> {
    match screen::screendump(qmp, path) {
        Ok(screen) => Some(screen),
        Err(error) => {
            eprintln!("screendump: {error}");
            None
        }
    }
}
