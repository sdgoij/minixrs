//! A host key reaches Doom through the input server, not the serial console.
//!
//! Boots the split image with a QMP socket, types `/bin/doom` at the shell, and
//! waits for the platform layer's first-frame verdict. Then it injects Escape as a
//! real device event: QEMU's PS/2 controller queues an HID usage, the input
//! server's ring hands it to a reader of its own, and the platform layer prints
//! `doom: kbd PASS` only when an event arrives from `/dev/kbd`. The serial console
//! is a different source, so that line cannot come from the typed command.
//!
//! The same path then drives the menu, which is what makes letters and quitting
//! testable: the cursor walks to Quit Game, Enter opens the confirmation, and `y`
//! leaves the game. Letters have to arrive lowercased (Doom compares `key` with
//! `'y'` and its cheat sequences are lowercase), and the platform layer prints
//! `doom: exit` from its `atexit` handler: the engine's own `D_Endoom` hook is
//! what calls `exit`, and `I_Quit` returns to the game loop without it.
//!
//! Unlike the tsv gate this cannot be driven through the shell's stdin: the input
//! server never sees the serial console, and the key has to be a device event,
//! which is what QMP injects.

use std::io;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use super::{fail, secs, step};
use crate::guest::Guest;
use crate::qmp::Qmp;

/// A port none of the other probes listens on.
const QMP_PORT: u16 = 4476;

/// The gate's system layout: the boot image plus the root filesystem on a disk.
fn qemu(root: &Path) -> Command {
    let image = root.join("target/images/x86_64-pc-minix/minix-x86-boot.elf");
    let disk = root.join("target/images/x86_64-pc-minix/disk.img");

    let mut command = Command::new("qemu-system-x86_64");
    command.args([
        "-nographic",
        "-monitor",
        "none",
        "-m",
        "256M",
        "-no-reboot",
        "-vga",
        "none",
        "-device",
        "bochs-display,id=fb0",
    ]);
    command.arg("-kernel").arg(image);
    command.args([
        "-drive",
        &format!(
            "if=none,id=disk0,file={},format=raw,cache=writethrough",
            disk.display()
        ),
        "-device",
        "virtio-blk-pci,disable-legacy=on,drive=disk0",
        "-netdev",
        "user,id=net0",
        "-device",
        "virtio-net-pci,disable-legacy=on,netdev=net0",
    ]);
    command
        .arg("-qmp")
        .arg(format!("tcp:127.0.0.1:{QMP_PORT},server=on,wait=off"));
    command
}

pub fn run(root: &Path) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut qemu(root))?;
    if !guest.wait_for(b"# ", secs(120)) {
        return Ok(fail(&guest, "the guest never reached a shell prompt"));
    }

    guest.send_paced("/bin/doom", Duration::from_millis(3))?;
    if !guest.wait_for(b"doom: PASS", secs(90)) {
        return Ok(fail(&guest, "Doom never rendered a verified frame"));
    }

    let mut qmp = Qmp::connect(QMP_PORT)?;
    thread::sleep(Duration::from_millis(500));
    qmp.press("esc")?;

    let mut ok = step(
        1,
        "a device key reached Doom through /dev/kbd",
        guest.wait_for(b"doom: kbd PASS", secs(30)),
    );

    if ok {
        // The menu is open from step 1: walk to Quit Game, open the confirmation,
        // and answer it. Quoting the shell's own prompt would match the one
        // printed before the game started, so the wait is anchored past the keys.
        let anchor = guest.transcript_len();
        for _ in 0..5 {
            qmp.press("down")?;
        }
        qmp.press("ret")?;
        thread::sleep(Duration::from_millis(300));
        qmp.press("y")?;
        ok = step(
            2,
            "`y` at the quit prompt leaves the game",
            guest.wait_for_after(b"doom: exit", anchor, secs(30)),
        );
        if ok {
            ok = step(
                3,
                "the shell prompt came back",
                guest.wait_for_after(b"# ", anchor, secs(30)),
            );
        }
    }
    if !ok {
        eprintln!("{}", guest.tail(2000));
    }
    println!("doom-kbd probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}
