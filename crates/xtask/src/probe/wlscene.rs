//! Two dma-buf surfaces composed on the GPU by a helper process, then presented.
//!
//! `diag wlscene` commits **two** render-node objects as
//! `zwp_linux_dmabuf_v1` buffers on one connection: a full-output red background
//! and a 128x128 green overlay at the output's origin. `/sbin/wlserver` keeps
//! them in its scene; once there is more than one surface it asks
//! `/bin/wlcompose serve` (a separate, stateless Mesa process on the same render
//! node) to draw them into one frame, imports that frame, and scans it out
//! (`FBIO_SCANOUT_SRC`). The first commit, with a lone surface, stays on the
//! per-commit fast path.
//!
//! Evidence checked here: both commits reach `wlscene: PASS`; the fb server took
//! the scan-out path (`fb: scanout src`, no `refused`); and a QMP screendump
//! shows the overlay's green at the origin over the background's red *everywhere
//! else*, including where the boot pattern's green and blue were. That last one
//! is a two-surface frame: the compositor's per-commit present could only have
//! shown one source plane, and the server's own buffer holds the boot pattern,
//! not the background. The compositor's `wlserver: composed N surfaces` line is
//! printed too, and reported when it survives the console; a GL display drops
//! console bytes, so the pixels, not the line, are the proof.

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

pub fn run(root: &Path, words: &[String]) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut command_from(words))?;
    if !guest.wait_for(b"# ", secs(240)) {
        eprintln!("NO PROMPT\n{}", guest.tail(2000));
        return Ok(false);
    }
    thread::sleep(Duration::from_secs(1));

    let mut qmp = Qmp::connect(QMP_PORT)?;
    let shot_path = root.join("target/wlscene.ppm");

    // Baseline: the fb server's boot RGB pattern, so we know the scanout is live
    // before anything re-points it.
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

    // The helper in the background, then the two-surface client. The shell's `&`
    // forks the helper; `wlserver` connects to it lazily on the first
    // multi-surface frame.
    if !console::type_line(&mut guest, "/bin/wlcompose serve &", 4) {
        eprintln!(
            "could not type /bin/wlcompose serve &: the console dropped bytes\n{}",
            guest.tail(2000)
        );
        return Ok(false);
    }
    thread::sleep(Duration::from_millis(1500));
    if !console::type_line(&mut guest, "/bin/diag wlscene", 4) {
        eprintln!(
            "could not type diag wlscene: the console dropped bytes\n{}",
            guest.tail(2000)
        );
        return Ok(false);
    }

    if !guest.wait_for(b"wlscene: PASS", secs(150)) {
        eprintln!("wlscene did not reach PASS\n{}", guest.tail(2000));
        return Ok(false);
    }
    let composed = guest.count(b"wlserver: composed 2 surfaces") > 0;
    let took_path = guest.count(b"fb: scanout src") > 0 && guest.count(b"fb: scanout src refused") == 0;

    // A GL display drops console bytes mid-line, so the captured stream is kept
    // whole for a failed run to be read back without the terminal's mangling.
    let log = root.join("target/wlscene.log");
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    guest.with_transcript(|out| std::fs::write(&log, out))?;
    for line in guest.with_transcript(|out| {
        let mut lines = lines_with(out, b"wlscene:");
        lines.extend(lines_with(out, b"wlserver:"));
        lines.extend(lines_with(out, b"fb: scanout"));
        lines.extend(lines_with(out, b"wlcompose:"));
        lines
    }) {
        println!("serial: {line}");
    }
    println!("composed 2 surfaces (serial, lossy): {composed}");
    println!("fb took the scanout path (no copy): {took_path}");

    thread::sleep(Duration::from_millis(500));
    let Some(shot) = screendump(&mut qmp, &shot_path) else {
        eprintln!("FINAL SCREENDUMP FAILED");
        return Ok(false);
    };
    // The overlay is a 128x128 square at the origin; the background is red
    // everywhere else, and the boot pattern's green (512,100) and blue (1023,767)
    // are gone with it.
    let overlay = shot.pixel(2, 2) == Some(GREEN);
    let bg_centre = shot.pixel(512, 384) == Some(RED);
    let bg_boot_green = shot.pixel(512, 100) == Some(RED);
    let bg_boot_blue = shot.pixel(1023, 767) == Some(RED);
    println!(
        "after composition: overlay green={overlay}, background red = {bg_centre} centre, \
         {bg_boot_green} at the boot green, {bg_boot_blue} at the boot blue"
    );

    let ok = took_path && overlay && bg_centre && bg_boot_green && bg_boot_blue;
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
