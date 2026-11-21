//! A whole-output `wl_shm` frame presented by re-pointing the scanout.
//!
//! `diag wlclient full` draws a 1024x768 pool and commits it at the output's own
//! size. `/sbin/wlserver` hands that buffer to the fb server (`FBIO_SCANOUT_SRC`)
//! instead of blitting it, so the virtio-gpu scanout becomes the client's memory
//! and the compositor copies nothing (`.notes/wayland.md` 6.10, 3d-b). The frame
//! is then *the pool*, so `/dev/fb` no longer holds it; this probe checks the
//! display from the host instead: a QMP screendump must be the client's colour,
//! and the serial log must carry the fb server's `fb: scanout src` line proving
//! the path was taken.
//!
//! The QEMU line uses `virtio-gpu-pci` as the display, with no `bochs-display`:
//! the re-point is a virtio-gpu capability, so the output device has to *be* the
//! virtio-gpu one (D11). The recipe passes that line, because the disk image and
//! the devices differ per arch.
//!
//! The session starts `diag wlpanel` before anything is typed, and a panel is a
//! second surface: with two in the scene the server composes the frame into its
//! own buffer instead of handing the client's buffer to the scanout, which is
//! the path this gate exists to measure. So the probe ends the panel first, by
//! the pid its ready line reports (moving a pointer onto it would need a pointer
//! device, and riscv64/aarch64 are booted with none), and goes on once the shell
//! is back.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use super::{command_from, console, lines_with, screen, secs};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4447;
// The client runs at the output's own size: whole-output, so the server can
// scan the pool out.
const COMMAND: &str = "/bin/diag wlclient full";
const MAGENTA: [u8; 3] = [255, 0, 255];

pub fn run(root: &Path, words: &[String]) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut command_from(words))?;
    if !guest.wait_for(b"# ", secs(240)) {
        eprintln!("NO PROMPT");
        return Ok(false);
    }
    thread::sleep(Duration::from_secs(1));

    let mut qmp = Qmp::connect(QMP_PORT)?;

    if !guest.wait_for(b"wlpanel: ready pid=", secs(60)) {
        eprintln!("NO PANEL");
        return Ok(false);
    }
    let Some(pid) = panel_pid(&guest) else {
        eprintln!("THE PANEL NAMED NO PID");
        return Ok(false);
    };
    if pid == 0 {
        eprintln!("THE PANEL NAMED NO PID");
        return Ok(false);
    }
    if !console::run_line(&mut guest, &format!("kill {pid}"), secs(15)) {
        eprintln!("THE PANEL COULD NOT BE KILLED");
        return Ok(false);
    }
    // The server drops the surface when the connection closes, which it notices
    // on its next poll; the client below is what proves whether it did.
    thread::sleep(Duration::from_secs(1));

    // Baseline: the display is live and does not hold the client's frame yet.
    // The fb server's boot pattern used to be the marker, but the panel paints
    // inside it, so what is asserted here is the property the final check needs:
    // the magenta below is the client's, not something that was already up.
    let shot_path = root.join("target/wlfull.ppm");
    let Some(base) = screendump(&mut qmp, &shot_path) else {
        eprintln!("BASELINE SCREENDUMP FAILED");
        return Ok(false);
    };
    let base_ok = base.pixel(512, 384) != Some(MAGENTA) && base.pixel(0, 0) != Some(MAGENTA);
    println!(
        "baseline centre {:?} corner {:?}",
        base.pixel(512, 384),
        base.pixel(0, 0)
    );
    if !base_ok {
        eprintln!("BASELINE RGB MISMATCH");
        return Ok(false);
    }

    // A whole-output client: its buffer is the output's size, so the server can
    // scan it out.
    if !console::type_line_and_wait(&mut guest, COMMAND, b"wlclient: PASS", secs(40), 4) {
        eprintln!("wlclient full did not reach PASS; tail:\n{}", guest.tail(1000));
        return Ok(false);
    }
    let took_path = guest.count(b"fb: scanout src") > 0 && guest.count(b"fb: scanout src refused") == 0;
    for line in guest.with_transcript(|out| lines_with(out, b"fb: scanout")) {
        println!("serial: {line}");
    }
    println!("fb took the scanout path: {took_path}");

    thread::sleep(Duration::from_millis(500));
    let Some(shot) = screendump(&mut qmp, &shot_path) else {
        eprintln!("FINAL SCREENDUMP FAILED");
        return Ok(false);
    };
    // The pool is magenta: 0x00FF00FF little-endian is B=FF,G=00,R=FF.
    let centre = shot.pixel(512, 384) == Some(MAGENTA);
    let corner = shot.pixel(0, 0) == Some(MAGENTA);
    println!("after commit: centre magenta={centre}, corner magenta={corner}");

    let ok = took_path && centre && corner;
    if !ok {
        eprintln!("{}", guest.tail(1000));
    }
    Ok(ok)
}

/// One frame, with the failure reported where it happened.
fn screendump(qmp: &mut Qmp, path: &Path) -> Option<screen::Screen> {
    match screen::screendump(qmp, path) {
        Ok(screen) => Some(screen),
        Err(error) => {
            eprintln!("screendump: {error}");
            None
        }
    }
}

/// The pid the panel's ready line reports.
fn panel_pid(guest: &Guest) -> Option<u32> {
    const MARKER: &str = "wlpanel: ready pid=";
    guest.with_transcript(|out| {
        let text = String::from_utf8_lossy(out);
        let at = text.rfind(MARKER)?;
        let tail = &text[at + MARKER.len()..];
        let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_panel_line_names_the_pid() {
        let mut command = std::process::Command::new("cat");
        let mut guest = Guest::spawn(&mut command).unwrap();
        guest
            .send_paced("wlpanel: ready pid=12", Duration::from_millis(1))
            .unwrap();
        assert!(guest.wait_for(b"wlpanel: ready pid=12", secs(5)));
        assert_eq!(panel_pid(&guest), Some(12));
    }
}
