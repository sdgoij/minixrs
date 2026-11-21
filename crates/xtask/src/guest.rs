//! A QEMU guest and its serial console.
//!
//! The probes this replaces each spawned QEMU themselves, merged stderr into the
//! transcript, and polled a shared byte buffer. This is that, with the child
//! killed on drop so an error path cannot leave an emulator behind.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

pub struct Guest {
    child: Child,
    stdin: Option<ChildStdin>,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Guest {
    /// Spawn `command`, which has to write the guest's serial console to stdout.
    pub fn spawn(command: &mut Command) -> io::Result<Guest> {
        spawn_inner(command, None)
    }

    /// Spawn `command` and copy the console to `log` as it arrives.
    pub fn spawn_tee(command: &mut Command, log: &Path) -> io::Result<Guest> {
        spawn_inner(command, Some(log))
    }

    /// Wait until `wanted` appears in the console, or `timeout` passes.
    pub fn wait_for(&self, wanted: &[u8], timeout: Duration) -> bool {
        self.wait_until(
            |out| out.windows(wanted.len()).any(|window| window == wanted),
            timeout,
        )
    }

    /// Wait until `predicate` accepts the console so far, or `timeout` passes.
    pub fn wait_until(&self, predicate: impl Fn(&[u8]) -> bool, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate(&lock(&self.out)) {
                return true;
            }
            if Instant::now() >= deadline {
                return predicate(&lock(&self.out));
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until `wanted` appears at or after `start`, or `timeout` passes.
    ///
    /// The anchor matters: a prompt from before a command was typed is still in
    /// the console, so searching the whole of it finds a prompt that says
    /// nothing about whether the shell has come back.
    pub fn wait_for_after(&self, wanted: &[u8], start: usize, timeout: Duration) -> bool {
        self.wait_until_after(
            start,
            |out| out.windows(wanted.len()).any(|window| window == wanted),
            timeout,
        )
    }

    /// Wait until `predicate` accepts the console from `start` on, or `timeout`
    /// passes.
    pub fn wait_until_after(
        &self,
        start: usize,
        predicate: impl Fn(&[u8]) -> bool,
        timeout: Duration,
    ) -> bool {
        self.wait_until(|out| predicate(from(out, start)), timeout)
    }

    /// The console's length, for anchoring a later [`Self::wait_for_after`].
    pub fn transcript_len(&self) -> usize {
        lock(&self.out).len()
    }

    /// How many times `needle` occurs in the console so far.
    pub fn count(&self, needle: &[u8]) -> usize {
        let out = lock(&self.out);
        out.windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }

    /// The byte offset of the console's last `needle`, if it has one.
    pub fn rfind(&self, needle: &[u8]) -> Option<usize> {
        let out = lock(&self.out);
        out.windows(needle.len())
            .rposition(|window| window == needle)
    }

    /// Write `data` to the guest's console in one write.
    pub fn send_raw(&mut self, data: &[u8]) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("the guest has no stdin"))?;
        stdin.write_all(data)?;
        stdin.flush()
    }

    /// Write `text` and a newline to the guest's console in one write.
    pub fn send_line(&mut self, text: &str) -> io::Result<()> {
        let mut line = Vec::with_capacity(text.len() + 1);
        line.extend_from_slice(text.as_bytes());
        line.push(b'\n');
        self.send_raw(&line)
    }

    /// Write `text` to the guest's console a byte at a time.
    ///
    /// A GL display drops serial bytes written in one burst, so the probes that
    /// type into such a guest pace every byte, as the smoke driver's `FEED_PACE` does.
    pub fn send_paced(&mut self, text: &str, per_byte: Duration) -> io::Result<()> {
        for byte in text.bytes().chain(std::iter::once(b'\n')) {
            let stdin = self
                .stdin
                .as_mut()
                .ok_or_else(|| io::Error::other("the guest has no stdin"))?;
            stdin.write_all(&[byte])?;
            stdin.flush()?;
            thread::sleep(per_byte);
        }
        Ok(())
    }

    /// The console so far, read under the same lock the readers use.
    pub fn with_transcript<T>(&self, read: impl FnOnce(&[u8]) -> T) -> T {
        let out = lock(&self.out);
        read(&out)
    }

    /// The last `count` bytes of the console, for a failure message.
    pub fn tail(&self, count: usize) -> String {
        self.with_transcript(|out| {
            let start = out.len().saturating_sub(count);
            String::from_utf8_lossy(&out[start..]).into_owned()
        })
    }
}

impl Drop for Guest {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                eprintln!("warning: could not poll qemu: {error}");
                return;
            }
        }
        if let Err(error) = self.child.kill() {
            eprintln!("warning: could not kill qemu: {error}");
        }
        if let Err(error) = self.child.wait() {
            eprintln!("warning: could not reap qemu: {error}");
        }
    }
}

/// The console from `start` on, clamped so an anchor past the end is empty
/// rather than a panic.
fn from(out: &[u8], start: usize) -> &[u8] {
    &out[start.min(out.len())..]
}

/// The console lock, taken even if a reader thread panicked while holding it: a
/// missing line of console output is not worth a second panic.
fn lock(out: &Mutex<Vec<u8>>) -> MutexGuard<'_, Vec<u8>> {
    out.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Spawn a guest, optionally copying its console to a log file as it arrives.
fn spawn_inner(command: &mut Command, log: Option<&Path>) -> io::Result<Guest> {
    // The log is created before the child, so a bad path is named without leaving
    // an emulator behind.
    let log = match log {
        Some(path) => Some(Arc::new(Mutex::new(File::create(path)?))),
        None => None,
    };
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "cannot run {}: {error} (is QEMU installed?)",
                    command.get_program().to_string_lossy()
                ),
            )
        })?;

    let out = Arc::new(Mutex::new(Vec::new()));
    if let Some(stdout) = child.stdout.take() {
        pump(stdout, Arc::clone(&out), log.clone());
    }
    if let Some(stderr) = child.stderr.take() {
        pump(stderr, Arc::clone(&out), log);
    }
    let stdin = child.stdin.take();
    Ok(Guest { child, stdin, out })
}

fn pump(
    mut reader: impl Read + Send + 'static,
    out: Arc<Mutex<Vec<u8>>>,
    log: Option<Arc<Mutex<File>>>,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    lock(&out).extend_from_slice(&buffer[..read]);
                    if let Some(log) = &log
                        && let Ok(mut file) = log.lock()
                    {
                        // The transcript above is the verdict; the file is for the
                        // caller's own assertions, so a write error is not fatal.
                        let _ = file.write_all(&buffer[..read]);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paced_send_reaches_a_real_child_and_the_console_is_read_back() {
        // `cat` echoes what it is given, which is the same pipe arrangement the
        // probes use: bytes in, transcript out, no QEMU needed.
        let mut command = Command::new("cat");
        let mut guest = Guest::spawn(&mut command).unwrap();
        guest
            .send_paced("hello", Duration::from_millis(1))
            .unwrap();
        assert!(guest.wait_for(b"hello", Duration::from_secs(5)));
        assert!(guest.tail(64).contains("hello"));
    }

    #[test]
    fn a_line_that_never_arrives_times_out() {
        let mut command = Command::new("cat");
        let guest = Guest::spawn(&mut command).unwrap();
        assert!(!guest.wait_for(b"never printed", Duration::from_millis(200)));
    }

    #[test]
    fn an_anchored_wait_ignores_what_was_already_there() {
        let mut command = Command::new("cat");
        let mut guest = Guest::spawn(&mut command).unwrap();
        guest
            .send_paced("first", Duration::from_millis(1))
            .unwrap();
        assert!(guest.wait_for(b"first", Duration::from_secs(5)));
        let anchor = guest.transcript_len();
        assert!(!guest.wait_for_after(b"first", anchor, Duration::from_millis(200)));
        guest
            .send_paced("second", Duration::from_millis(1))
            .unwrap();
        assert!(guest.wait_for_after(b"second", anchor, Duration::from_secs(5)));
        assert_eq!(guest.count(b"first"), 1);
        assert!(guest.rfind(b"second").unwrap() >= anchor);
    }
}
