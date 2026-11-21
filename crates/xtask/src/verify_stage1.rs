//! Verify a stage1 toolchain by consuming it the way a dev or CI job does.
//!
//! It installs the published toolchain for the commit the `rust` submodule pins
//! (`crate::fetch_stage1`), then links `tools/std-hello.rs` for all three minix
//! targets with the linker the toolchain resolves and reads the ELF header of
//! each result. A host that reaches the summary can run the published toolchain
//! and link with it, which is the part a Windows build machine cannot check.
//!
//! The linker is the one the published toolchain carries in its own sysroot
//! (`lib/rustlib/<host>/bin/rust-lld`, with `gcc-ld` beside it): that is one of
//! the things this checks for, so the lookup does not fall back to a linked LLVM
//! or to `PATH`. `MINIXRS_LLD` names one explicitly, which is how a stage1 built
//! from source (whose LLD lives under `rust/build/<host>/lld`) can be verified.
//!
//! The smoke binary names `minix_libc` (std's backtrace needs the port's unwind
//! stubs), so each target's rlib is built with the same toolchain first, as
//! `cargo xtask build std-hello` does.
//!
//! `MINIXRS_STAGE1_BASE` and `MINIXRS_STAGE1_DEST` behave as they do for
//! `fetch-stage1`; the default destination is `target/stage1-verify`, which the
//! other command never uses.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::arch;
use crate::build::newest_libc_rlib;
use crate::cc;
use crate::fetch_stage1;
use crate::toolchain;

const HELLO: &str = "tools/std-hello.rs";
const LINKER_SCRIPT: &str = "tools/minix-user.ld";

/// rustc triple -> the e_machine its ELF has to carry.
const TARGETS: [(&str, u16); 3] = [
    ("x86_64-pc-minix", 0x3e),
    ("riscv64gc-unknown-minix", 0xf3),
    ("aarch64-unknown-minix", 0xb7),
];

pub fn run(root: &Path) -> io::Result<()> {
    let sha = fetch_stage1::pinned_sha(root)?;
    let dest = std::env::var_os("MINIXRS_STAGE1_DEST")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target").join("stage1-verify"));

    // An install for the pinned commit is reused, so a second run does not
    // download the asset again; anything else in the way is replaced.
    let force = fetch_stage1::recorded_sha(&dest.join("stage1")).as_deref() != Some(sha.as_str());
    if force && dest.join("stage1").is_dir() {
        println!(
            "== replacing {}: it is not the published toolchain for {}",
            dest.join("stage1").display(),
            fetch_stage1::short(&sha)
        );
    }
    println!(
        "== installing the stage1 for {} with cargo xtask fetch-stage1",
        fetch_stage1::short(&sha)
    );
    let stage1 = fetch_stage1::install(root, force, Some(dest.clone()))?;
    if !stage1.join("bin").is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("fetch-stage1 installed no toolchain at {}", stage1.display()),
        ));
    }

    let rustc = stage1.join("bin").join(toolchain::exe("rustc"));
    let lld = linker(&stage1)?;
    println!("== toolchain: {}", rustc.display());
    run_inherited(Command::new(&rustc).arg("-vV"))?;
    run_inherited(Command::new(&lld).args(["-flavor", "gnu", "--version"]))?;

    println!("== linking {HELLO} for {} minix targets", TARGETS.len());
    for (target, machine) in TARGETS {
        let deps = arch::resolve(target)?.libc_deps(root);
        let mut cargo = Command::new(toolchain::cargo());
        cargo
            .current_dir(root)
            .args(["build", "-p", "minix-libc", "--target", target, "--release"])
            .env("RUSTC", &rustc);
        if !cc::run(&mut cargo)? {
            return Err(io::Error::other(format!(
                "`cargo build -p minix-libc --target {target}` failed with the toolchain"
            )));
        }
        let rlib = newest_libc_rlib(&deps)?;

        let out = dest.join(format!("hello-{target}"));
        let mut link = Command::new(&rustc);
        link.args(link_args(root, target, &lld, &rlib, &deps, &out));
        if !cc::run(&mut link)? {
            return Err(io::Error::other(format!(
                "linking {} for {target} failed",
                out.display()
            )));
        }

        let got = elf_machine(&out)?;
        if got != machine {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is {}, expected {}",
                    out.display(),
                    machine_name(got),
                    machine_name(machine)
                ),
            ));
        }
        let size = fs::metadata(&out)?.len();
        println!(
            "   {:<26} {:<8} {size} bytes",
            target,
            machine_name(machine)
        );
    }

    println!("ok: the published stage1 runs and links all three minix targets on this host");
    Ok(())
}

/// The linker to link the probe binaries with.
fn linker(stage1: &Path) -> io::Result<PathBuf> {
    if let Some(override_path) = std::env::var_os("MINIXRS_LLD")
        && !override_path.is_empty()
    {
        let path = PathBuf::from(&override_path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("MINIXRS_LLD is {}, which is not a file", path.display()),
        ));
    }
    if let Some(lld) = sysroot_lld(stage1) {
        return Ok(lld);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "no rust-lld under {}/lib/rustlib/*/bin, so the toolchain cannot link minix binaries, \
             which is one of the things this checks for; set MINIXRS_LLD to a full lld to verify \
             a stage1 built from source",
            stage1.display()
        ),
    ))
}

/// The sysroot's own LLD: `gcc-ld` is installed beside it for the host only,
/// which is what tells the host's copy apart from a target's.
fn sysroot_lld(stage1: &Path) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(stage1.join("lib").join("rustlib"))
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    entries.sort();
    entries.into_iter().find_map(|dir| {
        let bin = dir.join("bin");
        let lld = bin.join(toolchain::exe("rust-lld"));
        (lld.is_file() && bin.join("gcc-ld").is_dir()).then_some(lld)
    })
}

/// The ELF `e_machine` in `path`'s header.
fn elf_machine(path: &Path) -> io::Result<u16> {
    let mut header = [0u8; 20];
    let mut file = fs::File::open(path)?;
    let not_elf = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not an ELF file", path.display()),
        )
    };
    file.read_exact(&mut header).map_err(|_| not_elf())?;
    if &header[..4] != b"\x7fELF" {
        return Err(not_elf());
    }
    let machine = [header[18], header[19]];
    Ok(if header[5] == 1 {
        u16::from_le_bytes(machine)
    } else {
        u16::from_be_bytes(machine)
    })
}

fn machine_name(machine: u16) -> String {
    match machine {
        0x3e => "x86-64".to_string(),
        0xf3 => "RISC-V".to_string(),
        0xb7 => "AArch64".to_string(),
        other => format!("{other:#x}"),
    }
}

/// rustc's arguments for one probe binary.
///
/// A pure list so a dropped flag is a test failure rather than a linker error
/// discovered on a release host.
fn link_args(
    root: &Path,
    target: &str,
    lld: &Path,
    rlib: &Path,
    deps: &Path,
    out: &Path,
) -> Vec<String> {
    vec![
        "--target".to_string(),
        target.to_string(),
        "--edition".to_string(),
        "2024".to_string(),
        "-C".to_string(),
        format!("link-arg=-T{}", root.join(LINKER_SCRIPT).display()),
        "-C".to_string(),
        "link-arg=--no-eh-frame-hdr".to_string(),
        "-C".to_string(),
        format!("linker={}", lld.display()),
        "--extern".to_string(),
        format!("minix_libc={}", rlib.display()),
        "-L".to_string(),
        format!("dependency={}", deps.display()),
        "-o".to_string(),
        out.display().to_string(),
        root.join(HELLO).display().to_string(),
    ]
}

fn run_inherited(command: &mut Command) -> io::Result<()> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{command:?} failed ({status})")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn elf_header(little_endian: bool, machine: u16) -> Vec<u8> {
        let mut header = vec![0u8; 20];
        header[..4].copy_from_slice(b"\x7fELF");
        header[5] = if little_endian { 1 } else { 2 };
        let bytes = if little_endian {
            machine.to_le_bytes()
        } else {
            machine.to_be_bytes()
        };
        header[18..20].copy_from_slice(&bytes);
        header
    }

    #[test]
    fn the_elf_machine_is_read_in_either_byte_order() {
        let dir = temp_dir("minixrs-xtask-elf-machine");
        let little = dir.join("little");
        fs::write(&little, elf_header(true, 0x3e)).unwrap();
        assert_eq!(elf_machine(&little).unwrap(), 0x3e);

        let big = dir.join("big");
        fs::write(&big, elf_header(false, 0xb7)).unwrap();
        assert_eq!(elf_machine(&big).unwrap(), 0xb7);

        let other = dir.join("other");
        fs::write(&other, b"not an elf at all, just text").unwrap();
        assert!(elf_machine(&other).is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sysroot_lld_is_the_copy_with_gcc_ld_beside_it() {
        let dir = temp_dir("minixrs-xtask-sysroot-lld");
        let rustlib = dir.join("lib").join("rustlib");

        let target_bin = rustlib.join("x86_64-pc-minix").join("bin");
        fs::create_dir_all(&target_bin).unwrap();
        fs::write(target_bin.join(toolchain::exe("rust-lld")), b"").unwrap();

        let host_bin = rustlib.join("host-triple").join("bin");
        fs::create_dir_all(host_bin.join("gcc-ld")).unwrap();
        fs::write(host_bin.join(toolchain::exe("rust-lld")), b"").unwrap();

        assert_eq!(
            sysroot_lld(&dir),
            Some(host_bin.join(toolchain::exe("rust-lld")))
        );

        fs::remove_dir_all(host_bin.join("gcc-ld")).unwrap();
        assert_eq!(sysroot_lld(&dir), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_link_line_names_the_script_the_linker_and_the_output() {
        let root = Path::new("root");
        let lld = Path::new("toolchain").join("rust-lld");
        let rlib = Path::new("deps").join("libminix_libc-abc.rlib");
        let deps = Path::new("deps");
        let out = Path::new("out").join("hello");
        let args = link_args(root, "riscv64gc-unknown-minix", &lld, &rlib, deps, &out);
        let joined = args.join(" ");
        assert!(joined.contains("--target riscv64gc-unknown-minix"));
        assert!(joined.contains("--edition 2024"));
        assert!(joined.contains(&format!(
            "link-arg=-T{}",
            root.join(LINKER_SCRIPT).display()
        )));
        assert!(joined.contains("link-arg=--no-eh-frame-hdr"));
        assert!(joined.contains(&format!("linker={}", lld.display())));
        assert!(joined.contains(&format!("--extern minix_libc={}", rlib.display())));
        assert!(joined.contains(&format!("-L dependency={}", deps.display())));
        assert!(joined.contains(&format!("-o {}", out.display())));
        assert_eq!(
            args.last().map(String::as_str),
            Some(root.join(HELLO).display().to_string().as_str())
        );
    }

    #[test]
    fn every_minix_target_names_its_machine() {
        assert_eq!(machine_name(0x3e), "x86-64");
        assert_eq!(machine_name(0xf3), "RISC-V");
        assert_eq!(machine_name(0xb7), "AArch64");
        assert_eq!(machine_name(0x28), "0x28");
        let mut machines: Vec<u16> = TARGETS.iter().map(|(_, machine)| *machine).collect();
        machines.sort_unstable();
        machines.dedup();
        assert_eq!(machines.len(), TARGETS.len());
    }
}
