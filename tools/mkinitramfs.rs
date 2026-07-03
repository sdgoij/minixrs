//! Builds the initramfs CPIO newc archive containing all boot-critical
//! userland binaries and device nodes.
//!
//! Usage: rustc tools/mkinitramfs.rs --edition 2021 -o target/mkinitramfs.exe
//!        && target/mkinitramfs.exe
//!
//! Output: target/initramfs.cpio (CPIO newc archive)
//!         target/initramfs_data.rs (Rust source with embedded bytes)

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

/// List of boot-critical binaries to include in the initramfs.
/// Maps destination path → Cargo package.binary-name.
const BOOT_BINS: &[(&str, &str, &str)] = &[
    ("/sbin/init", "userland", "init"),
    ("/bin/sh", "userland", "sh"),
    ("/bin/cat", "userland", "cat"),
    ("/bin/echo", "userland", "echo"),
    ("/bin/ls", "userland", "ls"),
    ("/bin/mkdir", "userland", "mkdir"),
    ("/bin/rm", "userland", "rm"),
    ("/bin/cp", "userland", "cp"),
    ("/bin/ln", "userland", "ln"),
    ("/bin/chmod", "userland", "chmod"),
    ("/bin/sync", "userland", "sync"),
    ("/sbin/mknod", "userland", "mknod"),
    ("/sbin/reboot", "userland", "reboot"),
    ("/sbin/fsck", "userland", "fsck"),
    ("/sbin/pm", "servers", "pm"),
    ("/sbin/vfs", "servers", "vfs"),
    ("/sbin/vm", "servers", "vm"),
    ("/sbin/rs", "servers", "rs"),
    ("/sbin/ds", "servers", "ds"),
    ("/sbin/sched", "servers", "sched"),
    ("/sbin/tty", "servers", "tty"),
];

/// Device nodes to create in the initramfs.
const DEVICES: &[(&str, u32, u32, u32)] = &[
    ("/dev/tty00", 0o020777, 3, 0), // char device, major=3 (pseudo-tty), minor=0
    ("/dev/tty01", 0o020777, 3, 1), // char device, major=3, minor=1
    ("/dev/null", 0o020666, 1, 3),  // char device, major=1 (mem), minor=3
    ("/dev/console", 0o020600, 5, 0), // char device, major=5 (console), minor=0
];

/// CPIO newc header structure (110 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct CpioNewcHeader {
    magic: [u8; 6],      // "070701"
    ino: [u8; 8],        // inode number
    mode: [u8; 8],       // file mode
    uid: [u8; 8],        // user id
    gid: [u8; 8],        // group id
    nlink: [u8; 8],      // number of links
    mtime: [u8; 8],      // modification time
    filesize: [u8; 8],   // size of file data
    dev_major: [u8; 8],  // device major
    dev_minor: [u8; 8],  // device minor
    rdev_major: [u8; 8], // device major (for special files)
    rdev_minor: [u8; 8], // device minor (for special files)
    namesize: [u8; 8],   // length of filename in bytes, including null
    check: [u8; 8],      // checksum (0 for newc)
}

impl CpioNewcHeader {
    fn new(
        ino: u32,
        mode: u32,
        uid: u32,
        gid: u32,
        nlink: u32,
        mtime: u32,
        filesize: u32,
        dev: u32,
        rdev: u32,
        name: &str,
    ) -> Self {
        let namesize = name.len() + 1; // +1 for null terminator
        CpioNewcHeader {
            magic: *b"070701",
            ino: hex8(ino),
            mode: hex8(mode),
            uid: hex8(uid),
            gid: hex8(gid),
            nlink: hex8(nlink),
            mtime: hex8(mtime),
            filesize: hex8(filesize),
            dev_major: hex8(major(dev)),
            dev_minor: hex8(minor(dev)),
            rdev_major: hex8(major(rdev)),
            rdev_minor: hex8(minor(rdev)),
            namesize: hex8(namesize as u32),
            check: hex8(0),
        }
    }

    fn write<W: Write>(&self, w: &mut W) -> std::io::Result<()> {
        w.write_all(&self.magic)?;
        w.write_all(&self.ino)?;
        w.write_all(&self.mode)?;
        w.write_all(&self.uid)?;
        w.write_all(&self.gid)?;
        w.write_all(&self.nlink)?;
        w.write_all(&self.mtime)?;
        w.write_all(&self.filesize)?;
        w.write_all(&self.dev_major)?;
        w.write_all(&self.dev_minor)?;
        w.write_all(&self.rdev_major)?;
        w.write_all(&self.rdev_minor)?;
        w.write_all(&self.namesize)?;
        w.write_all(&self.check)?;
        Ok(())
    }
}

fn hex8(v: u32) -> [u8; 8] {
    let s = format!("{:08x}", v);
    let mut buf = [0u8; 8];
    buf.copy_from_slice(s.as_bytes());
    buf
}

fn major(dev: u32) -> u32 {
    (dev >> 8) & 0xFF
}

fn minor(dev: u32) -> u32 {
    dev & 0xFF
}

/// Pad a value up to the next 4-byte boundary (CPIO alignment).
fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

const MODE_DIR: u32 = 0o040755;
const MODE_FILE: u32 = 0o100755;
const MODE_CHAR: u32 = 0o020777;

fn main() {
    // Parse optional architecture argument: "x86_64" (default) or "riscv64"
    let arch = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x86_64".to_string());
    let is_riscv = arch == "riscv64";

    let workspace = Path::new(".");
    let target_dir = workspace.join("target");
    fs::create_dir_all(&target_dir).ok();

    // Architecture-specific settings
    let target_spec = if is_riscv {
        "riscv64gc-unknown-none-elf"
    } else {
        "x86_64-pc-minix.json"
    };
    let target_out_dir = if is_riscv {
        "riscv64gc-unknown-none-elf"
    } else {
        "x86_64-pc-minix"
    };

    println!("Building userland binaries for {}...", arch);
    let bins_dir = target_dir.join("initramfs_bins");
    fs::create_dir_all(&bins_dir).ok();

    for (pkg_name, label) in [("userland", "userland"), ("servers", "servers")] {
        println!("Building {label} binaries...");
        let mut cargo_args = vec![
            "run",
            "nightly",
            "cargo",
            "build",
            "-p",
            pkg_name,
            "--bins",
            "--target",
            target_spec,
        ];
        // Only add -Zjson-target-spec for custom targets (.json files)
        if target_spec.ends_with(".json") {
            cargo_args.push("-Zjson-target-spec");
        }
        cargo_args.push("-Zbuild-std=core,alloc");
        cargo_args.push("-Zbuild-std-features=compiler-builtins-mem");
        cargo_args.push("--release");

        let status = Command::new("rustup")
            .args(&cargo_args)
            .env(
                "RUSTFLAGS",
                "-C link-arg=-Ttools/minix-user.ld -C link-arg=--no-eh-frame-hdr",
            )
            .status()
            .expect(&format!("cargo build {} failed", pkg_name));
        if !status.success() {
            println!("  WARNING: {label} build failed, continuing without binaries");
        }
    }

    // Copy the built ELF binaries to our staging directory
    for &(_dest, _pkg, bin_name) in BOOT_BINS {
        let src = target_dir
            .join(target_out_dir)
            .join("release")
            .join(bin_name);
        if src.exists() {
            let dest = bins_dir.join(bin_name);
            fs::copy(&src, &dest).unwrap_or_else(|e| {
                panic!("Failed to copy {}: {}", src.display(), e);
            });
            println!("  Copied {} -> {}", src.display(), dest.display());
        } else {
            println!("  WARNING: {} not found at {}", bin_name, src.display());
        }
    }

    // Step 2: Create the CPIO newc archive
    let cpio_path = target_dir.join("initramfs.cpio");
    let mut cpio = Vec::new();
    let mut ino: u32 = 1;

    // Create root directory
    write_entry(&mut cpio, ino, "/", MODE_DIR, 0, 0, 0o755, 0, &[]);
    ino += 1;

    // Create /bin directory
    write_entry(&mut cpio, ino, "/bin", MODE_DIR, 0, 0, 0o755, 0, &[]);
    ino += 1;

    // Create /sbin directory
    write_entry(&mut cpio, ino, "/sbin", MODE_DIR, 0, 0, 0o755, 0, &[]);
    ino += 1;

    // Create /dev directory
    write_entry(&mut cpio, ino, "/dev", MODE_DIR, 0, 0, 0o755, 0, &[]);
    ino += 1;

    // Add binaries
    for &(dest, _pkg, bin_name) in BOOT_BINS {
        let src = bins_dir.join(bin_name);
        let data = if src.exists() {
            fs::read(&src).unwrap_or_default()
        } else {
            Vec::new()
        };
        write_entry(&mut cpio, ino, dest, MODE_FILE, 0, 0, 0o755, 0, &data);
        ino += 1;
    }

    // Add device nodes
    for &(path, mode, _rmajor, _rminor) in DEVICES {
        // Use device mode with char device type bit
        let dev_mode = if mode & 0o20000 != 0 { mode } else { MODE_CHAR };
        write_entry(&mut cpio, ino, path, dev_mode, 0, 0, dev_mode, 0, &[]);
        ino += 1;
    }

    // Add trailer
    write_entry(&mut cpio, 0, "TRAILER!!!", 0, 0, 0, 0, 0, &[]);

    fs::write(&cpio_path, &cpio).unwrap();
    println!("initramfs.cpio: {} bytes written", cpio.len());
    println!(
        "  {} entries ({} files, {} dirs, {} devices)",
        BOOT_BINS.len() + 7, // 4 dirs + bins + devices + trailer
        BOOT_BINS.len(),
        4,
        DEVICES.len()
    );

    // Step 3: Generate a Rust source file with embedded bytes
    let rs_path = target_dir.join("initramfs_data.rs");
    let mut rs = String::new();
    rs.push_str("// Auto-generated by tools/mkinitramfs.rs — DO NOT EDIT\n");
    rs.push_str("#[allow(dead_code)]\n");
    rs.push_str("#[allow(unused_attributes)]\n");
    rs.push_str("#[unsafe(link_section = \".initramfs\")]\n");
    rs.push_str("#[used]\n");
    rs.push_str("pub static INITRAMFS_CPIO: [u8; ");
    rs.push_str(&cpio.len().to_string());
    rs.push_str("] = [\n    ");
    for (i, byte) in cpio.iter().enumerate() {
        if i > 0 && i % 16 == 0 {
            rs.push_str("\n    ");
        }
        rs.push_str(&format!("{:#04x}, ", byte));
    }
    rs.push_str("\n];\n");
    rs.push_str("pub const INITRAMFS_CPIO_LEN: usize = ");
    rs.push_str(&cpio.len().to_string());
    rs.push_str(";\n");

    fs::write(&rs_path, &rs).unwrap();
    println!("initramfs_data.rs: {} bytes written", rs.len());

    // Clean up staging
    fs::remove_dir_all(&bins_dir).ok();

    println!("Done.");
}

fn write_entry(
    cpio: &mut Vec<u8>,
    ino: u32,
    name: &str,
    mode: u32,
    uid: u32,
    gid: u32,
    perm: u32,
    rdev: u32,
    data: &[u8],
) {
    let file_mode = if mode & 0o040000 != 0 {
        0o040000 | (perm & 0o0777) // directory
    } else if mode & 0o020000 != 0 {
        0o020000 | (perm & 0o0777) // character device
    } else {
        0o100000 | (perm & 0o0777) // regular file
    };

    let header = CpioNewcHeader::new(
        ino,
        file_mode,
        uid,
        gid,
        1, // nlink
        0, // mtime
        data.len() as u32,
        0, // dev
        rdev,
        name,
    );

    header.write(cpio).unwrap();

    // Write filename (including null terminator)
    cpio.write_all(name.as_bytes()).unwrap();
    cpio.write_all(&[0u8]).unwrap();

    // Pad filename to 4-byte boundary
    let _name_padded = pad4(name.len() + 1);
    while cpio.len() % 4 != 0 {
        cpio.push(0u8);
    }

    // Write file data
    cpio.write_all(data).unwrap();

    // Pad data to 4-byte boundary
    while cpio.len() % 4 != 0 {
        cpio.push(0u8);
    }
}
