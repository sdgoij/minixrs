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
    run_with(log, seconds, &scenario, pace, command)
}

/// Drive `command` through `scenario` with the pacing the caller chose.
///
/// This is the shape a gate uses: the scenario and its pace are part of the
/// gate's definition, not of the process environment.
pub fn run_with(
    log: &Path,
    seconds: u64,
    scenario: &Path,
    pace: Duration,
    command: &mut Command,
) -> io::Result<bool> {
    // Truncate before anything can fail, so a run never leaves an earlier log.
    fs::write(log, b"")?;
    let steps = match parse_file(scenario)? {
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

/// What a poll of the transcript says about the step in flight.
enum Answer {
    Matched,
    Finished,
    Waiting,
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
        self.ensure_submitted(at, &step.send);

        if step.expect.is_empty() {
            // No read-back, so the echoed command is the evidence. It is matched
            // at the end of its line: the prompt in front of the echo was printed
            // before this step's anchor.
            let pattern = format!("{}[[:space:]]*$", regex_lite::escape(&step.send));
            let re = compile(&pattern)?;
            if !self.wait_answer(at, &re, &step.send, false) {
                return Err(format!("was not accepted: sent '{}'", step.send));
            }
        } else {
            // The whole line, not a substring: the guest echoes what it is sent,
            // and the echoed text contains the expectation for many steps.
            let pattern = format!("^{}[[:space:]]*$", step.expect);
            let re = compile(&pattern)?;
            if !self.wait_answer(at, &re, &step.send, true) {
                return Err(format!(
                    "did not answer: sent '{}', expected the line '{}'",
                    step.send, step.expect
                ));
            }
        }
        eprintln!("  ok  {}", step.send);

        // A step that answered is not yet proof the next one will be read: input
        // sent before the shell is back at its prompt is dropped. The prompt has
        // to be after this command's echo: the prompt in front of it was printed
        // before the step and accepting it lets the next step race the command.
        if !self.wait_prompt_after(at, &step.send) {
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

    /// Make sure the shell received the line's terminator.
    ///
    /// The console drops input while it is still printing boot messages, and the
    /// byte most likely to go is the newline: the echo is complete, the line looks
    /// sent, and the shell is still editing it, so the step waits out the whole
    /// deadline with nothing to show. A submitted line has a newline after its
    /// echo, so resend the terminator for one that does not.
    fn ensure_submitted(&mut self, anchor: usize, sent: &str) {
        // Wait for the echo before touching the terminator. A newline sent
        // while the shell is still printing the line is an empty command, and
        // four of them can be consumed before the echo even arrives.
        let echoed_by = Instant::now() + Duration::from_secs(2);
        while Instant::now() < echoed_by {
            if self
                .guest
                .with_transcript(|out| contains(window(out, anchor), sent.as_bytes()))
            {
                break;
            }
            thread::sleep(POLL);
        }
        for _ in 0..ATTEMPTS {
            let submitted = self
                .guest
                .with_transcript(|out| line_submitted(window(out, anchor), sent.as_bytes()));
            if submitted {
                return;
            }
            let _ = self.guest.send_raw(b"\n");
            thread::sleep(self.pace.max(Duration::from_millis(30)));
        }
    }

    /// Wait for `pattern`, but stop when the shell's prompt comes back first.
    ///
    /// A command's output precedes the prompt, so a prompt with no matching line
    /// means the command finished without producing what the step asked for. The
    /// step used to wait out the whole scenario budget in that case, which made a
    /// one-line diagnostic look like a two-hour hang.
    fn wait_answer(&self, from: usize, pattern: &Regex, sent: &str, after_echo: bool) -> bool {
        loop {
            let answer = self.guest.with_transcript(|out| {
                let window = window(out, from);
                let answered = if after_echo {
                    line_matches(output_window(out, from, sent), pattern)
                } else {
                    line_matches(window, pattern)
                };
                if answered {
                    Answer::Matched
                } else if prompt_after(window, sent.as_bytes()) {
                    Answer::Finished
                } else {
                    Answer::Waiting
                }
            });
            match answer {
                Answer::Matched => return true,
                Answer::Finished => return false,
                Answer::Waiting => {}
            }
            if Instant::now() >= self.deadline {
                return self
                    .guest
                    .with_transcript(|out| line_matches(window(out, from), pattern));
            }
            thread::sleep(POLL);
        }
    }

    fn wait_prompt(&self, from: usize) -> bool {
        self.wait_lines(from, prompt())
    }

    /// Wait for the prompt that follows this step's command echo.
    fn wait_prompt_after(&self, from: usize, sent: &str) -> bool {
        loop {
            if self
                .guest
                .with_transcript(|out| prompt_after(window(out, from), sent.as_bytes()))
            {
                return true;
            }
            if Instant::now() >= self.deadline {
                return self
                    .guest
                    .with_transcript(|out| prompt_after(window(out, from), sent.as_bytes()));
            }
            thread::sleep(POLL);
        }
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
/// Every trailing carriage return is dropped first. One is the console's own; a
/// guest whose tty and serial driver both translate a newline (riscv64 does) emits
/// two, and a pattern that ends in `$` is compared against the line's last byte,
/// so a second `\r` left in place fails it even though the line is right. The
/// driver appends `[[:space:]]*$`, which is exactly what `feed.sh` relied on and
/// what a `$` inside the expectation itself makes unusable.
fn line_matches(window: &[u8], pattern: &Regex) -> bool {
    window
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let mut line = line;
            while let Some(stripped) = line.strip_suffix(b"\r") {
                line = stripped;
            }
            std::str::from_utf8(line).ok()
        })
        .any(|line| pattern.is_match(line))
}

fn contains(window: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && window.windows(needle.len()).any(|window| window == needle)
}

/// Whether the last `sent` in `window` is followed by a newline, i.e. the shell
/// received the terminator. A retry can echo the line more than once, so the last
/// occurrence decides.
fn line_submitted(window: &[u8], sent: &[u8]) -> bool {
    if sent.is_empty() {
        return false;
    }
    match window.windows(sent.len()).rposition(|window| window == sent) {
        Some(at) => window[at + sent.len()..].contains(&b'\n'),
        None => false,
    }
}

/// Whether the shell's prompt appears after the last echo of `sent`.
///
/// The prompt is printed after the command's output, so it marks the command as
/// finished even when no line matched the step's expectation. A `#` line before
/// the echo is the previous prompt and is not part of the answer. Later output
/// (a server's start-up line, for example) does not undo a prompt that already
/// appeared, so this is any matching line, not the last one.
fn prompt_after(window: &[u8], sent: &[u8]) -> bool {
    if sent.is_empty() {
        return false;
    }
    let Some(at) = window.windows(sent.len()).rposition(|window| window == sent) else {
        return false;
    };
    window[at + sent.len()..]
        .split(|&byte| byte == b'\n')
        .any(|line| line_matches(line, prompt()))
}

fn window(out: &[u8], from: usize) -> &[u8] {
    &out[from.min(out.len())..]
}

/// What the command printed, after its echo.
///
/// Anchoring the expectation at the echo keeps the previous prompt out of the
/// window: the shell can still be writing that prompt's terminator when the
/// step starts, and a blank line from it satisfies a `^$` expectation before
/// the command has run.
fn output_window<'a>(out: &'a [u8], from: usize, sent: &str) -> &'a [u8] {
    let rest = window(out, from);
    match rest
        .windows(sent.len())
        .rposition(|window| window == sent.as_bytes())
    {
        Some(at) => &rest[at + sent.len()..],
        None => rest,
    }
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
    fn a_line_is_submitted_only_after_its_newline() {
        assert!(!line_submitted(b"# echo hi", b"echo hi"));
        assert!(line_submitted(b"# echo hi\r\n", b"echo hi"));
        assert!(line_submitted(b"# echo hi\r\n# echo hi\r\n", b"echo hi"));
        assert!(
            !line_submitted(b"# echo hi\r\n# echo hi", b"echo hi"),
            "a retry still being edited is not submitted"
        );
        assert!(!line_submitted(b"# other", b"echo hi"));
        assert!(!line_submitted(b"anything", b""), "an empty send is never submitted");
    }

    #[test]
    fn a_write_ignores_regex_syntax_in_the_command() {
        let re = compile(&format!(
            "{}[[:space:]]*$",
            regex_lite::escape("clang++ -c x.cpp")
        ))
        .expect("the escaped echo is a valid pattern");
        assert!(line_matches(b"# clang++ -c x.cpp\r\n", &re));
    }

    #[test]
    fn an_expectation_is_read_after_the_echo() {
        let transcript = b"#\r\n# cmd\r\noutput\r\n# ";
        assert_eq!(output_window(transcript, 0, "cmd"), b"\r\noutput\r\n# ");
        assert_eq!(
            output_window(b"# cmd\r\n", 0, "missing"),
            b"# cmd\r\n",
            "a command that was not echoed keeps the whole window"
        );
    }

    #[test]
    fn a_prompt_after_the_echo_finishes_the_step() {
        assert!(prompt_after(b"# echo hi\r\noutput\r\n# ", b"echo hi"));
        assert!(prompt_after(b"# echo hi\r\n#", b"echo hi"));
        assert!(!prompt_after(b"# echo hi\r\noutput\r\n", b"echo hi"));
        assert!(
            !prompt_after(b"# \r\n# echo hi\r\noutput\r\n", b"echo hi"),
            "the prompt before the echo is not the answer"
        );
        assert!(
            prompt_after(b"# echo hi\r\n# \r\nlater output\r\n", b"echo hi"),
            "output after the prompt does not take the prompt back"
        );
        assert!(!prompt_after(b"# echo hi", b"echo hi"));
    }

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
    fn a_doubled_carriage_return_does_not_defeat_an_anchored_expectation() {
        // riscv64's tty and serial driver both translate the newline, so a line
        // arrives as `...\r\r\n`. The expectation ends in `$` itself, which the
        // driver cannot rescue with its trailing `[[:space:]]*$`.
        let re = compile("^BURST-1-[0-9]+$[[:space:]]*$").unwrap();
        assert!(line_matches(b"BURST-1-42\r\r\n", &re));
        assert!(line_matches(b"BURST-1-42\r\n", &re));
        assert!(!line_matches(b"BURST-1-4x\r\r\n", &re));
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
