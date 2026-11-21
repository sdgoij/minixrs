//! A QEMU guest and its serial console.
//!
//! The probes this replaces each spawned QEMU themselves, merged stderr into the
//! transcript, and polled a shared byte buffer. This is that, with the child
//! killed on drop so an error path cannot leave an emulator behind.

use std::io::{self, Read, Write};
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
            pump(stdout, Arc::clone(&out));
        }
        if let Some(stderr) = child.stderr.take() {
            pump(stderr, Arc::clone(&out));
        }
        let stdin = child.stdin.take();
        Ok(Guest { child, stdin, out })
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

    /// Write `text` to the guest's console a byte at a time.
    ///
    /// A GL display drops serial bytes written in one burst, so the probes that
    /// type into such a guest pace every byte, as `feed.sh`'s `FEED_PACE` does.
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

/// The console lock, taken even if a reader thread panicked while holding it: a
/// missing line of console output is not worth a second panic.
fn lock(out: &Mutex<Vec<u8>>) -> MutexGuard<'_, Vec<u8>> {
    out.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn pump(mut reader: impl Read + Send + 'static, out: Arc<Mutex<Vec<u8>>>) {
    thread::spawn(move || {
        let mut buffer = [0u8; 65536];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => lock(&out).extend_from_slice(&buffer[..read]),
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
}
