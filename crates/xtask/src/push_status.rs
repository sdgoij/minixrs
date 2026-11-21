//! List every repository in this tree whose commits are not on its remote.
//!
//! The superproject and every registered submodule are checked, so a fork left
//! behind in a push shows up here instead of being discovered later. Each
//! repository is compared with its own upstream branch; nothing is fetched unless
//! `--fetch` is given, so without it the answer is only as fresh as the last
//! fetch. Exit status is 1 when something needs a push or is dirty, so a release
//! gate can call this.

use std::io;
use std::path::Path;
use std::process::Command;

#[derive(Clone)]
struct Info {
    branch: String,
    upstream: Option<String>,
    ahead: Option<i64>,
    behind: Option<i64>,
    dirty: bool,
}

pub fn run(root: &Path, fetch: bool, commits: bool, table: bool) -> io::Result<bool> {
    let paths = repositories(root);
    if fetch {
        for path in &paths {
            git(root, path, &["fetch", "--quiet", "--prune"]);
        }
    }

    let mut rows = Vec::new();
    let mut needs_push = Vec::new();
    let mut dirty = Vec::new();
    for path in &paths {
        let info = describe(root, path);
        if info.ahead.unwrap_or(0) > 0 {
            needs_push.push((path.clone(), info.clone()));
        }
        if info.dirty {
            dirty.push(path.clone());
        }
        rows.push((path.clone(), info));
    }

    if table {
        let width = rows
            .iter()
            .map(|(path, _)| path.len())
            .max()
            .unwrap_or(0)
            + 2;
        println!(
            "{:<width$}{:<24}{:>6}{:>7}  tree",
            "repo", "branch", "ahead", "behind"
        );
        for (path, info) in &rows {
            let ahead = info.ahead.map_or_else(|| "-".to_string(), |n| n.to_string());
            let behind = info
                .behind
                .map_or_else(|| "-".to_string(), |n| n.to_string());
            let tree = if info.dirty { "dirty" } else { "clean" };
            let upstream = if info.upstream.is_none() {
                "  (no upstream)"
            } else {
                ""
            };
            println!(
                "{path:<width$}{:<24}{ahead:>6}{behind:>7}  {tree}{upstream}",
                info.branch
            );
        }
        if !fetch {
            println!("remote-tracking refs are as of the last fetch; pass --fetch to refresh them");
        }
    }

    if !table {
        if needs_push.is_empty() {
            println!("nothing to push");
        } else {
            for (path, info) in &needs_push {
                println!("{path} {} {}", info.branch, info.ahead.unwrap_or(0));
                if commits {
                    for subject in unpushed_subjects(root, path) {
                        println!("  {subject}");
                    }
                }
            }
        }
    }

    for path in &dirty {
        println!("{path} dirty");
    }
    Ok(needs_push.is_empty() && dirty.is_empty())
}

fn repositories(root: &Path) -> Vec<String> {
    let mut paths = vec![".".to_string()];
    let listing = git(root, ".", &["submodule", "status"]).unwrap_or_default();
    for path in submodule_paths(&listing) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// The paths in `git submodule status` output, skipping a submodule that is not
/// checked out (a leading `-`), which has nothing to compare.
fn submodule_paths(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('-'))
        .filter_map(|line| {
            let fields: Vec<&str> = line[1..].split_whitespace().collect();
            fields.get(1).map(|path| path.to_string())
        })
        .collect()
}

fn describe(root: &Path, path: &str) -> Info {
    let branch = git(root, path, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_else(|| "?".into());
    let upstream = git(
        root,
        path,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"],
    );
    let (ahead, behind) = match &upstream {
        Some(upstream) => {
            let range = format!("{upstream}...HEAD");
            let counts = git(root, path, &["rev-list", "--left-right", "--count", &range]);
            match counts.and_then(|counts| {
                let mut fields = counts.split_whitespace();
                Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
            }) {
                Some((behind, ahead)) => (Some(ahead), Some(behind)),
                None => (None, None),
            }
        }
        None => {
            // A detached vendored checkout (llvm-project, the Wayland protocol
            // trees) still has an answer: how many commits no remote-tracking ref
            // reaches.
            let unreachable = git(root, path, &["rev-list", "--count", "HEAD", "--not", "--remotes"]);
            (unreachable.and_then(|count| count.parse().ok()), None)
        }
    };
    let dirty = git(root, path, &["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    Info {
        branch,
        upstream,
        ahead,
        behind,
        dirty,
    }
}

fn unpushed_subjects(root: &Path, path: &str) -> Vec<String> {
    git(root, path, &["log", "--oneline", "--no-decorate", "@{upstream}..HEAD"])
        .map(|subjects| {
            subjects
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `git -C <path> <args>` from the repository root, or `None` when it fails.
fn git(root: &Path, path: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_checked_out_submodule_is_a_path_and_a_missing_one_is_skipped() {
        let listing = "\
 1111111111111111111111111111111111111111 rust (v1)
+2222222222222222222222222222222222222222 wayland (v2)
-3333333333333333333333333333333333333333 vendor
";
        assert_eq!(submodule_paths(listing), vec!["rust", "wayland"]);
    }
}
