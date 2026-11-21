//! Run N execs and sample the kernel's free-page and region counts.
//!
//! The repeated-exec leak (`.agents/skills/bare-metal-debug`, "Repeated-Exec
//! Leak Hunting") shows up as free pages falling a little on every exec, or as a
//! wedge at a suspiciously round exec count. Neither needs a debugger: `memstat`
//! reports the kernel's free pages and `regions` reports VM's region count, so a
//! loop that runs the command N times and samples every K isolates both per exec.
//!
//! The driver never samples past an exec that has not finished. An exec is done
//! when its own output has arrived, its completion line has arrived, and the
//! shell has printed the next prompt, all three *after* the command was sent: a
//! marker matched anywhere in the transcript stale-matches the previous exec and
//! runs the loop ahead of the guest, which reads as a wedge that never happened.
//! A real wedge is reported by sending `hangdump`, so the blocked process is
//! named rather than guessed at.
//!
//! A `memstat` that does not answer is retried once, because a write that lands
//! while the guest is mid-output is a miss, not a hang.

use std::cell::Cell;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

use regex_lite::Regex;

use super::{fail, secs, send_command, step};
use crate::guest::Guest;

const PROMPT: &[u8] = b"# ";
/// A leak over this many pages per exec (64 KiB) fails the run.
const LEAK_CEILING: f64 = 16.0;
/// Slack for transient churn in VM's region accounting.
const REGION_CEILING: f64 = 4.0;
const REGION_PAGES_CEILING: f64 = 16.0;

pub struct Options<'a> {
    pub arch: &'a str,
    pub memory: &'a str,
    pub execs: u64,
    pub sample_every: u64,
    pub command: &'a str,
    pub marker: &'a str,
    pub complete: &'a str,
}

pub fn run(root: &Path, options: &Options) -> io::Result<bool> {
    let image = image(root, options.arch)?;
    if !image.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no image at {} - build it first", image.display()),
        ));
    }

    let mut guest = Guest::spawn(&mut qemu(options.arch, options.memory, &image))?;
    if !guest.wait_for(PROMPT, secs(60)) {
        return Ok(fail(&guest, "the guest never reached a shell prompt"));
    }

    let baseline = free_pages(&mut guest, "boot");
    let mut points: Vec<(u64, Option<u64>)> = vec![(0, baseline)];
    let mut region_points: Vec<(u64, Option<(u64, u64)>)> = vec![(0, regions(&mut guest, "boot"))];

    let mut hung = None;
    for done in 1..=options.execs {
        let start = guest.transcript_len();
        if !send_command(&mut guest, options.command) {
            hung = Some(done);
            eprintln!("HANG: exec {done} could not be sent");
            break;
        }
        if !guest.wait_for_after(options.marker.as_bytes(), start, secs(12)) {
            hung = Some(done);
            eprintln!("HANG: exec {done} did not complete");
            break;
        }
        if !guest.wait_for_after(options.complete.as_bytes(), start, secs(10)) {
            hung = Some(done);
            eprintln!("HANG: exec {done} no completion marker");
            break;
        }
        // The prompt is this exec's only if it arrives after this exec's
        // completion line; the shell reaps the child before printing it.
        let after = guest.rfind(options.complete.as_bytes()).unwrap_or(start);
        if !guest.wait_for_after(PROMPT, after, secs(10)) {
            hung = Some(done);
            eprintln!("HANG: no prompt after exec {done}");
            break;
        }
        thread::sleep(Duration::from_millis(300));

        if done % options.sample_every == 0 {
            let free = match free_pages(&mut guest, &format!("exec {done}")) {
                Some(free) => Some(free),
                None => {
                    thread::sleep(Duration::from_secs(2));
                    let retry = free_pages(&mut guest, &format!("exec {done} retry"));
                    if retry.is_none() {
                        hung = Some(done);
                        break;
                    }
                    retry
                }
            };
            points.push((done, free));
            let region = match regions(&mut guest, &format!("exec {done}")) {
                Some(region) => Some(region),
                None => {
                    thread::sleep(Duration::from_secs(2));
                    regions(&mut guest, &format!("exec {done} retry"))
                }
            };
            region_points.push((done, region));
        }
    }

    if let Some(done) = hung {
        // The shell is inside a builtin when the first sample was taken, so a
        // sample that fails is retried after a dump, which names the blocked
        // process rather than guessing at it.
        let mut free = free_pages(&mut guest, "hang");
        if free.is_none() {
            let _ = send_command(&mut guest, "hangdump");
            thread::sleep(Duration::from_secs(3));
            free = free_pages(&mut guest, "hang retry");
        }
        points.push((done, free));
    }

    println!("points (exec, free pages): {points:?}");
    println!("regions (exec, (regions, pages)): {region_points:?}");

    let mut ok = hung.is_none();
    if let (Some((exec0, Some(free0))), Some((exec1, Some(free1)))) =
        (points.first().copied(), points.last().copied())
    {
        let leak = (free0 as f64 - free1 as f64) / (exec1.saturating_sub(exec0).max(1) as f64);
        println!("leak: {leak:.1} pages/exec ({:.1} KiB)", leak * 4.0);
        ok = ok && leak < LEAK_CEILING;
    }
    if let (
        Some((exec0, Some((count0, pages0)))),
        Some((exec1, Some((count1, pages1)))),
    ) = (region_points.first().copied(), region_points.last().copied())
    {
        let span = exec1.saturating_sub(exec0).max(1) as f64;
        let per_region = (count1 as f64 - count0 as f64) / span;
        let per_page = (pages1 as f64 - pages0 as f64) / span;
        println!("regions: {per_region:.2}/exec, pages: {per_page:.2}/exec");
        ok = ok && per_region < REGION_CEILING && per_page < REGION_PAGES_CEILING;
    }

    step(
        1,
        "no exec hung and the per-exec cost is flat",
        ok,
    );
    Ok(ok)
}

fn free_pages(guest: &mut Guest, tag: &str) -> Option<u64> {
    let found = Cell::new(None);
    let start = guest.transcript_len();
    if !send_command(guest, "memstat") {
        eprintln!("memstat[{tag}] the write did not reach the guest");
        return None;
    }
    let hit = guest.wait_until_after(
        start,
        |out| match free_pattern().captures(&String::from_utf8_lossy(out)) {
            Some(caps) => {
                found.set(caps.get(1).and_then(|m| m.as_str().parse().ok()));
                true
            }
            None => false,
        },
        secs(8),
    );
    if hit {
        found.get()
    } else {
        eprintln!("memstat[{tag}] TIMEOUT");
        None
    }
}

fn regions(guest: &mut Guest, tag: &str) -> Option<(u64, u64)> {
    let found = Cell::new(None);
    let start = guest.transcript_len();
    if !send_command(guest, "regions") {
        eprintln!("regions[{tag}] the write did not reach the guest");
        return None;
    }
    let hit = guest.wait_until_after(
        start,
        |out| match regions_pattern().captures(&String::from_utf8_lossy(out)) {
            Some(caps) => {
                let count = caps.get(1).and_then(|m| m.as_str().parse().ok());
                let pages = caps.get(2).and_then(|m| m.as_str().parse().ok());
                found.set(count.zip(pages));
                true
            }
            None => false,
        },
        secs(8),
    );
    if hit {
        found.get()
    } else {
        eprintln!("regions[{tag}] TIMEOUT");
        None
    }
}

fn free_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"mem: page \d+ total \d+ pages free (\d+)").expect("a constant pattern")
    })
}

fn regions_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"regions (\d+) pages (\d+)").expect("a constant pattern"))
}

fn image(root: &Path, arch: &str) -> io::Result<std::path::PathBuf> {
    match arch {
        "x86" => Ok(root.join("target/images/x86_64-pc-minix/minix-x86.elf")),
        "riscv64" => Ok(root.join("target/riscv64gc-unknown-minix/release/kernel-boot-riscv64")),
        "aarch64" => Ok(root.join("target/aarch64-unknown-minix/release/kernel-boot-aarch64")),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown --arch {other} (have x86, riscv64, aarch64)"),
        )),
    }
}

/// No `-monitor none` on riscv64 and aarch64: those machines drop UART input
/// bytes when the monitor is taken off stdio, and the loop types into the console
/// for every exec.
fn qemu(arch: &str, memory: &str, image: &Path) -> Command {
    let mut command = Command::new(match arch {
        "x86" => "qemu-system-x86_64",
        "riscv64" => "qemu-system-riscv64",
        _ => "qemu-system-aarch64",
    });
    match arch {
        "x86" => {
            command.args(["-nographic", "-monitor", "none", "-m", memory, "-no-reboot"]);
        }
        "riscv64" => {
            command.args([
                "-machine",
                "virt",
                "-m",
                memory,
                "-nographic",
                "-global",
                "virtio-mmio.force-legacy=off",
            ]);
        }
        _ => {
            command.args([
                "-machine",
                "virt",
                "-cpu",
                "cortex-a57",
                "-m",
                memory,
                "-nographic",
                "-no-reboot",
                "-global",
                "virtio-mmio.force-legacy=off",
            ]);
        }
    }
    command.arg("-kernel").arg(image);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memstat_line_reports_its_free_pages() {
        let text = "mem: page 4096 total 65536 pages free 12345 (48 MiB)";
        let re = Regex::new(r"mem: page \d+ total \d+ pages free (\d+)").unwrap();
        let caps = re.captures(text).expect("the line matches");
        assert_eq!(caps.get(1).unwrap().as_str(), "12345");
    }

    #[test]
    fn a_regions_line_reports_both_counts() {
        let re = Regex::new(r"regions (\d+) pages (\d+)").unwrap();
        let caps = re.captures("regions 37 pages 812\r").expect("the line matches");
        assert_eq!(caps.get(1).unwrap().as_str(), "37");
        assert_eq!(caps.get(2).unwrap().as_str(), "812");
    }

    #[test]
    fn the_arch_superset_is_the_one_the_recipes_stage() {
        assert!(image(Path::new("root"), "x86")
            .expect("a known arch")
            .ends_with("target/images/x86_64-pc-minix/minix-x86.elf"));
        assert!(image(Path::new("root"), "wasm32").is_err());
    }
}
