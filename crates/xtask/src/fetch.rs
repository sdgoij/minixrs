//! Install the prebuilt bash or Mesa DSOs for the pinned sources.
//!
//! `just build-bash` and `just build-mesa` are authoritative: they are what run
//! when the port's C headers, libc or toolchain change, or to bump a pin. These
//! download the artifact a release workflow published for the same pin instead,
//! so CI and most developers do not build 209 bash objects or Mesa from source.
//!
//! An existing artifact is never replaced unless asked for with `--force`, and a
//! marker records which release it came from (a source build leaves no marker,
//! so it is reported as coming from nowhere). Two environment overrides exist for
//! testing and for consuming a release from elsewhere: `MINIXRS_BASH_BASE` /
//! `MINIXRS_MESA_BASE` is a base URL holding `<asset>` and `SHA256SUMS`
//! (`file://` works), and `MINIXRS_BASH_DEST` / `MINIXRS_MESA_DEST` is the
//! install directory.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::arch;
use crate::fetch_stage1::{
    download, extract, remove_dir_if_present, remove_file_if_present, verify,
};
use crate::pin;

const RELEASE_REPO: &str = "sdgoij/minixrs";
/// The DSO the loader looks up first, and the header `gltriangle` includes.
const SONAME: &str = "libEGL.so.1";
const HEADER: &str = "include/EGL/egl.h";

pub fn bash(root: &Path, target: Option<&str>, force: bool) -> io::Result<bool> {
    let arch = resolve(target)?;
    let tag = pin::bash_tag(root)?;
    let dest = destination(
        "MINIXRS_BASH_DEST",
        root.join("target").join("bash").join(arch.name),
    );
    let binary = dest.join("bash");

    if binary.is_file() && !force {
        let recorded = recorded_tag(&dest);
        if recorded == tag {
            println!(
                "bash for {tag} is already installed at {}",
                binary.display()
            );
        } else if !recorded.is_empty() {
            println!(
                "the bash at {} was fetched for {recorded}, not {tag}",
                binary.display()
            );
            println!("re-run with --force to replace it, or `just build-bash` to build from source");
        } else {
            println!(
                "a bash already exists at {} and did not come from here",
                binary.display()
            );
            println!("(a source build leaves no marker, so it cannot be identified)");
            println!("re-run with --force to replace it with the published one");
        }
        return Ok(true);
    }

    let asset_name = format!("{tag}-{}.tar.xz", arch.name);
    let base = base("MINIXRS_BASH_BASE", &tag);
    let tmp = root.join("target").join("bash-download");
    let (asset, _sums) = download_pair(
        &tmp,
        &asset_name,
        &base,
        "bash",
        "Run `just build-bash <arch>` to build from source, or publish a release with the `bash release` workflow.",
    )?;

    remove_file_if_present(&binary)?;
    println!("extracting into {}", dest.display());
    extract(&asset, &dest)?;
    remove_dir_if_present(&tmp)?;

    if !binary.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{asset_name} did not contain bash"),
        ));
    }
    write_marker(&dest, &tag, &base, &asset_name)?;
    let size = fs::metadata(&binary)?.len();
    println!(
        "installed bash for {tag} ({}) at {} ({:.1} MB)",
        arch.name,
        binary.display(),
        size as f64 / 1e6
    );
    Ok(true)
}

pub fn mesa(root: &Path, target: Option<&str>, force: bool) -> io::Result<bool> {
    let arch = resolve(target)?;
    let tag = pin::mesa_tag(root)?;
    let dest = destination(
        "MINIXRS_MESA_DEST",
        root.join("target").join("mesa").join(arch.name),
    );
    let lib = dest.join("lib");

    if lib.join(SONAME).is_file() && !force {
        let recorded = recorded_tag(&dest);
        if recorded == tag {
            println!("Mesa DSOs for {tag} are already installed at {}", lib.display());
        } else if !recorded.is_empty() {
            println!(
                "the DSOs at {} were fetched for {recorded}, not {tag}",
                lib.display()
            );
            println!("re-run with --force to replace them, or `just build-mesa` to build from source");
        } else {
            println!("DSOs already exist at {} and did not come from here", lib.display());
            println!("(a source build leaves no marker, so it cannot be identified)");
            println!("re-run with --force to replace them with the published set");
        }
        return Ok(true);
    }

    let asset_name = format!("{tag}-{}.tar.xz", arch.name);
    let base = base("MINIXRS_MESA_BASE", &tag);
    let tmp = root.join("target").join("mesa-download");
    let (asset, _sums) = download_pair(
        &tmp,
        &asset_name,
        &base,
        "Mesa DSOs",
        "Run `just build-mesa <arch> --build` to build from source, or publish a release with the `mesa release` workflow.",
    )?;

    // Replace rather than merge: a set built for another pin may not contain
    // everything this one does, and leftovers would be staged into the image.
    remove_dir_if_present(&lib)?;
    println!("extracting into {}", dest.display());
    extract(&asset, &dest)?;
    remove_dir_if_present(&tmp)?;

    if !lib.join(SONAME).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{asset_name} did not contain lib/{SONAME}"),
        ));
    }
    if !dest.join(HEADER).is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{asset_name} did not contain {HEADER}"),
        ));
    }
    let count = fs::read_dir(&lib)?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".so"))
        .count();
    write_marker(&dest, &tag, &base, &asset_name)?;
    println!(
        "installed Mesa DSOs for {tag} ({}) at {} ({count} objects)",
        arch.name,
        lib.display()
    );
    Ok(true)
}

fn resolve(target: Option<&str>) -> io::Result<&'static arch::Arch> {
    match target {
        Some(token) => arch::resolve(token),
        None => Ok(arch::DEFAULT),
    }
}

fn destination(override_name: &str, fallback: PathBuf) -> PathBuf {
    match env::var_os(override_name) {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => fallback,
    }
}

fn base(override_name: &str, tag: &str) -> String {
    match env::var_os(override_name) {
        Some(value) if !value.is_empty() => value.to_string_lossy().into_owned(),
        _ => format!("https://github.com/{RELEASE_REPO}/releases/download/{tag}"),
    }
    .trim_end_matches('/')
    .to_string()
}

fn recorded_tag(dest: &Path) -> String {
    fs::read_to_string(dest.join(pin::MARKER))
        .ok()
        .and_then(|text| text.split_whitespace().next().map(str::to_string))
        .unwrap_or_default()
}

/// Fetch `<asset>` and `SHA256SUMS` into `tmp`, verify the checksum, and return
/// both paths.
fn download_pair(
    tmp: &Path,
    asset_name: &str,
    base: &str,
    what: &str,
    advice: &str,
) -> io::Result<(PathBuf, PathBuf)> {
    remove_dir_if_present(tmp)?;
    fs::create_dir_all(tmp)?;
    let asset = tmp.join(asset_name);
    let sums = tmp.join("SHA256SUMS");
    let asset_url = format!("{base}/{asset_name}");
    let sums_url = format!("{base}/SHA256SUMS");
    println!("fetching {asset_name}");
    download(
        &asset_url,
        &asset,
        &format!(
            "no prebuilt {what} at {asset_url}\n       \
             Nothing has been published for this pin yet.\n       {advice}"
        ),
    )?;
    download(&sums_url, &sums, &format!("no prebuilt {what} at {sums_url}"))?;
    verify(&asset, &sums)?;
    Ok((asset, sums))
}

fn write_marker(dest: &Path, tag: &str, base: &str, asset_name: &str) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    fs::write(
        dest.join(pin::MARKER),
        format!("{tag}  {base}/{asset_name}\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marker_names_the_tag_and_the_empty_one_names_nothing() {
        let dir = std::env::temp_dir().join("minixrs-xtask-fetch-marker");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(recorded_tag(&dir), "");
        fs::write(dir.join(pin::MARKER), "bash-abc-rdef  https://example/x\n").unwrap();
        assert_eq!(recorded_tag(&dir), "bash-abc-rdef");
        let _ = fs::remove_dir_all(&dir);
    }
}
