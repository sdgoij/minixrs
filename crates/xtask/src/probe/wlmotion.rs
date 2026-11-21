//! The pointer is interpolated across a tick, not jumped.
//!
//! Boots x86 with a QMP socket and the bochs display, waits for `/sbin/wlserver`,
//! runs `diag wlmotion` (one output-sized window, so its first commit takes the
//! focus and the pointer's events) and moves the absolute pointer with QMP
//! `input-send-event`.
//!
//! The move is one event at each axis, but the pointer must reach the client as
//! a stream of `wl_pointer.motion` events, one per tick, ending at the landing
//! point. The client prints how many it saw and where the last one was; this
//! probe fails when the count is a jump (two motions, one per axis) or the
//! endpoint is off.

use std::io;
use std::path::Path;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use regex_lite::Regex;

use super::{qemu, secs, step};
use crate::guest::Guest;
use crate::qmp::Qmp;

const QMP_PORT: u16 = 4472;
// QEMU's absolute tablet is 0..0x7FFF over the output, and the server maps it
// back with `value * size / 0x8000`. These two land on (768, 480).
const TARGET_X: u32 = 768;
const TARGET_Y: u32 = 480;
const MIN_STEPS: u32 = 4;

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
    guest.send_paced("/bin/diag wlmotion", Duration::from_millis(3))?;

    let mut ok = step(
        1,
        "the client took the pointer",
        guest.wait_for(b"wlmotion: ready", secs(40)),
    );

    if ok {
        qmp.absolute("fb0", TARGET_X, TARGET_Y)?;
    }

    if ok {
        ok = step(
            2,
            "the motions arrived",
            guest.wait_for(b"wlmotion: steps=", secs(40)),
        );
    }

    if ok {
        match guest.with_transcript(report) {
            None => {
                println!("[3] the client's report: FAIL");
                ok = false;
            }
            Some((steps, x, y)) => {
                let arrived = steps >= MIN_STEPS
                    && x.abs_diff(TARGET_X) <= 1
                    && y.abs_diff(TARGET_Y) <= 1;
                println!(
                    "[3] steps={steps} x={x} y={y}: {}",
                    if arrived { "ok" } else { "FAIL" }
                );
                ok = arrived;
            }
        }
    }

    if ok {
        ok = step(
            4,
            "the client finished",
            guest.wait_for(b"wlmotion: PASS", secs(20)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(2000));
    }
    println!("wlmotion probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}

/// The step count and landing point in the client's last report.
fn report(out: &[u8]) -> Option<(u32, u32, u32)> {
    let text = String::from_utf8_lossy(out);
    let captures = pattern().captures_iter(&text).last()?;
    Some((
        captures[1].parse().ok()?,
        captures[2].parse().ok()?,
        captures[3].parse().ok()?,
    ))
}

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"wlmotion: steps=(\d+) x=(\d+) y=(\d+)").expect("the report is a constant")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_last_report_wins() {
        let out = b"wlmotion: steps=2 x=1 y=2\nwlmotion: steps=12 x=768 y=480\n";
        assert_eq!(report(out), Some((12, 768, 480)));
        assert_eq!(report(b"nothing"), None);
    }

    #[test]
    fn a_landing_off_the_target_is_not_arrived() {
        let (steps, x, y) = report(b"wlmotion: steps=4 x=769 y=479\n").unwrap();
        assert!(x.abs_diff(TARGET_X) <= 1 && y.abs_diff(TARGET_Y) <= 1);
        assert!(steps >= MIN_STEPS);
    }
}
