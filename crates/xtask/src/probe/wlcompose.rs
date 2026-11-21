//! GPU composition across processes.
//!
//! `/bin/wlcompose serve` (one process) holds an EGL context and the Mesa DSOs;
//! `/bin/wlcompose test` (another) makes two GL surfaces on the render node,
//! hands their dma-buf descriptors over a UNIX-domain socket with `SCM_RIGHTS`,
//! and asks the helper to compose them into one frame. The helper imports the
//! peer's descriptors, draws them as quads, reads the frame back once
//! (`.notes/wayland.md` §6.10, D16, §5 row 27), and returns the frame's
//! descriptor; the peer imports that and checks the two rectangles and the
//! background.
//!
//! What this measures is **cross-process sampling**: whether a surface one
//! process rendered on the render node is visible to another process's context.
//! The peer only reaches `PASS` if it read the peer-drawn colours out of the
//! returned frame, so a `FAIL` with the pixel values is the measurement, not a
//! crash.
//!
//! Like the other Mesa gates this needs a GL-capable dev host (headless CI has
//! no GL). The recipe passes the QEMU line, which is the 4d gate's: a 2D
//! `virtio-gpu-pci` first and a GL device second, because those are the devices
//! the port's role selection (D11) has been validated against.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{command_from, console, lines_with, secs};
use crate::guest::Guest;

pub fn run(root: &Path, words: &[String]) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut command_from(words))?;
    if !guest.wait_for(b"# ", secs(240)) {
        eprintln!("NO PROMPT\n{}", guest.tail(2000));
        return Ok(false);
    }
    thread::sleep(Duration::from_secs(1));

    // The helper in the background, then the peer. The shell's `&` forks it; it
    // prints `ready` once it has bound, but the peer retries anyway, so no
    // ordering wait is needed here.
    if !console::type_line(&mut guest, "/bin/wlcompose serve &", 4) {
        eprintln!(
            "could not type /bin/wlcompose serve &: the console dropped bytes\n{}",
            guest.tail(2000)
        );
        return Ok(false);
    }
    thread::sleep(Duration::from_millis(1500));
    if !console::type_line(&mut guest, "/bin/wlcompose test", 4) {
        eprintln!(
            "could not type /bin/wlcompose test: the console dropped bytes\n{}",
            guest.tail(2000)
        );
        return Ok(false);
    }

    let passed = guest.wait_for(b"wlcompose: PASS", secs(120));
    // A GL display drops console bytes mid-line, so the captured stream is also
    // kept whole, for a failed run to be read back without the terminal's
    // mangling.
    let log = root.join("target/wlcompose.log");
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    guest.with_transcript(|out| std::fs::write(&log, out))?;
    for line in guest.with_transcript(|out| {
        let mut lines = lines_with(out, b"wlcompose:");
        lines.extend(lines_with(out, b"GL_RENDERER"));
        lines
    }) {
        println!("serial: {line}");
    }
    if !passed {
        eprintln!(
            "wlcompose: no PASS; the run is in {}",
            log.display()
        );
    }
    Ok(passed)
}
