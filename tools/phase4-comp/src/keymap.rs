//! The xkb data smithay needs, materialised from the keymap artifact the tree already ships.
//!
//! smithay's seat builds its keyboard with `xkbcommon`'s *rules* compiler: `XkbConfig` names a
//! layout and xkbcommon reads `$XKB_CONFIG_ROOT/rules/<rules>` and the component files it names.
//! This image has no `/usr/share/X11/xkb` (the in-house server serves a *compiled* keymap text,
//! `crates/servers/data/us-evdev.xkb`, instead — §6.12), so the same artifact is split back into
//! the four component files here and written under a temporary root. The result is that both
//! servers serve the **same** keymap, one compiled and one as components, from one source of
//! truth, and smithay and xkbcommon stay unmodified.
//!
//! This runs once, before any thread exists and before the first `xkb_context` is created.

/// The compiled keymap `wlserver` serves to its clients.
const KEYMAP: &str = include_str!("../../../crates/servers/data/us-evdev.xkb");

/// `MinixErr` carries a *positive* errno, the constant in `minix-std` is the negative one.
const EEXIST: i32 = -minix_std::EEXIST;

/// The root xkbcommon reads. `/tmp` is the image's only writable path outside the image binary
/// zones, and the tree is rewritten on every start.
const ROOT: &str = "/tmp/xkb";

/// A rules file whose only job is to name the split components: one model (`*`), one layout
/// (`us`), the four component kinds. Validated against xkbcommon on the host before it was
/// written here (`keycode 38` resolves to `a`).
const RULES: &str = "\
! model = keycodes
  * = evdev
! model = types
  * = complete
! model = compat
  * = complete
! layout = symbols
  * = us
";

/// Writes the component tree and points `XKB_CONFIG_ROOT` at it.
pub fn materialize() -> Result<(), String> {
    let keycodes = block(KEYMAP, "keycodes")?;
    let types = block(KEYMAP, "types")?;
    let compatibility = block(KEYMAP, "compatibility")?;
    let symbols = block(KEYMAP, "symbols")?;

    for path in [
        "/tmp".to_string(),
        ROOT.to_string(),
        format!("{ROOT}/rules"),
        format!("{ROOT}/keycodes"),
        format!("{ROOT}/types"),
        format!("{ROOT}/compat"),
        format!("{ROOT}/symbols"),
    ] {
        match minix_std::fs::mkdir(path.as_bytes(), 0o755) {
            Ok(()) => {}
            Err(e) if e.0 == EEXIST => {}
            Err(e) => return Err(format!("mkdir {path}: errno {}", e.0)),
        }
    }

    write(&format!("{ROOT}/rules/evdev"), RULES)?;
    write(&format!("{ROOT}/keycodes/evdev"), keycodes)?;
    write(&format!("{ROOT}/types/complete"), types)?;
    write(&format!("{ROOT}/compat/complete"), compatibility)?;
    write(&format!("{ROOT}/symbols/us"), symbols)?;

    // `std::env::set_var` on this port updates only Rust's snapshot of the environment
    // (`library/std/src/sys/pal/minix/env.rs`), and xkbcommon is C: it reads `environ` through
    // `getenv`. So the variable is set through libc, on the same table xkbcommon sees.
    let name = c"XKB_CONFIG_ROOT";
    let value =
        std::ffi::CString::new(ROOT).map_err(|_| "the xkb root contains a NUL".to_string())?;
    // SAFETY: both are NUL-terminated, and libc's `setenv` copies them.
    if unsafe { libc::setenv(name.as_ptr(), value.as_ptr(), 1) } != 0 {
        return Err("setenv XKB_CONFIG_ROOT failed".to_string());
    }
    Ok(())
}

/// Writes `data` to `path`, creating or truncating it.
fn write(path: &str, data: &str) -> Result<(), String> {
    let mode = minix_std::fs::O_WRONLY | minix_std::fs::O_CREAT | minix_std::fs::O_TRUNC;
    // SAFETY: a fixed path this function owns, and a regular file.
    let fd = unsafe { minix_std::fs::open(path.as_bytes(), mode, 0o644) }
        .map_err(|e| format!("open {path}: errno {}", e.0))?;
    let bytes = data.as_bytes();
    let mut written = 0usize;
    while written < bytes.len() {
        // SAFETY: the descriptor is open for writing and the slice is a prefix of the file body.
        match unsafe { minix_std::fs::write(fd, &bytes[written..]) } {
            Ok(0) => {
                let _ = minix_std::fs::close(fd);
                return Err(format!("write {path}: no progress at {written}"));
            }
            Ok(n) => written += n as usize,
            Err(e) => {
                let _ = minix_std::fs::close(fd);
                return Err(format!("write {path}: errno {}", e.0));
            }
        }
    }
    minix_std::fs::close(fd).map_err(|e| format!("close {path}: errno {}", e.0))
}

/// Returns the `xkb_<kind>` section of the compiled keymap, braces and terminator included.
///
/// The text is `xkbcomp` output, so the sections are top-level blocks and a brace counter is
/// enough; they contain no braces inside strings or comments.
fn block<'a>(text: &'a str, kind: &str) -> Result<&'a str, String> {
    let header = format!("xkb_{kind} ");
    let start = text
        .find(&header)
        .ok_or_else(|| format!("the keymap has no {header} section"))?;
    let mut depth = 0i32;
    let mut opened = false;
    for (offset, ch) in text[start..].char_indices() {
        match ch {
            '{' => {
                depth += 1;
                opened = true;
            }
            '}' => {
                depth -= 1;
                if opened && depth == 0 {
                    let after = &text[start + offset..];
                    let semi = after
                        .find(';')
                        .ok_or_else(|| format!("{header} is not terminated by `;`"))?;
                    return Ok(&text[start..start + offset + semi + 1]);
                }
            }
            _ => {}
        }
    }
    Err(format!("{header} is unterminated"))
}
