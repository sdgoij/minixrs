//! The pinned third-party sources and the release tag an artifact is identified by.
//!
//! The values live in `tools/pins.ini`, which the Python builders that still run
//! under WSL read too, so there is one source of truth. The release tag includes
//! the `rust` gitlink (`git rev-parse HEAD:rust`): a fork bump can change the
//! target ABI, so an artifact built before it must not be handed out for the new
//! one. That is what makes a fork bump rebuild rather than ship a stale binary.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use crate::fetch_stage1::short;

/// Written in an installed artifact, the only record of which release it came from.
pub const MARKER: &str = ".minixrs-fetched";

pub struct Pins(BTreeMap<String, String>);

/// Read `tools/pins.ini`: `key=value` lines, `#` comments.
pub fn load(root: &Path) -> io::Result<Pins> {
    let path = root.join("tools").join("pins.ini");
    let text = fs::read_to_string(&path)?;
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    Ok(Pins(values))
}

impl Pins {
    pub fn get(&self, key: &str) -> io::Result<&str> {
        self.0.get(key).map(String::as_str).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("tools/pins.ini has no {key}"),
            )
        })
    }
}

/// The commit the `rust` submodule pins, or an error naming the failure.
///
/// Reading the gitlink rather than `git -C rust rev-parse HEAD` works whether or
/// not the submodule is checked out, which is what lets a read-only `*-pin` job
/// resolve a tag without fetching its history.
pub fn rust_gitlink(root: &Path) -> io::Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD:rust"])
        .current_dir(root)
        .output()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot read the pinned `rust` commit: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "`git rev-parse HEAD:rust` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if commit.len() < 12 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("`HEAD:rust` is not a commit: {commit:?}"),
        ));
    }
    Ok(commit)
}

/// The bash release tag: the bash commit and the toolchain pin.
pub fn bash_tag(root: &Path) -> io::Result<String> {
    let pins = load(root)?;
    Ok(bash_tag_for(pins.get("bash.commit")?, &rust_gitlink(root)?))
}

/// The Mesa release tag: both commits, the build config and the toolchain pin.
pub fn mesa_tag(root: &Path) -> io::Result<String> {
    let pins = load(root)?;
    Ok(mesa_tag_for(
        pins.get("mesa.commit")?,
        pins.get("drm.commit")?,
        pins.get("mesa.build_config")?,
        &rust_gitlink(root)?,
    ))
}

pub(crate) fn bash_tag_for(bash: &str, rust: &str) -> String {
    format!("bash-{}-r{}", short(bash), short(rust))
}

pub(crate) fn mesa_tag_for(mesa: &str, drm: &str, config: &str, rust: &str) -> String {
    format!(
        "mesa-{}-{}-r{}-c{}",
        short(mesa),
        short(drm),
        short(rust),
        config
    )
}

/// Print one pin, for the release workflows and for a human.
pub fn run(root: &Path, matches: &clap::ArgMatches) -> io::Result<()> {
    let pins = load(root)?;
    let value = match matches.subcommand() {
        Some(("bash", matches)) => {
            let field = matches
                .get_one::<String>("field")
                .map(String::as_str)
                .unwrap_or("tag");
            match field {
                "tag" => bash_tag(root)?,
                "describe" => pins.get("bash.describe")?.to_string(),
                "commit" => pins.get("bash.commit")?.to_string(),
                _ => unreachable!("clap limits the field"),
            }
        }
        Some(("mesa", matches)) => {
            let field = matches
                .get_one::<String>("field")
                .map(String::as_str)
                .unwrap_or("tag");
            match field {
                "tag" => mesa_tag(root)?,
                "mesa-tag" => pins.get("mesa.tag")?.to_string(),
                "mesa-commit" => pins.get("mesa.commit")?.to_string(),
                "drm-tag" => pins.get("drm.tag")?.to_string(),
                "drm-commit" => pins.get("drm.commit")?.to_string(),
                "build-config" => pins.get("mesa.build_config")?.to_string(),
                _ => unreachable!("clap limits the field"),
            }
        }
        _ => unreachable!("clap requires a pin subcommand"),
    };
    println!("{value}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_is_the_short_commits_and_the_build_config() {
        assert_eq!(
            bash_tag_for("b460816602167718f78a6233164e8875f49b75b2", "0123456789abcdef"),
            "bash-b46081660216-r0123456789ab"
        );
        assert_eq!(
            mesa_tag_for(
                "06f9e28304d5d3f109c33535c1c25b9df5769af2",
                "a8e5e10a873f67f557dc70e5407af4553f35edd9",
                "gbm",
                "0123456789abcdef"
            ),
            "mesa-06f9e28304d5-a8e5e10a873f-r0123456789ab-cgbm"
        );
    }

    #[test]
    fn the_repository_pins_file_names_every_key() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let pins = load(&root).unwrap();
        for key in [
            "bash.commit",
            "bash.describe",
            "mesa.commit",
            "drm.commit",
            "mesa.build_config",
        ] {
            assert!(pins.get(key).is_ok(), "missing {key}");
        }
    }
}
