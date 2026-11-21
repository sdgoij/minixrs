//! A GL client whose render target *is* the linux-dmabuf buffer scanned out.
//!
//! `/bin/wldmabufgl` renders a GLES2 triangle with Mesa on the render node into
//! a GBM buffer object (through an `EGLImage` over the BO's descriptor,
//! `tools/egl_bo_probe.c` is the measurement that this works on the port),
//! exports it to a dma-buf, and commits it as a `zwp_linux_dmabuf_v1` buffer.
//! `/sbin/wlserver` imports the object on the render node and hands its frames to
//! the fb server (`FBIO_SCANOUT_SRC`), so the frame *is* the client's object and
//! the compositor copies nothing (`.notes/wayland.md` §6.10, 3d-b-4d).
//!
//! Where this host departs from the design (`.notes/wayland.md` §5): its virgl
//! renders into a host-side texture, so the object's own pages only hold the
//! frame after the client resolves it and transfers it in, one copy, in the
//! client. 3d-a read the *whole* surface back with `glReadPixels`; 4d transfers
//! it into the buffer the compositor scans out, which is the compositor-side
//! saving.
//!
//! The QEMU line uses `-display egl-headless` and a **2D `virtio-gpu-pci` as the
//! first device**: the client renders on the GL node, and the screendump
//! captures the 2D device's scanout, where the re-pointed frame lands. The
//! recipe passes that line, since it needs a GL-capable host.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{command_from, console, lines_with, screen, secs};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4447;
const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];
const BLUE: [u8; 3] = [0, 0, 255];
const BLACK: [u8; 3] = [0, 0, 0];

pub fn run(root: &Path, words: &[String]) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut command_from(words))?;
    if !guest.wait_for(b"# ", secs(240)) {
        eprintln!("NO PROMPT");
        return Ok(false);
    }
    thread::sleep(Duration::from_secs(1));

    let mut qmp = Qmp::connect(QMP_PORT)?;
    let shot_path = root.join("target/wldmabufgl.ppm");

    // Baseline: the fb server's boot RGB pattern, so we know the 2D scanout is
    // live before the client re-points it.
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

    if !console::type_line(&mut guest, "/bin/wldmabufgl", 4) {
        eprintln!("could not type /bin/wldmabufgl: the console dropped bytes");
        return Ok(false);
    }
    if !guest.wait_for(b"wldmabufgl: PASS", secs(120)) {
        eprintln!("wldmabufgl did not reach PASS; tail:\n{}", guest.tail(2000));
        return Ok(false);
    }
    let took_path = guest.count(b"fb: scanout src") > 0 && guest.count(b"fb: scanout src refused") == 0;
    for line in guest.with_transcript(|out| {
        let mut lines = lines_with(out, b"wldmabufgl:");
        lines.extend(lines_with(out, b"fb: scanout"));
        lines.extend(lines_with(out, b"wlserver:"));
        lines
    }) {
        println!("serial: {line}");
    }
    println!("fb took the scanout path (no copy): {took_path}");

    thread::sleep(Duration::from_millis(500));
    let Some(shot) = screendump(&mut qmp, &shot_path) else {
        eprintln!("FINAL SCREENDUMP FAILED");
        return Ok(false);
    };
    let centre = shot.pixel(512, 384) == Some(RED);
    let corner = shot.pixel(0, 0) == Some(BLACK);
    println!("after commit: centre red={centre}, corner black={corner}");

    let ok = took_path && centre && corner;
    if !ok {
        eprintln!("{}", guest.tail(2000));
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
