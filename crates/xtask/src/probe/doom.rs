//! Two Doom gates that have to drive a real device and answer a real prompt.
//!
//! `run` (the `doom-kbd` gate) checks that a host key reaches Doom through the
//! input server, not the serial console. `run_save` (the `doom-save` gate) saves
//! a game, leaves to the shell, lists the file, syncs and reboots, which is the
//! VFS write path rather than the render path. Both run on every arch: x86's
//! keys come from the PS/2 controller and the others' from the virtio keyboard,
//! and the display is a bochs framebuffer or a virtio-gpu one.
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

/// The save probe boots twice, so it keeps its own two ports.
const SAVE_QMP_PORT: u16 = 4481;

/// The gate's system layout: the boot image plus the root filesystem on a disk.
///
/// Each arch boots the image its `system-image-<arch>` recipe writes, with the
/// devices its gate line carries: a bochs display and the PS/2 keyboard on x86,
/// a virtio-gpu display and virtio-keyboard on riscv64/aarch64. The probe cannot
/// take the gate's own command because it needs a QMP socket on its own port and
/// no tablet (the x86 recipe's `-nographic` line is the one the gate uses, and it
/// has one).
fn qemu(root: &Path, arch: &str, qmp_port: u16) -> Command {
    let (triple, boot) = match arch {
        "x86" => ("x86_64-pc-minix", "minix-x86-boot.elf"),
        "riscv64" => ("riscv64gc-unknown-minix", "minix-riscv64-boot.elf"),
        _ => ("aarch64-unknown-minix", "minix-aarch64-boot.elf"),
    };
    let image = root.join("target/images").join(triple).join(boot);
    let disk = root.join("target/images").join(triple).join("disk.img");
    let drive = format!(
        "if=none,id=disk0,file={},format=raw,cache=writethrough",
        disk.display()
    );

    let mut command = match arch {
        "x86" => {
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
                &drive,
                "-device",
                "virtio-blk-pci,disable-legacy=on,drive=disk0",
                "-netdev",
                "user,id=net0",
                "-device",
                "virtio-net-pci,disable-legacy=on,netdev=net0",
            ]);
            command
        }
        "riscv64" => {
            let mut command = Command::new("qemu-system-riscv64");
            command.args(["-machine", "virt", "-m", "256M", "-nographic", "-monitor", "none"]);
            virtio_devices(&mut command, &drive, &image);
            command
        }
        _ => {
            let mut command = Command::new("qemu-system-aarch64");
            command.args([
                "-machine",
                "virt",
                "-cpu",
                "cortex-a57",
                "-m",
                "256M",
                "-nographic",
                "-monitor",
                "none",
                "-no-reboot",
            ]);
            virtio_devices(&mut command, &drive, &image);
            command
        }
    };
    command
        .arg("-qmp")
        .arg(format!("tcp:127.0.0.1:{qmp_port},server=on,wait=off"));
    command
}

/// The virtio-mmio devices riscv64 and aarch64 carry: the disk, the display the
/// frame is blitted into, and the keyboard the keys come from.
fn virtio_devices(command: &mut Command, drive: &str, image: &Path) {
    command.args(["-global", "virtio-mmio.force-legacy=off"]);
    command.args([
        "-drive",
        drive,
        "-device",
        "virtio-blk-device,drive=disk0",
        "-device",
        "virtio-gpu-device",
        "-device",
        "virtio-keyboard-device",
    ]);
    command.arg("-kernel").arg(image);
}

/// Boot to a prompt takes far longer under TCG than on x86's KVM-less but
/// better-tuned emulation; the gates' own per-arch budgets say the same.
fn boot_timeout(arch: &str) -> Duration {
    if arch == "x86" {
        secs(120)
    } else {
        secs(300)
    }
}

pub fn run(root: &Path, arch: &str) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut qemu(root, arch, QMP_PORT))?;
    if !guest.wait_for(b"# ", boot_timeout(arch)) {
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

/// Save a game, leave to the shell, list the file, sync, reboot, list it again.
///
/// Doom creates `./.savegame/` with `mkdir("./.savegame/")` at startup and
/// writes `temp.dsg`, renaming it to `doomsav0.dsg`. Both are directory
/// operations on a relative path: `mkdir` resolves its parent through
/// `last_dir`, and the rename works on the same name one level down. Quitting
/// gives the shell back, so the file can be listed from outside Doom, and the
/// reboot is what separates a save that reached the device from one that only
/// lived in MFS's write-back cache.
///
/// The menu steps are the engine's own: New Game is the first row, Save Game is
/// three rows below it, and after a save the cursor is still on Save Game, two
/// rows above Quit Game. A press needs a tic of its own, which is what `tap`
/// gives it.
pub fn run_save(root: &Path, arch: &str) -> io::Result<bool> {
    let mut guest = Guest::spawn(&mut qemu(root, arch, SAVE_QMP_PORT))?;
    if !guest.wait_for(b"# ", boot_timeout(arch)) {
        return Ok(fail(&guest, "the guest never reached a shell prompt"));
    }
    guest.send_paced("/bin/doom", Duration::from_millis(3))?;
    if !guest.wait_for(b"doom: PASS", secs(90)) {
        return Ok(fail(&guest, "Doom never rendered a verified frame"));
    }

    let mut qmp = Qmp::connect(SAVE_QMP_PORT)?;
    thread::sleep(Duration::from_millis(500));

    // New Game, the episode, the skill, and the level load.
    for key in ["esc", "ret", "ret", "ret"] {
        tap(&mut qmp, key)?;
    }
    thread::sleep(Duration::from_secs(4));

    // Save Game, slot 0, a name, accept.
    tap(&mut qmp, "esc")?;
    for _ in 0..3 {
        tap(&mut qmp, "down")?;
    }
    tap(&mut qmp, "ret")?;
    thread::sleep(Duration::from_secs(1));
    tap(&mut qmp, "ret")?;
    thread::sleep(Duration::from_millis(500));
    tap(&mut qmp, "a")?;
    tap(&mut qmp, "ret")?;
    thread::sleep(Duration::from_secs(2));

    // A save returns to the game with the menu cursor still on Save Game.
    let quit_anchor = guest.transcript_len();
    tap(&mut qmp, "esc")?;
    for _ in 0..2 {
        tap(&mut qmp, "down")?;
    }
    tap(&mut qmp, "ret")?;
    thread::sleep(Duration::from_millis(500));
    tap(&mut qmp, "y")?;

    let mut ok = step(
        1,
        "the save leaves the game at the shell",
        guest.wait_for_after(b"doom: exit", quit_anchor, secs(30))
            && guest.wait_for_after(b"# ", quit_anchor, secs(30)),
    );

    if ok {
        // Doom ran in the data directory; the shell is still where it started.
        let anchor = guest.transcript_len();
        guest.send_paced("cd /usr/share/games/doom", Duration::from_millis(3))?;
        let cd_ok = guest.wait_for_after(b"# ", anchor, secs(15));
        let anchor = guest.transcript_len();
        guest.send_paced("ls .savegame", Duration::from_millis(3))?;
        let listed = guest.wait_for_after(b"doomsav0.dsg", anchor, secs(15));
        // The recovery file is the engine's fallback when the directory or the
        // create fails, so its message is a failure even if a file is listed.
        let no_fallback = guest.count(b"Failed to open savegame file") == 0;
        ok = step(
            2,
            "the save is a file in .savegame, not a recovery copy",
            cd_ok && listed && no_fallback,
        );
    }

    if ok {
        // MFS caches writes; a QEMU that is killed without this loses them.
        let anchor = guest.transcript_len();
        guest.send_paced("sync", Duration::from_millis(3))?;
        ok = step(
            3,
            "the guest syncs the save to the device",
            guest.wait_for_after(b"# ", anchor, secs(15)),
        );
    }

    if !ok {
        eprintln!("{}", guest.tail(3000));
    }
    drop(qmp);
    drop(guest);

    if ok {
        // A directory entry can reach the device without its inode; only a
        // second boot tells a durable save from that half-written one.
        let mut guest = Guest::spawn(&mut qemu(root, arch, SAVE_QMP_PORT + 1))?;
        if !guest.wait_for(b"# ", boot_timeout(arch)) {
            return Ok(fail(&guest, "the second boot never reached a shell prompt"));
        }
        let anchor = guest.transcript_len();
        guest.send_paced("cd /usr/share/games/doom", Duration::from_millis(3))?;
        let cd_ok = guest.wait_for_after(b"# ", anchor, secs(15));
        let anchor = guest.transcript_len();
        guest.send_paced("ls .savegame", Duration::from_millis(3))?;
        let listed = guest.wait_for_after(b"doomsav0.dsg", anchor, secs(15));
        ok = step(4, "the save survives a reboot", cd_ok && listed);
        if !ok {
            eprintln!("{}", guest.tail(3000));
        }
    }

    println!("doom-save probe: {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}

/// Press `key`, then let Doom run a tic before the next one.
fn tap(qmp: &mut Qmp, key: &str) -> io::Result<()> {
    qmp.press(key)?;
    thread::sleep(Duration::from_millis(300));
    Ok(())
}
