//! A minimal QMP client, for the probes that have to inject input.
//!
//! QMP is a JSON line protocol over a TCP socket: the server greets on connect,
//! then answers each command with `{"return": ...}` or `{"error": ...}` and
//! interleaves asynchronous events. Every command here is a fixed string, so the
//! reply only has to be classified, not parsed into a typed value, and the crate
//! needs no JSON dependency.

use std::io::{self, BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
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

    /// Press and release `qcode`, the way a keyboard sends a key.
    pub fn press(&mut self, qcode: &str) -> io::Result<()> {
        self.key(qcode, true)?;
        std::thread::sleep(Duration::from_millis(200));
        self.key(qcode, false)
    }

    /// Move the absolute pointer to a pixel of a `size`-wide axis.
    ///
    /// The display device has to be named: without it QEMU answers "Input
    /// handler not found for event type abs", and the bochs display is what
    /// routes the axis events to the tablet.
    pub fn absolute(&mut self, device: &str, px: u32, py: u32) -> io::Result<()> {
        for (axis, value, size) in [("x", px, 1024), ("y", py, 768)] {
            self.execute(&absolute_command(device, axis, value, size))?;
        }
        Ok(())
    }


    /// Write a range of guest physical memory to `path`.
    ///
    /// `pmemsave` is the only way to read a guest's memory from the host, and it
    /// answers before returning, so the file is complete when this does.
    pub fn pmemsave(&mut self, phys: u64, size: u64, path: &Path) -> io::Result<()> {
        let name = path.to_string_lossy();
        self.execute(&format!(
            r#"{{"execute":"pmemsave","arguments":{{"val":{phys},"size":{size},"filename":{}}}}}"#,
            json_string(&name)
        ))
    }
}

/// `text` as a JSON string, escaping the backslashes a Windows path carries.
pub(crate) fn json_string(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len() + 2);
    escaped.push('"');
    for ch in text.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", control as u32))
            }
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

fn key_command(qcode: &str, down: bool) -> String {
    format!(
        r#"{{"execute":"input-send-event","arguments":{{"events":[{{"type":"key","data":{{"key":{{"type":"qcode","data":"{qcode}"}},"down":{down}}}}}]}}}}"#
    )
}

fn absolute_command(device: &str, axis: &str, value: u32, size: u32) -> String {
    // The wire wants a fraction of the axis, 0x8000 being its length.
    let scaled = value * 0x8000 / size;
    format!(
        r#"{{"execute":"input-send-event","arguments":{{"device":"{device}","events":[{{"type":"abs","data":{{"axis":"{axis}","value":{scaled}}}}}]}}}}"#
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

    #[test]
    fn an_absolute_move_is_scaled_to_the_axis_and_names_the_device() {
        let command = absolute_command("fb0", "x", 512, 1024);
        assert!(command.contains(r#""device":"fb0""#));
        assert!(command.contains(r#""axis":"x""#));
        assert!(command.contains(r#""value":16384"#));
    }

    #[test]
    fn a_windows_path_survives_the_json_round_trip() {
        assert_eq!(
            json_string(r"C:\Users\T\target\dump.bin"),
            r#""C:\\Users\\T\\target\\dump.bin""#
        );
    }
}
