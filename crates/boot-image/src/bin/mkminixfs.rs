//! Thin CLI wrapper for building the MinixFS root image.
//!
//! Usage: `cargo run -p boot-image --bin mkminixfs [x86_64|riscv64|aarch64|wasm32]`
//!
//! Reads the already-built userland + server binaries from the shared
//! `target/<triple>/release/` dir (built by `just build <target>`) and
//! writes `target/images/<triple>/minixfs.img`. The kernel build pipeline
//! assembles the same image directly via `crates/kernel/build.rs` when it embeds
//! a root; this CLI is the *disk* image builder, and reads `MINIXFS_EXTRA` and
//! `DYNLINK_BINS` (`dest=path;…`) plus `MINIXFS_BLOCKS`, so a system image larger
//! than the kernel's 16 MiB ramdisk window needs no kernel change.
//!
//! `wasm32` is the one arch whose image is not built by `just build`: the files it wants are
//! *modules* (`manifest::WASM_MODULES`), the module build is a separate cargo workspace, and the
//! Asyncify pass between the two is `tools/wasm-servers/run.sh`'s — so the harness stages the
//! finished modules in `target/wasm32-minix/release/` and this CLI reads them from there, exactly
//! as it reads every other target's binaries.

use std::path::Path;
use std::process::ExitCode;

use boot_image::{manifest, minixfs, targets};

fn main() -> ExitCode {
    let arch = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "x86_64".to_string());
    let t = match targets::target_from_arch(&arch) {
        Some(t) => t,
        None => {
            eprintln!("mkminixfs: unknown arch '{arch}' (use x86_64, riscv64, aarch64, or wasm32)");
            return ExitCode::FAILURE;
        }
    };

    let workspace = Path::new(".");
    let release = targets::release_dir(&t, workspace);

    // Which files an image holds is the arch's business: three archs ship the same userland
    // executables under different triples, and wasm32 ships modules instead.
    let wanted: &[(&str, &str)] = if t.arch == "wasm32" {
        manifest::WASM_MODULES
    } else {
        manifest::BOOT_BINS
    };

    let mut files: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for &(dest, bin_name) in wanted {
        // The C smoke tests are x86_64-only (their builders are), so an image for
        // another arch never has them to carry. `coreutils` is carried on every other
        // arch, but aarch64's multicall loses output (KNOWN_ISSUES aarch64 #9) and is
        // therefore opt-in there: set `MINIXFS_COREUTILS_AARCH64` to include it while
        // chasing the bug.
        if matches!(dest, "/bin/helloc" | "/bin/ctest") && t.arch != "x86_64" {
            continue;
        }
        if dest == "/bin/coreutils"
            && t.arch == "aarch64"
            && std::env::var_os("MINIXFS_COREUTILS_AARCH64").is_none()
        {
            continue;
        }
        let src = release.join(bin_name);
        if src.exists() {
            match std::fs::read(&src) {
                Ok(data) if !data.is_empty() => files.push((dest, data)),
                Ok(_) => eprintln!("mkminixfs: WARNING: {} is empty", src.display()),
                Err(e) => {
                    eprintln!("mkminixfs: failed to read {}: {e}", src.display());
                    return ExitCode::FAILURE;
                }
            }
        } else if t.arch == "wasm32" {
            // A missing *module* is not a warning: an image whose `/bin/sh` is absent would
            // boot and then fail every exec with a path error that points at the filesystem
            // rather than at the build that did not run.
            eprintln!(
                "mkminixfs: {} not found at {} (run `sh tools/wasm-servers/run.sh`)",
                bin_name,
                src.display()
            );
            return ExitCode::FAILURE;
        } else {
            eprintln!(
                "mkminixfs: WARNING: {bin_name} not found at {} (run `just build {arch}`)",
                src.display()
            );
        }
    }

    // MINIXFS_EXTRA / DYNLINK_BINS: extra "dest=path" files for the *disk* image.
    // The image builder reads them here, not `crates/kernel/build.rs`, so a system
    // image larger than the kernel's 16 MiB ramdisk window (the Mesa DSOs are ~36 MiB)
    // needs no kernel change and no embedded root. A POSIX-style dest is converted by
    // MSYS on the way to a native tool, so a dest that is not one of the image's
    // directories is refused while its provenance is still known.
    if t.arch != "wasm32" {
        for (var, prefixes) in [
            (
                "DYNLINK_BINS",
                &["/bin/", "/sbin/", "/lib/", "/libexec/"][..],
            ),
            (
                "MINIXFS_EXTRA",
                &["/bin/", "/sbin/", "/etc/", "/lib/", "/libexec/"][..],
            ),
        ] {
            let Ok(list) = std::env::var(var) else {
                continue;
            };
            for entry in list.split(';').filter(|s| !s.is_empty()) {
                let (dest, path) = entry
                    .split_once('=')
                    .unwrap_or_else(|| panic!("mkminixfs: {var} entry must be dest=path"));
                if !prefixes.iter().any(|dir| dest.starts_with(dir)) {
                    panic!(
                        "mkminixfs: {var}: {dest:?} is not one of {prefixes:?}. A POSIX-style \
                         value is converted on the way to a native tool, and a converted dest \
                         lands in the root filesystem. Set MSYS2_ENV_CONV_EXCL={var}."
                    );
                }
                // `build_minixfs` wants `&'static str` dests; the env string is
                // transient, so leak each one (the CLI runs once).
                let dest: &'static str = Box::leak(dest.to_owned().into_boxed_str());
                let p = Path::new(path);
                let p = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    workspace.join(p)
                };
                let data = std::fs::read(&p).unwrap_or_else(|e| {
                    panic!("mkminixfs: {var}: reading {} failed ({e})", p.display())
                });
                files.push((dest, data));
            }
        }
    }

    let image = minixfs::build_minixfs(&files);

    let img_path = workspace
        .join("target")
        .join("images")
        .join(t.out_dir)
        .join("minixfs.img");
    if let Some(parent) = img_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if write_if_changed(&img_path, &image).is_err() {
        eprintln!("mkminixfs: failed to write {}", img_path.display());
        return ExitCode::FAILURE;
    }
    println!("minixfs.img: {} bytes, {} files", image.len(), files.len());
    ExitCode::SUCCESS
}

fn write_if_changed(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Ok(existing) = std::fs::read(path)
        && existing == data
    {
        return Ok(());
    }
    std::fs::write(path, data)
}
