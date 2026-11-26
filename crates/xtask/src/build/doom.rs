//! Build doomgeneric for a minix target: the engine, the minix platform layer,
//! the shareware IWAD.
//!
//! The engine is the pinned upstream tree (`tools/pins.ini`, checked out under
//! `target/doom-src`), compiled with the same freestanding flags as `c-direct` and
//! linked against `libc.a` and the arch's `crt0`, so a Doom binary is the same
//! shape as every other C program on the port.
//!
//! The platform layer is this repository's (`tools/doom/doomgeneric_minix.c`).
//! Upstream's backends each own a `main` and a host window (`doomgeneric_xlib.c`,
//! `doomgeneric_sdl.c`, ...); minix has neither, but it does have `/dev/fb` and a
//! console, which is all `DG_*` needs. The engine is built exactly as upstream's
//! `Makefile` lists it, minus the platform file.
//!
//! The IWAD is the freely redistributable shareware `doom1.wad`, downloaded once
//! and checked against the digest in `tools/pins.ini`; it is installed in the
//! image at the directory Doom's Unix build searches, which the platform layer
//! makes the working directory.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::c_hello::linker_script;
use super::libc_static;
use super::{git_output, have_commit, run_git};
use crate::arch::{self, Arch};
use crate::cc;
use crate::fetch_stage1;
use crate::pin;
use crate::toolchain;

/// Where the pinned engine sources are checked out.
const SRC: &str = "target/doom-src";
/// The shareware IWAD, beside the sources rather than inside them so the checkout
/// stays clean.
const WAD: &str = "target/doom-data/doom1.wad";
/// The program in the target's release directory.
const PROGRAM: &str = "doom";

/// Doom draws 320x200 and the platform layer scales that up, so the engine's
/// own frame buffer is the native size.
const DEFINES: [&str; 5] = [
    "DOOMGENERIC_RESX=320",
    "DOOMGENERIC_RESY=200",
    "NORMALUNIX",
    "LINUX",
    "_DEFAULT_SOURCE",
];

/// The engine's translation units, as `doomgeneric/Makefile`'s `SRC_DOOM` lists
/// them minus the platform file. A source added upstream has to be added here:
/// the list is the link's contents, so an omission is an undefined symbol, not a
/// silently missing feature.
const ENGINE: [&str; 80] = [
    "dummy",
    "am_map",
    "doomdef",
    "doomstat",
    "dstrings",
    "d_event",
    "d_items",
    "d_iwad",
    "d_loop",
    "d_main",
    "d_mode",
    "d_net",
    "f_finale",
    "f_wipe",
    "g_game",
    "hu_lib",
    "hu_stuff",
    "info",
    "i_cdmus",
    "i_endoom",
    "i_joystick",
    "i_scale",
    "i_sound",
    "i_system",
    "i_timer",
    "memio",
    "m_argv",
    "m_bbox",
    "m_cheat",
    "m_config",
    "m_controls",
    "m_fixed",
    "m_menu",
    "m_misc",
    "m_random",
    "p_ceilng",
    "p_doors",
    "p_enemy",
    "p_floor",
    "p_inter",
    "p_lights",
    "p_map",
    "p_maputl",
    "p_mobj",
    "p_plats",
    "p_pspr",
    "p_saveg",
    "p_setup",
    "p_sight",
    "p_spec",
    "p_switch",
    "p_telept",
    "p_tick",
    "p_user",
    "r_bsp",
    "r_data",
    "r_draw",
    "r_main",
    "r_plane",
    "r_segs",
    "r_sky",
    "r_things",
    "sha1",
    "sounds",
    "statdump",
    "st_lib",
    "st_stuff",
    "s_sound",
    "tables",
    "v_video",
    "wi_stuff",
    "w_checksum",
    "w_file",
    "w_main",
    "w_wad",
    "z_zone",
    "w_file_stdc",
    "i_input",
    "i_video",
    "doomgeneric",
];

pub fn run(root: &Path, target: Option<&str>) -> io::Result<bool> {
    let lld = toolchain::find_lld(root).ok_or_else(toolchain::missing_lld)?;

    let arches: Vec<&Arch> = match target {
        Some("all") => arch::ALL.to_vec(),
        Some(token) => vec![arch::resolve(token)?],
        None => vec![arch::DEFAULT],
    };

    fetch(root)?;
    for arch in arches {
        if !build(root, arch, &lld)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Check out the pinned engine and make sure the pinned IWAD is on disk.
pub fn fetch(root: &Path) -> io::Result<()> {
    let pins = pin::load(root)?;
    let commit = pins.get("doom.commit")?.to_string();
    let url = pins.get("doom.git")?.to_string();
    let src = root.join(SRC);

    if !src.join(".git").is_dir() {
        if let Some(parent) = src.parent() {
            fs::create_dir_all(parent)?;
        }
        if !run_git(&[
            "clone".to_string(),
            url.clone(),
            src.to_string_lossy().into_owned(),
        ])? {
            return Err(io::Error::other(format!("cloning {url} failed")));
        }
    } else if !have_commit(&src, &commit)? {
        println!("[doom] the pinned commit is not in the local clone; fetching");
        if !run_git(&[
            "-C".to_string(),
            src.to_string_lossy().into_owned(),
            "fetch".to_string(),
            "origin".to_string(),
        ])? {
            return Err(io::Error::other("fetching doomgeneric failed"));
        }
    }

    if !run_git(&[
        "-C".to_string(),
        src.to_string_lossy().into_owned(),
        "checkout".to_string(),
        "--force".to_string(),
        commit.clone(),
    ])? {
        return Err(io::Error::other(format!(
            "checking out {commit} in {} failed (is the commit published?)",
            src.display()
        )));
    }
    let head = git_output(&[
        "-C".to_string(),
        src.to_string_lossy().into_owned(),
        "rev-parse".to_string(),
        "HEAD".to_string(),
    ])?;
    if head.trim() != commit {
        return Err(io::Error::other(format!(
            "{} is at {}, not the pinned {commit}",
            src.display(),
            head.trim()
        )));
    }

    fetch_wad(root, &pins)
}

/// Download the IWAD once and hold it to the pinned digest.
///
/// The shareware release is freely redistributable but it is not in the engine's
/// repository, so the pin is the digest: a mirror that changes the file fails the
/// build instead of rendering a different game.
fn fetch_wad(root: &Path, pins: &pin::Pins) -> io::Result<()> {
    let dest = root.join(WAD);
    let wanted = pins.get("doom.wad.sha256")?;

    if dest.is_file() && fetch_stage1::sha256_file(&dest)? == wanted {
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let url = pins.get("doom.wad.url")?;
    println!("[doom] fetching the shareware IWAD from {url}");
    fetch_stage1::download(
        url,
        &dest,
        "the pinned shareware IWAD is not at that URL; fix doom.wad.url in tools/pins.ini",
    )?;
    let got = fetch_stage1::sha256_file(&dest)?;
    if got != wanted {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("doom1.wad checksum mismatch\n       expected {wanted}\n       got      {got}"),
        ));
    }
    println!("[doom] IWAD checksum ok");
    Ok(())
}

fn build(root: &Path, arch: &Arch, lld: &Path) -> io::Result<bool> {
    let libc = libc_static::archive(root, arch)?;
    let release = root.join("target").join(arch.triple).join("release");
    let work = root.join("target").join("doom").join(arch.name);
    fs::create_dir_all(&release)?;
    fs::create_dir_all(&work)?;

    let src = root.join(SRC).join("doomgeneric");
    let cflags = compile_flags(root, arch, &src)?;

    let mut objects = Vec::with_capacity(ENGINE.len() + 2);
    for name in ENGINE {
        let source = src.join(format!("{name}.c"));
        if !source.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} is missing; the pinned doomgeneric tree is incomplete (run `cargo xtask build doom` again)",
                    source.display()
                ),
            ));
        }
        let object = work.join(format!("{name}.o"));
        if !compile(&cflags, &source, &object)? {
            return Ok(false);
        }
        objects.push(object);
    }

    let platform = root.join("tools").join("doom").join("doomgeneric_minix.c");
    let platform_object = work.join("doomgeneric_minix.o");
    if !compile(&cflags, &platform, &platform_object)? {
        return Ok(false);
    }
    objects.push(platform_object);

    let mut crt0_flags = arch.base_cflags();
    crt0_flags.push("-c".to_string());
    crt0_flags.extend(cc::compile_flags(root)?);
    let crt0 = work.join("crt0.o");
    let mut assemble = Command::new("clang");
    assemble
        .args(&crt0_flags)
        .arg("-o")
        .arg(&crt0)
        .arg(arch.crt0(root));
    if !cc::run(&mut assemble)? {
        return Ok(false);
    }

    let out = root
        .join("target")
        .join(arch.triple)
        .join("release")
        .join(PROGRAM);
    let mut link = Command::new(lld);
    link.args(["-flavor", "gnu"])
        .args(link_args(arch, &crt0, &objects, &libc, root, &out));
    if !cc::run(&mut link)? {
        return Ok(false);
    }
    println!("wrote {}", out.display());
    Ok(true)
}

fn compile_flags(root: &Path, arch: &Arch, src: &Path) -> io::Result<Vec<String>> {
    let mut flags = arch.base_cflags();
    flags.extend(["-fno-builtin", "-O2", "-c"].map(String::from));
    flags.extend(cc::compile_flags(root)?);
    flags.push(format!("-I{}", root.join("tools").join("doom").display()));
    flags.push(format!("-I{}", src.display()));
    for define in DEFINES {
        flags.push(format!("-D{define}"));
    }
    // The engine is 1993-era C from another project; its warnings are not this
    // port's to fix, and a wall of them would hide the one that is.
    flags.push("-w".to_string());
    Ok(flags)
}

fn compile(flags: &[String], source: &Path, object: &Path) -> io::Result<bool> {
    let mut command = Command::new("clang");
    command.args(flags).arg("-o").arg(object).arg(source);
    cc::run(&mut command)
}

/// The LLD line, as a vector so the per-arch values can be asserted.
fn link_args(
    arch: &Arch,
    crt0: &Path,
    objects: &[PathBuf],
    libc: &Path,
    root: &Path,
    out: &Path,
) -> Vec<String> {
    let mut args = vec![
        "-o".to_string(),
        out.display().to_string(),
        format!("--image-base={}", arch.image_base),
        format!("--defsym=BASE_ADDRESS={}", arch.image_base),
        format!("--defsym=LOADER_BASE={}", arch.loader_base),
        "-T".to_string(),
        linker_script(root).display().to_string(),
        "-z".to_string(),
        "norelro".to_string(),
        "--no-eh-frame-hdr".to_string(),
        crt0.display().to_string(),
    ];
    args.extend(objects.iter().map(|object| object.display().to_string()));
    args.push(libc.display().to_string());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_list_is_upstreams_without_a_platform_backend() {
        assert_eq!(ENGINE.len(), 80);
        assert!(ENGINE.contains(&"d_main"));
        assert!(ENGINE.contains(&"i_video"));
        assert!(ENGINE.contains(&"doomgeneric"));
        assert!(
            !ENGINE.iter().any(|name| name.starts_with("doomgeneric_")),
            "the platform layer is this repository's, not an upstream backend"
        );
    }

    #[test]
    fn the_engine_list_has_no_duplicates() {
        let mut sorted = ENGINE.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ENGINE.len());
    }

    #[test]
    fn the_defines_pin_the_native_frame_and_the_platform_family() {
        assert!(DEFINES.contains(&"DOOMGENERIC_RESX=320"));
        assert!(DEFINES.contains(&"DOOMGENERIC_RESY=200"));
        assert!(DEFINES.contains(&"LINUX"));
    }

    #[test]
    fn the_link_pins_the_archs_image_base_and_loader_base() {
        let objects = vec![
            PathBuf::from("d_main.o"),
            PathBuf::from("doomgeneric_minix.o"),
        ];
        let args = link_args(
            &arch::X86_64,
            Path::new("crt0.o"),
            &objects,
            Path::new("libc.a"),
            Path::new("repo"),
            Path::new("doom"),
        );
        assert!(args.contains(&"--image-base=0x4000000".to_string()));
        assert!(args.contains(&"--defsym=LOADER_BASE=0x8000000".to_string()));
        assert!(args.iter().any(|arg| arg.ends_with("minix-user.ld")));
        assert!(args.contains(&"d_main.o".to_string()));
        assert!(args.contains(&"doomgeneric_minix.o".to_string()));
        assert_eq!(args.last().unwrap(), "libc.a");
    }

    #[test]
    fn the_iwad_and_the_sources_live_under_target() {
        assert!(
            WAD.replace('\\', "/")
                .ends_with("target/doom-data/doom1.wad")
        );
        assert!(SRC.replace('\\', "/").ends_with("target/doom-src"));
    }
}
