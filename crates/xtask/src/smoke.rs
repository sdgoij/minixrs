//! Drive a booted guest through a `.tsv` smoke scenario.
//!
//! Replaces `tools/smoke/feed.sh`. The command is the arch's QEMU line, whose
//! serial console is stdio: this types the scenario into the shell a user would
//! get, one line at a time, waits for each line's answer, then stops the guest.
//! The console is copied to the log as it arrives, so the caller's own log
//! assertions still work.
//!
//! The scenario format is a contract read by four targets (this driver, the
//! image boot on three arches, and the wasm harness): one step per line, the
//! command, a tab, and the line its output must produce. A line without the tab
//! is an error, because a space would fold the expectation into the command and
//! the step would pass on its own echo.
//!
//! Every wait shares one deadline, as `feed.sh`'s single `waited` counter did:
//! the driver stops a second before the guest's own timeout, so a step that
//! hangs is reported by the driver, which can name it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::thread;
use std::time::{Duration, Instant};

use regex_lite::Regex;

use crate::guest::Guest;

/// `feed.sh` polled every 0.2s and counted polls; the deadline is the same span.
const POLL: Duration = Duration::from_millis(50);
const POLL_MS: u64 = 200;
/// A prompt is a line that is just the shell's `#`.
const PROMPT: &str = "^#[[:space:]]*$";
/// A step that arrives truncated still runs, as a shorter command, so a paced
/// send is retried before the step is failed.
const ATTEMPTS: usize = 4;

pub fn run(root: &Path, log: &Path, seconds: u64, command: &mut Command) -> io::Result<bool> {
    let scenario = scenario_path(root);
    let pace = pace()?;
    // Truncate before anything can fail, so a run never leaves an earlier log.
    fs::write(log, b"")?;
    let steps = match parse_file(&scenario)? {
        Ok(steps) => steps,
        Err((step, detail)) => {
            // The shell detected this after booting; failing before the guest is
            // the same verdict, sooner, and the message is the one it printed.
            eprintln!("!! step {step}: {detail}. Tail of {}:", log.display());
            return Ok(false);
        }
    };

    let mut guest = Guest::spawn_tee(command, log)?;
    let verdict = {
        let mut driver = Driver {
            guest: &mut guest,
            deadline: Instant::now() + budget(seconds),
            pace,
        };
        driver.drive(&steps)
    };
    let transcript = guest.with_transcript(|out| out.to_vec());

    match verdict {
        Verdict::Passed(count) => {
            println!("the smoke scenario ran ({count} steps)");
            Ok(true)
        }
        Verdict::NoPrompt => {
            report(log, "no prompt", "the guest never reached a shell", &transcript);
            Ok(false)
        }
        Verdict::NoSteps => {
            report(
                log,
                "no steps",
                &format!("there are none in {}", scenario.display()),
                &transcript,
            );
            Ok(false)
        }
        Verdict::Failed { step, detail } => {
            report(log, &format!("step {step}"), &detail, &transcript);
            Ok(false)
        }
    }
}

/// The driver's verdict, before it is turned into the caller's message.
enum Verdict {
    Passed(usize),
    NoPrompt,
    NoSteps,
    Failed { step: usize, detail: String },
}

/// One scenario step: the command, and the line its output must produce. An
/// empty expectation means a write, whose echoed command is the whole evidence.
#[derive(Debug)]
struct Step {
    send: String,
    expect: String,
}

/// The deadline `feed.sh` gave every wait: `seconds * 5 - 5` polls of 0.2s.
fn budget(seconds: u64) -> Duration {
    Duration::from_millis(seconds.saturating_mul(5).saturating_sub(5).saturating_mul(POLL_MS))
}

fn scenario_path(root: &Path) -> PathBuf {
    match std::env::var_os("FEED_SCENARIO") {
        Some(value) if !value.is_empty() => root.join(value),
        _ => root.join("tools/smoke/scenario.tsv"),
    }
}

/// `FEED_PACE` is the pause between a step's bytes, or 0 for one write.
fn pace() -> io::Result<Duration> {
    let Some(value) = std::env::var_os("FEED_PACE") else {
        return Ok(Duration::ZERO);
    };
    let value = value.to_string_lossy();
    if value.is_empty() {
        return Ok(Duration::ZERO);
    }
    let seconds: f64 = value.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("FEED_PACE is {value}, not a number of seconds"),
        )
    })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("FEED_PACE is {value}, not a positive number of seconds"),
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}

/// The scenario's steps, or the first malformed line as `(step, detail)`.
///
/// The file's carriage returns are dropped first, as `feed.sh`'s `tr -d '\r'`
/// did, so a scenario written on Windows parses here too.
fn parse_file(path: &Path) -> io::Result<Result<Vec<Step>, (usize, String)>> {
    let text = fs::read_to_string(path)?;
    Ok(parse_text(&text))
}

fn parse_text(text: &str) -> Result<Vec<Step>, (usize, String)> {
    let mut steps = Vec::new();
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((send, expect)) = line.split_once('\t') else {
            return Err((steps.len() + 1, format!("has no tab separator: '{line}'")));
        };
        steps.push(Step {
            send: send.to_string(),
            expect: expect.to_string(),
        });
    }
    Ok(steps)
}

struct Driver<'a> {
    guest: &'a mut Guest,
    deadline: Instant,
    pace: Duration,
}

impl Driver<'_> {
    fn drive(&mut self, steps: &[Step]) -> Verdict {
        if !self.wait_prompt(0) {
            return Verdict::NoPrompt;
        }
        if steps.is_empty() {
            return Verdict::NoSteps;
        }
        for (index, step) in steps.iter().enumerate() {
            if let Err(detail) = self.step(step) {
                return Verdict::Failed {
                    step: index + 1,
                    detail,
                };
            }
        }
        Verdict::Passed(steps.len())
    }

    fn step(&mut self, step: &Step) -> Result<(), String> {
        let at = self.guest.transcript_len();
        if self.pace.is_zero() {
            self.guest
                .send_line(&step.send)
                .map_err(|error| format!("could not write to the console: {error}"))?;
        } else if !self.type_step(step, at) {
            return Err(format!(
                "never arrived whole: sent '{}' a byte at a time and the guest never echoed the whole of it",
                step.send
            ));
        }

        if step.expect.is_empty() {
            // No read-back, so the echoed command is the evidence. It is matched
            // at the end of its line: the prompt in front of the echo was printed
            // before this step's anchor.
            let pattern = format!("{}[[:space:]]*$", step.send);
            let re = compile(&pattern)?;
            if !self.wait_lines(at, &re) {
                return Err(format!("was not accepted: sent '{}'", step.send));
            }
        } else {
            // The whole line, not a substring: the guest echoes what it is sent,
            // and the echoed text contains the expectation for many steps.
            let pattern = format!("^{}[[:space:]]*$", step.expect);
            let re = compile(&pattern)?;
            if !self.wait_lines(at, &re) {
                return Err(format!(
                    "did not answer: sent '{}', expected the line '{}'",
                    step.send, step.expect
                ));
            }
        }
        eprintln!("  ok  {}", step.send);

        // A step that answered is not yet proof the next one will be read: input
        // sent before the shell is back at its prompt is dropped.
        if !self.wait_prompt(at) {
            return Err("answered, but the shell did not come back to a prompt".to_string());
        }
        Ok(())
    }

    /// Type a step a byte at a time and accept it only on its whole echo.
    ///
    /// The anchor is taken once per step, not per attempt, so a retry can still
    /// accept an echo a slow earlier attempt produced. What is anchored per
    /// attempt is the prompt: the shell prints it before the bytes.
    fn type_step(&mut self, step: &Step, anchor: usize) -> bool {
        for _ in 0..ATTEMPTS {
            let sent_at = self.guest.transcript_len();
            if self.send_paced(&step.send).is_err() {
                return false;
            }
            if self.wait_window(anchor, |window| contains(window, step.send.as_bytes())) {
                return true;
            }
            if !self.wait_prompt(sent_at) {
                return false;
            }
        }
        false
    }

    fn send_paced(&mut self, text: &str) -> io::Result<()> {
        let bytes = text.as_bytes();
        for (index, byte) in bytes.iter().enumerate() {
            self.guest.send_raw(&[*byte])?;
            // Four bytes is far below the console's input FIFO, and one pause per
            // group keeps a long scenario to seconds rather than minutes.
            if (index + 1) % 4 == 0 || index + 1 == bytes.len() {
                thread::sleep(self.pace);
            }
        }
        self.guest.send_raw(b"\n")
    }

    fn wait_prompt(&self, from: usize) -> bool {
        self.wait_lines(from, prompt())
    }

    fn wait_lines(&self, from: usize, pattern: &Regex) -> bool {
        self.wait_window(from, |window| line_matches(window, pattern))
    }

    fn wait_window(&self, from: usize, predicate: impl Fn(&[u8]) -> bool) -> bool {
        loop {
            if self.guest.with_transcript(|out| predicate(window(out, from))) {
                return true;
            }
            if Instant::now() >= self.deadline {
                return self
                    .guest
                    .with_transcript(|out| predicate(window(out, from)));
            }
            thread::sleep(POLL);
        }
    }
}

fn prompt() -> &'static Regex {
    static PROMPT_RE: OnceLock<Regex> = OnceLock::new();
    PROMPT_RE.get_or_init(|| Regex::new(PROMPT).expect("the prompt pattern is a constant"))
}

fn compile(pattern: &str) -> Result<Regex, String> {
    Regex::new(pattern).map_err(|error| format!("the pattern '{pattern}' is not valid: {error}"))
}

/// Whether any line of `window` matches `pattern`.
///
/// The line's carriage return is dropped first; `feed.sh`'s `[[:space:]]*`
/// absorbed it, and the patterns here still do.
fn line_matches(window: &[u8], pattern: &Regex) -> bool {
    window
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            std::str::from_utf8(line).ok()
        })
        .any(|line| pattern.is_match(line))
}

fn contains(window: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && window.windows(needle.len()).any(|window| window == needle)
}

fn window(out: &[u8], from: usize) -> &[u8] {
    &out[from.min(out.len())..]
}

fn report(log: &Path, verdict: &str, detail: &str, transcript: &[u8]) {
    eprintln!("!! {verdict}: {detail}. Tail of {}:", log.display());
    let tail = last_lines(transcript, 20);
    if !tail.is_empty() {
        eprintln!("{tail}");
    }
}

fn last_lines(text: &[u8], count: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(count)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_needs_a_tab() {
        let error = parse_text("echo hi\n").expect_err("a step without a tab is an error");
        assert_eq!(error.0, 1);
        assert!(error.1.contains("has no tab separator"));
    }

    #[test]
    fn comments_blank_lines_and_carriage_returns_are_skipped() {
        let steps = parse_text("# a comment\r\n\r\necho hi\thi\r\n").unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].send, "echo hi");
        assert_eq!(steps[0].expect, "hi");
    }

    #[test]
    fn an_empty_expectation_is_a_write_not_a_missing_tab() {
        let steps = parse_text("touch f\t\n").unwrap();
        assert_eq!(steps[0].send, "touch f");
        assert!(steps[0].expect.is_empty());
    }

    #[test]
    fn an_expectation_must_match_the_whole_line() {
        let re = compile("^0 t1.txt[[:space:]]*$").unwrap();
        assert!(line_matches(b"0 t1.txt\r\n", &re));
        assert!(line_matches(b"0 t1.txt", &re));
        assert!(!line_matches(b"# coreutils wc -c t1.txt\r\n", &re));
        assert!(!line_matches(b"0 t1.txt extra\r\n", &re));
    }

    #[test]
    fn a_write_is_accepted_on_the_echoed_command_at_the_end_of_its_line() {
        let re = compile("coreutils touch t1.txt[[:space:]]*$").unwrap();
        assert!(line_matches(b"# coreutils touch t1.txt\r\n", &re));
        assert!(!line_matches(b"# coreutils touch t1.tx\r\n", &re));
    }

    #[test]
    fn a_character_class_expectation_matches_its_line() {
        let re = compile("^[0-9]+[[:space:]]+cpg[[:space:]]*$").unwrap();
        assert!(line_matches(b"123 cpg\r\n", &re));
        assert!(!line_matches(b"x cpg\r\n", &re));
    }

    #[test]
    fn an_anchor_inside_an_expectation_keeps_working() {
        // burst.tsv writes its own anchors; feed.sh wrapped them in another pair.
        let re = compile("^^BURST-1-[0-9]+$[[:space:]]*$").unwrap();
        assert!(line_matches(b"BURST-1-123\r\n", &re));
        assert!(!line_matches(b"# /bin/echo BURST-1-123\r\n", &re));
    }

    #[test]
    fn a_truncated_echo_is_not_the_command() {
        let command = b"/bin/echo gpu3d-ok";
        assert!(contains(b"# /bin/echo gpu3d-ok\r\n", command));
        assert!(!contains(b"# /bin/echo gpu3d\r\n", command));
    }

    #[test]
    fn the_tail_is_the_last_lines() {
        assert_eq!(last_lines(b"a\nb\nc\n", 2), "b\nc");
        assert_eq!(last_lines(b"", 20), "");
    }

    #[test]
    fn every_repository_scenario_parses() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/smoke");
        let mut seen = 0;
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("tsv") {
                continue;
            }
            let steps = parse_file(&path)
                .unwrap()
                .unwrap_or_else(|(step, detail)| panic!("{} step {step}: {detail}", path.display()));
            assert!(!steps.is_empty(), "{} has no steps", path.display());
            seen += 1;
        }
        assert!(seen >= 40, "only {seen} scenarios found in {}", dir.display());
    }
}
