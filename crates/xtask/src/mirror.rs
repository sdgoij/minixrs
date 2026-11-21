//! Create or refresh the `sdgoij/bash` GitHub mirror the build clones from.
//!
//! savannah is slow and times out, so the build does not clone from it: CI and dev
//! machines clone the mirror named in `tools/pins.ini`. This is how that mirror is
//! created and refreshed, a rare local step, which is the point: the flaky fetch
//! never sits on a CI critical path.
//!
//! The objects are taken from the pinned upstream, never from another mirror of
//! unknown provenance, so this project is the anchor for what the mirror holds. The
//! pinned commit is verified present before anything is pushed, so a pin upstream
//! has not published cannot be mirrored by accident.
//!
//! `--check` only verifies the pin is present, without pushing.
//! `MINIXRS_BASH_UPSTREAM`, `MINIXRS_BASH_PUSH` and `MINIXRS_BASH_RETRIES` override
//! the upstream URL, the push URL and the fetch attempt count.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use crate::fetch_stage1::short;
use crate::pin;

pub fn run(root: &Path, matches: &clap::ArgMatches) -> io::Result<bool> {
    match matches.subcommand() {
        Some(("bash", matches)) => bash(root, matches.get_flag("check")),
        _ => unreachable!("clap requires a mirror subcommand"),
    }
}

fn bash(root: &Path, check: bool) -> io::Result<bool> {
    let pins = pin::load(root)?;
    let upstream = override_or("MINIXRS_BASH_UPSTREAM", pins.get("bash.upstream")?);
    let push = override_or("MINIXRS_BASH_PUSH", pins.get("bash.push")?);
    let commit = pins.get("bash.commit")?.to_string();
    let describe = pins.get("bash.describe")?.to_string();
    let clone = pins.get("bash.git")?.to_string();
    let retries = parse_retries(env::var("MINIXRS_BASH_RETRIES").ok().as_deref())?;
    let mirror = mirror_path(root);

    refresh(&mirror, &upstream, retries)?;

    if !have_commit(&mirror, &commit)? {
        return Err(io::Error::other(format!(
            "{describe} ({commit}) is not in the mirror after refreshing {upstream}; \
             it is the commit the build pins, so the mirror is not usable as it stands"
        )));
    }
    note(&format!("{describe} ({}) is present", short(&commit)));

    if check {
        return Ok(true);
    }

    if !run_command(
        Command::new("git")
            .arg("-C")
            .arg(&mirror)
            .args(["push", "--mirror"])
            .arg(&push),
    )? {
        return Err(io::Error::other(format!(
            "could not push to {push}; create the GitHub repository empty (no README, \
             no .gitignore) and make sure this host's key can push to it \
             (MINIXRS_BASH_PUSH overrides the remote)"
        )));
    }
    note(&format!("pushed to {push}; the build clones {clone}"));
    Ok(true)
}

fn mirror_path(root: &Path) -> PathBuf {
    root.join("target").join("bash-mirror.git")
}

/// Clone the mirror once, then fetch it, retrying against savannah's timeouts.
fn refresh(mirror: &Path, upstream: &str, retries: u32) -> io::Result<()> {
    for attempt in 1..=retries {
        let fresh = !mirror.is_dir();
        if fresh {
            if let Some(parent) = mirror.parent() {
                fs::create_dir_all(parent)?;
            }
            note(&format!(
                "cloning {upstream} (a full mirror, ~290 MiB the first time)"
            ));
        }
        let mut git = Command::new("git");
        if fresh {
            git.args(["-c", "core.autocrlf=false"])
                .args(["clone", "--mirror"])
                .arg(upstream)
                .arg(mirror);
        } else {
            git.arg("-C")
                .arg(mirror)
                .args(["remote", "update", "--prune"]);
        }
        if run_command(&mut git)? {
            return Ok(());
        }
        // A failed clone leaves a half-fetched directory behind; drop it so the
        // next attempt starts clean rather than resuming something unusable.
        if fresh && mirror.is_dir() {
            fs::remove_dir_all(mirror)?;
        }
        note(&format!("attempt {attempt}/{retries} failed"));
        // The point of the retries is to ride out savannah's timeouts, not to spin.
        if attempt < retries {
            thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    Err(io::Error::other(format!(
        "could not refresh the mirror from {upstream} after {retries} attempts \
         (savannah is slow; retry later, or point MINIXRS_BASH_UPSTREAM at a copy)"
    )))
}

fn have_commit(mirror: &Path, commit: &str) -> io::Result<bool> {
    let status = Command::new("git")
        .arg("-C")
        .arg(mirror)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{commit}^{{commit}}"))
        .status()
        .map_err(|error| {
            io::Error::new(error.kind(), format!("cannot run git: {error} (is git installed?)"))
        })?;
    Ok(status.success())
}

fn run_command(command: &mut Command) -> io::Result<bool> {
    let printable: Vec<String> = std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|word| word.to_string_lossy().into_owned())
        .collect();
    eprintln!("+ {}", printable.join(" "));
    Ok(command.status()?.success())
}

fn note(message: &str) {
    println!("[bash] {message}");
}

fn override_or(name: &str, fallback: &str) -> String {
    match env::var_os(name) {
        Some(value) if !value.is_empty() => value.to_string_lossy().into_owned(),
        _ => fallback.to_string(),
    }
}

fn parse_retries(value: Option<&str>) -> io::Result<u32> {
    let Some(value) = value else {
        return Ok(3);
    };
    value.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "MINIXRS_BASH_RETRIES must be an integer",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_retry_count_defaults_to_three_and_rejects_junk() {
        assert_eq!(parse_retries(None).unwrap(), 3);
        assert_eq!(parse_retries(Some("5")).unwrap(), 5);
        assert!(parse_retries(Some("many")).is_err());
    }

    #[test]
    fn the_mirror_sits_under_target_beside_the_build_checkout() {
        assert!(mirror_path(Path::new("root")).ends_with("target/bash-mirror.git"));
    }
}
