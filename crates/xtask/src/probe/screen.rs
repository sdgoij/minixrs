//! Screen dumps from QEMU, and pixels out of them.
//!
//! QEMU answers `screendump` before the PPM on disk is complete, so a fixed
//! sleep is a guess a loaded host loses and the read then sees a partial file.
//! The header carries the size the file will be, so every read waits on that
//! number instead.

use std::fs;
use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use crate::qmp::{Qmp, json_string};

/// One frame, as the PPM carries it: eight bits per channel, no padding.
pub struct Screen {
    pub width: usize,
    pub height: usize,
    pixels: Vec<u8>,
}

impl Screen {
    /// The RGB triple at `(x, y)`, or `None` when the point is off the frame.
    pub fn pixel(&self, x: usize, y: usize) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (y * self.width + x) * 3;
        Some([
            *self.pixels.get(offset)?,
            *self.pixels.get(offset + 1)?,
            *self.pixels.get(offset + 2)?,
        ])
    }
}

/// Ask QEMU for one frame and read it back once it is complete.
pub fn screendump(qmp: &mut Qmp, path: &Path) -> io::Result<Screen> {
    // A complete file from an earlier dump must not answer for this one: QEMU
    // overwrites the path in place, so a read that beats the overwrite would
    // report the previous frame.
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let name = path.to_string_lossy();
    qmp.execute(&format!(
        r#"{{"execute":"screendump","arguments":{{"filename":{}}}}}"#,
        json_string(&name)
    ))?;
    read_ppm(path, Duration::from_secs(15))
}

/// A P6 PPM once its pixel data has reached the size the header promises.
fn read_ppm(path: &Path, timeout: Duration) -> io::Result<Screen> {
    let deadline = Instant::now() + timeout;
    loop {
        let data = fs::read(path).unwrap_or_default();
        if let Some(screen) = parse_ppm(&data) {
            return Ok(screen);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{} is not a complete P6 PPM", path.display()),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// The frame in `data`, or `None` while the file is still short.
fn parse_ppm(data: &[u8]) -> Option<Screen> {
    let mut parts = data.splitn(4, |byte| *byte == b'\n');
    if parts.next()? != b"P6" {
        return None;
    }
    let mut dims = parts.next()?.split(|byte| byte.is_ascii_whitespace());
    let width = std::str::from_utf8(dims.next()?).ok()?.parse().ok()?;
    let height = std::str::from_utf8(dims.next()?).ok()?.parse().ok()?;
    // The maximum value line, which is 255 for every dump QEMU writes here.
    parts.next()?;
    let pixels = parts.next()?;
    if pixels.len() < width * height * 3 {
        return None;
    }
    Some(Screen {
        width,
        height,
        pixels: pixels[..width * height * 3].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_complete_ppm_carries_its_pixels() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[255, 0, 0, 0, 255, 0]);
        let screen = parse_ppm(&data).expect("a complete frame");
        assert_eq!(screen.pixel(0, 0), Some([255, 0, 0]));
        assert_eq!(screen.pixel(1, 0), Some([0, 255, 0]));
        assert_eq!(screen.pixel(2, 0), None);
        assert_eq!(screen.pixel(1, 1), None);
    }

    #[test]
    fn a_short_ppm_is_not_a_frame_yet() {
        let mut data = b"P6\n2 1\n255\n".to_vec();
        data.extend_from_slice(&[255, 0, 0]);
        assert!(parse_ppm(&data).is_none());
    }

    #[test]
    fn another_format_is_rejected() {
        assert!(parse_ppm(b"P3\n2 1\n255\n0 0 0 0 0 0").is_none());
    }
}
