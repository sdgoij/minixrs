//! The allocator's region count is flat under alloc/free churn.
//!
//! `/bin/allocprobe` drives `minix_rt::allocator` directly: it grows the heap
//! across several chunks and releases all of it (a fully free chunk must be
//! munmapped, so the count returns to the baseline it started from), then churns
//! 200 x 1 KiB blocks inside one chunk (a freed block must be reused, so the
//! count never grows). It prints its own verdict, `allocprobe: PASS regions flat
//! at N`, or an `allocprobe: FAIL ...` line naming the phase that broke, so this
//! driver only types the command and reads the answer.
//!
//! Not a `tools/smoke` step: that scenario is typed into four targets, and the
//! wasm program module has no `allocprobe` arm to dispatch to.

use std::io;
use std::path::Path;
use std::process::Command;

use super::{contains, fail, secs, send_command, step};
use crate::guest::Guest;

const PROMPT: &[u8] = b"# ";
const PASS: &[u8] = b"allocprobe: PASS";
const FAIL: &[u8] = b"allocprobe: FAIL";

pub fn run(root: &Path, arch: &str, memory: &str) -> io::Result<bool> {
    let image = image(root, arch)?;
    if !image.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no image at {} - build it first", image.display()),
        ));
    }

    let mut guest = Guest::spawn(&mut qemu(arch, memory, &image))?;
    if !guest.wait_for(PROMPT, secs(60)) {
        return Ok(fail(&guest, "the guest never reached a shell prompt"));
    }

    let start = guest.transcript_len();
    if !send_command(&mut guest, "allocprobe") {
        return Ok(fail(
            &guest,
            "the guest never echoed the whole of 'allocprobe'",
        ));
    }
    let spoke = guest.wait_until_after(
        start,
        |out| contains(out, PASS) || contains(out, FAIL),
        secs(60),
    );
    let out = guest.with_transcript(|out| out[start.min(out.len())..].to_vec());
    for line in String::from_utf8_lossy(&out).lines() {
        if line.starts_with("allocprobe:") {
            println!("{line}");
        }
    }

    let ok = spoke && contains(&out, PASS) && !contains(&out, FAIL);
    step(1, "the region count returned to its baseline", ok);
    Ok(ok)
}

/// The assembled image each arch boots, which is the one the recipes stage.
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

/// The serial console on stdio, with no display: the probe reads text, and the
/// pattern it would draw is another gate's business.
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
    fn the_probe_boots_the_image_the_recipes_stage() {
        let staged = image(Path::new("root"), "x86").expect("a known arch");
        assert!(staged.ends_with("target/images/x86_64-pc-minix/minix-x86.elf"));
        assert!(image(Path::new("root"), "wasm32").is_err());
    }

    #[test]
    fn the_console_is_stdio_on_every_arch() {
        for arch in ["x86", "riscv64", "aarch64"] {
            let command = qemu(arch, "256M", Path::new("image.elf"));
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(args.contains(&"-nographic".to_string()), "{arch}");
            assert!(args.contains(&"image.elf".to_string()), "{arch}");
        }
    }
}
