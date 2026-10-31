//! The XDG base directories, read from the environment.
//!
//! The published `dirs` crate asks the platform (`libc` on Unix); on the port
//! the environment is the source that matters, and the XDG defaults below are
//! spelled out rather than looked up in a passwd entry.

use std::env;
use std::path::PathBuf;

/// An environment directory, or `None` when it is unset or relative.
///
/// The XDG spec says a relative path in one of these variables must be
/// ignored, so the caller falls back to the default location.
fn env_dir(name: &str) -> Option<PathBuf> {
    let value = env::var_os(name)?;
    let path = PathBuf::from(value);
    path.is_absolute().then_some(path)
}

/// The user's home directory, from `HOME`.
pub fn home_dir() -> Option<PathBuf> {
    env_dir("HOME")
}

/// The configuration directory: `XDG_CONFIG_HOME`, or `~/.config`.
pub fn config_dir() -> Option<PathBuf> {
    env_dir("XDG_CONFIG_HOME").or_else(|| home_dir().map(|home| home.join(".config")))
}

/// The data directory: `XDG_DATA_HOME`, or `~/.local/share`.
pub fn data_dir() -> Option<PathBuf> {
    env_dir("XDG_DATA_HOME").or_else(|| home_dir().map(|home| home.join(".local").join("share")))
}

/// The cache directory: `XDG_CACHE_HOME`, or `~/.cache`.
pub fn cache_dir() -> Option<PathBuf> {
    env_dir("XDG_CACHE_HOME").or_else(|| home_dir().map(|home| home.join(".cache")))
}

/// The state directory: `XDG_STATE_HOME`, or `~/.local/state`.
pub fn state_dir() -> Option<PathBuf> {
    env_dir("XDG_STATE_HOME").or_else(|| home_dir().map(|home| home.join(".local").join("state")))
}
