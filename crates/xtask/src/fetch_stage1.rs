//! Install the prebuilt stage1 toolchain for the pinned rust commit.
//!
//! `just bootstrap` builds the fork's stage1 compiler from source, LLVM included:
//! tens of minutes and tens of GB. This downloads the same toolchain from a
//! release published in this repository instead (see
//! `.github/workflows/toolchain-release.yml`), tagged by the commit the `rust`
//! submodule pins, so the toolchain always matches the sources in the tree.
//!
//! `just bootstrap` stays authoritative: it is what you run when the fork itself
//! changes (std edits, a new arch, a rebase that moves the LLVM pin). This is the
//! cache for everyone else.
//!
//! An existing toolchain is never replaced unless asked for with `--force`. The
//! toolchain cannot identify its own commit (a bootstrap-built rustc reports
//! `commit-hash: unknown`), so a source build is indistinguishable from a fetched
//! one; the only record is the marker this writes next to what it installs.
//!
//! The transport is `curl`: a named tool whose absence is an error that names it,
//! the same policy as clang and QEMU, and cheaper than a TLS stack in a build
//! tool. Two environment overrides exist for testing and for consuming a release
//! from elsewhere:
//!
//! - `MINIXRS_STAGE1_BASE` is a base URL holding `<asset>` and `SHA256SUMS`, by
//!   default the fork's release for the pinned commit. `file://` works, which is
//!   how the fetcher is tested without a network.
//! - `MINIXRS_STAGE1_DEST` is the install directory, by default
//!   `rust/build/<host>`.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::toolchain;

const RELEASE_REPO: &str = "sdgoij/minixrs";
const MINIX_TARGETS: [&str; 3] = [
    "x86_64-pc-minix",
    "riscv64gc-unknown-minix",
    "aarch64-unknown-minix",
];
/// Written inside the installed toolchain, the only way to know which commit a
/// fetched one came from.
const MARKER: &str = ".minixrs-fetched";

pub fn run(root: &Path, force: bool) -> io::Result<()> {
    let sha = pinned_sha(root)?;
    let host = toolchain::host_triple().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cannot run `rustc -vV` to detect the host triple, which names the release asset",
        )
    })?;
    let dest = std::env::var_os("MINIXRS_STAGE1_DEST")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("rust").join("build").join(&host));
    let stage1 = dest.join("stage1");

    if rustc_in(&stage1).is_some() && !force {
        match recorded_sha(&stage1) {
            Some(recorded) if recorded == sha => {
                println!(
                    "stage1 for {} is already installed at {}",
                    short(&sha),
                    stage1.display()
                );
            }
            Some(recorded) => {
                println!(
                    "the stage1 at {} was fetched for {}, not {}",
                    stage1.display(),
                    short(&recorded),
                    short(&sha)
                );
                println!(
                    "re-run with --force to replace it, or `just bootstrap` to build from source"
                );
            }
            None => {
                println!(
                    "a stage1 already exists at {} and did not come from here",
                    stage1.display()
                );
                println!("(a source build reports no commit, so it cannot be identified)");
                println!("re-run with --force to replace it with the published toolchain");
            }
        }
        return Ok(());
    }

    let asset_name = format!("stage1-{sha}-{host}.tar.xz");
    let base = std::env::var("MINIXRS_STAGE1_BASE")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            format!("https://github.com/{RELEASE_REPO}/releases/download/stage1-{sha}")
        });
    let base = base.trim_end_matches('/').to_string();

    let tmp = root.join("target").join("stage1-download");
    remove_dir_if_present(&tmp)?;
    fs::create_dir_all(&tmp)?;
    let asset = tmp.join(&asset_name);
    let sums = tmp.join("SHA256SUMS");

    println!("fetching {asset_name}");
    download(&format!("{base}/{asset_name}"), &asset)?;
    download(&format!("{base}/SHA256SUMS"), &sums)?;
    verify(&asset, &sums)?;

    // Replace rather than merge: a toolchain built from another commit may not
    // contain everything this one does, and leftovers from it would be used.
    remove_dir_if_present(&stage1)?;
    println!("extracting into {}", dest.display());
    extract(&asset, &dest)?;
    remove_dir_if_present(&tmp)?;

    if rustc_in(&stage1).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{asset_name} did not contain stage1/bin/rustc"),
        ));
    }
    let missing: Vec<&str> = MINIX_TARGETS
        .iter()
        .copied()
        .filter(|target| !stage1.join("lib/rustlib").join(target).join("lib").is_dir())
        .collect();
    if !missing.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the toolchain has no std for: {}", missing.join(", ")),
        ));
    }

    fs::write(
        stage1.join(MARKER),
        format!("{sha}  {base}/{asset_name}\n"),
    )?;
    let size = dir_size(&stage1)?;
    println!(
        "installed stage1 {} ({host}) at {} ({:.2} GB)",
        short(&sha),
        stage1.display(),
        size as f64 / 1e9
    );
    Ok(())
}

/// The commit the `rust` submodule pins, which is the identity of the toolchain.
fn pinned_sha(root: &Path) -> io::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root.join("rust"))
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "cannot read the rust submodule commit (`git -C rust rev-parse HEAD`); \
             run `git submodule update --init rust` first: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn rustc_in(stage1: &Path) -> Option<PathBuf> {
    let rustc = stage1.join("bin").join(toolchain::exe("rustc"));
    rustc.is_file().then_some(rustc)
}

/// The commit a fetched toolchain was installed for, or `None` when unknown.
fn recorded_sha(stage1: &Path) -> Option<String> {
    let text = fs::read_to_string(stage1.join(MARKER)).ok()?;
    text.split_whitespace().next().map(str::to_string)
}

/// Download `url` to `dest`, failing with the publish advice on a 404.
fn download(url: &str, dest: &Path) -> io::Result<()> {
    let output = Command::new("curl")
        .args(["--silent", "--show-error", "--location", "--output"])
        .arg(dest)
        .args(["--write-out", "%{http_code}"])
        .arg(url)
        .output()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot run `curl`, needed to download the toolchain: {error}"),
            )
        })?;
    let code = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // `%{http_code}` is `000` for a non-HTTP URL (`file://`, which the tests and
    // `MINIXRS_STAGE1_BASE` use); there curl's own exit status is the verdict.
    let fetched = output.status.success() && (code == "200" || code == "000");
    if fetched {
        return Ok(());
    }

    remove_file_if_present(dest)?;
    if code == "404" {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "no prebuilt stage1 at {url}\n\
                 Nothing has been published for this rust commit and host yet.\n\
                 Run `just bootstrap` to build the toolchain from source, or publish a\n\
                 release with the `toolchain release` workflow."
            ),
        ));
    }
    Err(io::Error::other(format!(
        "downloading {url} failed (curl exit {}, HTTP {code}): {}",
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// Check `asset` against its line in `sums` (a `sha256sum` file).
fn verify(asset: &Path, sums: &Path) -> io::Result<()> {
    let name = asset
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| io::Error::other(format!("{} has no file name", asset.display())))?;
    let text = fs::read_to_string(sums)?;
    let wanted = wanted_hash(&text, name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} has no entry for {name}", sums.display()),
        )
    })?;

    let actual = sha256_file(asset)?;
    if actual != wanted {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("checksum mismatch for {name}\n       expected {wanted}\n       got      {actual}"),
        ));
    }
    println!("checksum ok: {name}");
    Ok(())
}

/// The digest `sums` records for `name`, from a `sha256sum` file.
fn wanted_hash(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        match fields.as_slice() {
            [hash, file] if file.trim_start_matches('*') == name => Some(hash.to_ascii_lowercase()),
            _ => None,
        }
    })
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn extract(asset: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    let file = fs::File::open(asset)?;
    let decoder = xz2::read::XzDecoder::new(file);
    // `Archive::unpack` refuses absolute paths and `..`, the same protection the
    // script's `tar` filter had; the archive is checksum-verified besides.
    tar::Archive::new(decoder).unpack(dest)
}

fn dir_size(path: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            total += dir_size(&entry.path())?;
        } else if file_type.is_file() {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

fn remove_dir_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
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

    #[test]
    fn the_digest_of_known_bytes_is_known() {
        let dir = temp_dir("minixrs-xtask-sha256");
        let file = dir.join("abc");
        fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_sum_entry_is_matched_by_name_and_case_is_folded() {
        let sums = "\
            aaaa  other.tar.xz\n\
            BBBB  *stage1-x.tar.xz\n\
            cccc  stage1-x.tar.xz  extra\n";
        assert_eq!(
            wanted_hash(sums, "stage1-x.tar.xz").as_deref(),
            Some("bbbb")
        );
        assert_eq!(wanted_hash(sums, "missing.tar.xz"), None);
    }

    #[test]
    fn a_marker_records_the_commit() {
        let dir = temp_dir("minixrs-xtask-marker");
        assert_eq!(recorded_sha(&dir), None);
        fs::write(
            dir.join(MARKER),
            "abc123  https://example/stage1-abc123-host.tar.xz\n",
        )
        .unwrap();
        assert_eq!(recorded_sha(&dir).as_deref(), Some("abc123"));
        fs::write(dir.join(MARKER), " \n").unwrap();
        assert_eq!(recorded_sha(&dir), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_xz_archive_is_extracted_into_the_destination() {
        let dir = temp_dir("minixrs-xtask-fetch-extract");
        let asset = dir.join("stage1.tar.xz");
        let out = dir.join("out");

        let file = fs::File::create(&asset).unwrap();
        let encoder = xz2::write::XzEncoder::new(file, 6);
        let mut builder = tar::Builder::new(encoder);
        let payload = b"stub";
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "stage1/bin/rustc", &payload[..])
            .unwrap();
        builder.into_inner().unwrap().finish().unwrap();

        extract(&asset, &out).unwrap();
        assert_eq!(
            fs::read_to_string(out.join("stage1/bin/rustc")).unwrap(),
            "stub"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_short_form_is_twelve_characters() {
        assert_eq!(short("0123456789abcdef"), "0123456789ab");
        assert_eq!(short("abc"), "abc");
    }
}
