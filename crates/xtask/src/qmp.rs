//! A minimal QMP client, for the probes that have to inject input.
//!
//! QMP is a JSON line protocol over a TCP socket: the server greets on connect,
//! then answers each command with `{"return": ...}` or `{"error": ...}` and
//! interleaves asynchronous events. Every command here is a fixed string, so the
//! reply only has to be classified, not parsed into a typed value, and the crate
//! needs no JSON dependency.

use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct Qmp {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
}

impl Qmp {
    pub fn connect(port: u16) -> io::Result<Qmp> {
        let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("cannot reach QMP on 127.0.0.1:{port}: {error}"),
            )
        })?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let reader = BufReader::new(stream.try_clone()?);
        let mut qmp = Qmp { stream, reader };
        // Consumes the greeting and switches the server to command mode.
        qmp.execute(r#"{"execute":"qmp_capabilities"}"#)?;
        Ok(qmp)
    }

    /// Send one JSON command and wait for its reply, skipping events.
    pub fn execute(&mut self, command: &str) -> io::Result<()> {
        self.stream.write_all(command.as_bytes())?;
        self.stream.write_all(b"\n")?;
        self.stream.flush()?;

        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "QMP closed the connection",
                ));
            }
            let reply = line.trim();
            if reply.is_empty() {
                continue;
            }
            if reply.contains("\"error\"") {
                return Err(io::Error::other(format!("QMP refused {command}: {reply}")));
            }
            if reply.contains("\"return\"") {
                return Ok(());
            }
            // The greeting and asynchronous events carry neither key.
        }
    }

    /// Press or release `qcode`, an evdev-style key name QEMU knows.
    pub fn key(&mut self, qcode: &str, down: bool) -> io::Result<()> {
        self.execute(&key_command(qcode, down))
    }
}

fn key_command(qcode: &str, down: bool) -> String {
    format!(
        r#"{{"execute":"input-send-event","arguments":{{"events":[{{"type":"key","data":{{"key":{{"type":"qcode","data":"{qcode}"}},"down":{down}}}}}]}}}}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_event_is_one_qcode_press_or_release() {
        assert_eq!(
            key_command("a", true),
            r#"{"execute":"input-send-event","arguments":{"events":[{"type":"key","data":{"key":{"type":"qcode","data":"a"},"down":true}}]}}"#
        );
        assert!(key_command("a", false).contains(r#""down":false"#));
    }
}
