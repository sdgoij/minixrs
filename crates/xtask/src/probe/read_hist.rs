//! Summarise a `virtio_blk_handle_read` trace: request count, bytes, size
//! histogram.
//!
//! The recipe (`just test-read-cost-<arch>`) boots the guest with
//! `-trace enable=virtio_blk_handle_read`, then runs this over the trace file.
//! `--max` makes it a gate: a count above the ceiling fails, a count below it
//! passes, so a change that makes pages lazy can lower the bound rather than
//! trip it.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

use regex_lite::Regex;

/// What a `virtio_blk_handle_read` trace said.
#[derive(Debug, Default, PartialEq, Eq)]
struct Summary {
    requests: u64,
    sectors: u64,
    /// nsectors -> how many requests asked for that many.
    sizes: BTreeMap<u64, u64>,
    /// The first line that parsed, trimmed, so a log names one real request.
    first: Option<String>,
}

/// Summarise `trace` and, when `ceiling` is given, fail the gate if the request
/// count is above it.
pub fn read_hist(trace: &Path, ceiling: Option<u64>) -> io::Result<bool> {
    // The script this replaces opened with `errors="replace"`: a trace is text,
    // but a truncated write is not worth a hard failure.
    let bytes = fs::read(trace)?;
    let summary = summarise(&String::from_utf8_lossy(&bytes));

    println!(
        "requests={} sectors={} bytes={}",
        summary.requests,
        summary.sectors,
        summary.sectors * 512
    );
    println!("nsectors histogram: {}", histogram(&summary.sizes));
    if let Some(first) = &summary.first {
        println!("first: {first}");
    }

    if let Some(ceiling) = ceiling
        && summary.requests > ceiling
    {
        println!(
            "!! {} requests is above the {} ceiling (a per-command read cost regression)",
            summary.requests, ceiling
        );
        return Ok(false);
    }
    Ok(true)
}

fn summarise(text: &str) -> Summary {
    let mut summary = Summary::default();
    for line in text.lines() {
        if !line.contains("virtio_blk_handle_read") {
            continue;
        }
        let Some(captures) = pattern().captures(line) else {
            continue;
        };
        let Ok(nsectors) = captures[1].parse::<u64>() else {
            continue;
        };
        if summary.first.is_none() {
            summary.first = Some(line.trim().to_string());
        }
        summary.requests += 1;
        summary.sectors += nsectors;
        *summary.sizes.entry(nsectors).or_insert(0) += 1;
    }
    summary
}

/// The histogram the old script printed: a Python dict of the sizes in
/// ascending order, for example `{8: 2, 16: 1}`.
fn histogram(sizes: &BTreeMap<u64, u64>) -> String {
    let mut text = String::from("{");
    for (index, (nsectors, count)) in sizes.iter().enumerate() {
        if index > 0 {
            text.push_str(", ");
        }
        write!(text, "{nsectors}: {count}").expect("writing to a String cannot fail");
    }
    text.push('}');
    text
}

fn pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"sector \d+ nsectors (\d+)").expect("the read pattern is a constant")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_is_summarised_by_request_size() {
        let summary = summarise(
            "virtio_blk_handle_read sector 0 nsectors 8\n\
             unrelated line\n\
             virtio_blk_handle_read sector 64 nsectors 16\n\
             virtio_blk_handle_read sector 128 nsectors 8\n",
        );
        assert_eq!(summary.requests, 3);
        assert_eq!(summary.sectors, 32);
        assert_eq!(histogram(&summary.sizes), "{8: 2, 16: 1}");
        assert_eq!(
            summary.first.as_deref(),
            Some("virtio_blk_handle_read sector 0 nsectors 8")
        );
    }

    #[test]
    fn a_marked_line_without_the_fields_is_ignored() {
        let summary = summarise(
            "virtio_blk_handle_read (no fields)\n\
             virtio_blk_handle_read sector x nsectors y\n",
        );
        assert_eq!(summary, Summary::default());
    }

    #[test]
    fn the_first_line_is_trimmed() {
        let summary = summarise("  virtio_blk_handle_read sector 1 nsectors 9  \n");
        assert_eq!(
            summary.first.as_deref(),
            Some("virtio_blk_handle_read sector 1 nsectors 9")
        );
    }

    #[test]
    fn the_ceiling_turns_the_count_into_a_verdict() {
        let dir = std::env::temp_dir().join("minixrs-xtask-read-hist");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let trace = dir.join("read.trace");
        fs::write(
            &trace,
            "virtio_blk_handle_read sector 0 nsectors 8\n\
             virtio_blk_handle_read sector 8 nsectors 8\n",
        )
        .unwrap();

        assert!(read_hist(&trace, Some(2)).unwrap());
        assert!(!read_hist(&trace, Some(1)).unwrap());
        assert!(read_hist(&trace, None).unwrap());

        let _ = fs::remove_dir_all(&dir);
    }
}
