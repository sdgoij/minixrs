//! Gate probes over a running guest, and over the data it produced.
//!
//! `read_hist` reads a trace file; `rc` and `wlkey` boot an image and watch its
//! console through `crate::guest`, the latter injecting a key through the QMP
//! socket (`crate::qmp`). `dso_share` boots the dynamic image and reads guest
//! physical memory over QMP.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub mod dso_share;
pub mod rc;
pub mod read_hist;
pub mod wlfocus;
pub mod wlkey;
pub mod wlpointer;

/// The x86 image every probe here boots, with the bochs display and a tablet so
/// the console and the pointer both have a device.
pub(crate) fn qemu(root: &Path, qmp_port: u16) -> Command {
    let image = root.join("target/images/x86_64-pc-minix/minix-x86.elf");
    let mut command = Command::new("qemu-system-x86_64");
    command.args([
        "-nographic",
        "-monitor",
        "none",
        "-display",
        "none",
        "-m",
        "256M",
        "-no-reboot",
        "-vga",
        "none",
        "-device",
        "bochs-display,id=fb0",
        "-netdev",
        "user,id=net0",
        "-device",
        "virtio-net-pci,disable-legacy=on,netdev=net0",
        "-device",
        "virtio-tablet-pci,display=fb0",
    ]);
    command.arg("-kernel").arg(image);
    command
        .arg("-qmp")
        .arg(format!("tcp:127.0.0.1:{qmp_port},server=on,wait=off"));
    command
}

/// A `Command` built from the words a recipe passed after `--`.
pub(crate) fn command_from(words: &[String]) -> Command {
    let (program, args) = words
        .split_first()
        .expect("clap requires at least the program");
    let mut command = Command::new(program);
    command.args(args);
    command
}

/// Print one step's verdict the way the scripts did, and pass it through.
pub(crate) fn step(number: u32, name: &str, passed: bool) -> bool {
    println!("[{number}] {name}: {}", if passed { "ok" } else { "FAIL" });
    passed
}

pub(crate) fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_image_is_the_one_the_recipes_stage() {
        let command = qemu(Path::new("root"), 4470);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"bochs-display,id=fb0".to_string()));
        assert!(args.contains(&"virtio-tablet-pci,display=fb0".to_string()));
        assert!(args
            .iter()
            .any(|arg| arg.ends_with("minix-x86.elf")));
        assert!(args.contains(&"tcp:127.0.0.1:4470,server=on,wait=off".to_string()));
    }

    #[test]
    fn a_recipe_line_becomes_a_command_and_its_arguments() {
        let command = command_from(&[
            "qemu-system-x86_64".to_string(),
            "-nographic".to_string(),
            "-m".to_string(),
            "256M".to_string(),
        ]);
        assert_eq!(command.get_program(), "qemu-system-x86_64");
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["-nographic", "-m", "256M"]);
    }
}
