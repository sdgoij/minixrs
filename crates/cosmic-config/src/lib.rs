//! The cosmic settings store, over one RON document per application version.
//!
//! The published crate writes one file per key. This port keeps the same API
//! and stores the whole key set as one document,
//! `<config>/cosmic/<name>/v<n>/config.ron`, whose map values are the RON text
//! of each key. A transaction is then published by a single atomic rename,
//! which is what the caller asked for; the trade-off is that a hand-edited
//! document is a map rather than a directory of files.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

// Lets the derive in this crate's own tests spell its output the way a consumer
// does: `cosmic_config::ConfigGet`.
extern crate self as cosmic_config;

#[cfg(feature = "calloop")]
pub mod calloop;

#[cfg(feature = "macro")]
pub use cosmic_config_derive;

/// The document name inside an application's version directory.
const DOCUMENT: &str = "config.ron";

#[derive(Debug)]
pub enum Error {
    AtomicWrites(atomicwrites::Error<io::Error>),
    InvalidName(String),
    Io(io::Error),
    NoConfigDirectory,
    NotFound,
    Ron(ron::Error),
    RonSpanned(ron::error::SpannedError),
    GetKey(String, io::Error),
}

impl fmt::Display for Error {
    #[cold]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AtomicWrites(err) => err.fmt(f),
            Self::InvalidName(name) => write!(f, "invalid config name '{name}'"),
            Self::Io(err) => err.fmt(f),
            Self::NoConfigDirectory => write!(f, "cosmic config directory not found"),
            Self::NotFound => write!(f, "cosmic config key not configured"),
            Self::Ron(err) => err.fmt(f),
            Self::RonSpanned(err) => err.fmt(f),
            Self::GetKey(key, err) => write!(f, "failed to get key '{key}': {err}"),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// Whether the missing config is a real failure or merely an unset key.
    #[inline]
    pub fn is_err(&self) -> bool {
        !matches!(self, Self::NoConfigDirectory | Self::NotFound)
    }
}

impl From<atomicwrites::Error<io::Error>> for Error {
    fn from(err: atomicwrites::Error<io::Error>) -> Self {
        Self::AtomicWrites(err)
    }
}

impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<ron::Error> for Error {
    fn from(err: ron::Error) -> Self {
        Self::Ron(err)
    }
}

impl From<ron::error::SpannedError> for Error {
    fn from(err: ron::error::SpannedError) -> Self {
        Self::RonSpanned(err)
    }
}

pub trait ConfigGet {
    /// Get a value, falling back to the system default.
    fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T, Error>;

    /// Get a value from this user's configuration only.
    fn get_local<T: DeserializeOwned>(&self, key: &str) -> Result<T, Error>;

    /// Get the system-wide default.
    fn get_system_default<T: DeserializeOwned>(&self, key: &str) -> Result<T, Error>;
}

pub trait ConfigSet {
    fn set<T: Serialize>(&self, key: &str, value: T) -> Result<(), Error>;
}

#[derive(Clone, Debug)]
pub struct Config {
    system_path: Option<PathBuf>,
    user_path: Option<PathBuf>,
    previous: Option<Box<Config>>,
}

/// A key or name is a relative path of plain components, never `.` or `..`.
fn sanitize_name(name: &str) -> Result<&Path, Error> {
    let path = Path::new(name);
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(path)
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

/// The directories searched for system defaults: `XDG_DATA_DIRS`, or the
/// usual two when it is unset.
fn data_dirs() -> Vec<PathBuf> {
    if let Some(list) = env::var_os("XDG_DATA_DIRS") {
        let dirs: Vec<PathBuf> = env::split_paths(&list).collect();
        if !dirs.is_empty() {
            return dirs;
        }
    }
    vec![
        PathBuf::from("/usr/local/share"),
        PathBuf::from("/usr/share"),
    ]
}

/// The first `<data>/cosmic/<name>/v<n>` that actually holds a document.
fn system_directory(relative: &Path) -> Option<PathBuf> {
    data_dirs()
        .into_iter()
        .map(|dir| dir.join("cosmic").join(relative))
        .find(|dir| dir.join(DOCUMENT).is_file())
}

impl Config {
    /// The system defaults for `name` at `version`.
    pub fn system(name: &str, version: u64) -> Result<Self, Error> {
        Self::system_inner(name, version, true)
    }

    fn system_inner(name: &str, version: u64, look_for_previous: bool) -> Result<Self, Error> {
        let relative = sanitize_name(name)?.join(format!("v{version}"));
        Ok(Self {
            system_path: system_directory(&relative),
            user_path: None,
            previous: if version > 1 && look_for_previous {
                Self::system_inner(name, version - 1, false)
                    .ok()
                    .map(Box::new)
            } else {
                None
            },
        })
    }

    /// This user's configuration for `name` at `version`.
    pub fn new(name: &str, version: u64) -> Result<Self, Error> {
        Self::new_inner(name, version, true)
    }

    fn new_inner(name: &str, version: u64, look_for_previous: bool) -> Result<Self, Error> {
        let relative = sanitize_name(name)?.join(format!("v{version}"));
        let mut user_path = dirs::config_dir().ok_or(Error::NoConfigDirectory)?;
        user_path.push("cosmic");
        user_path.push(&relative);
        fs::create_dir_all(&user_path)?;
        Ok(Self {
            system_path: system_directory(&relative),
            user_path: Some(user_path),
            previous: if version > 1 && look_for_previous {
                Self::new_inner(name, version - 1, false).ok().map(Box::new)
            } else {
                None
            },
        })
    }

    /// This user's configuration under a caller-chosen root instead of the
    /// XDG config directory.
    pub fn with_custom_path(name: &str, version: u64, root: PathBuf) -> Result<Self, Error> {
        Self::with_custom_path_inner(name, version, root, true)
    }

    fn with_custom_path_inner(
        name: &str,
        version: u64,
        root: PathBuf,
        look_for_previous: bool,
    ) -> Result<Self, Error> {
        let relative = sanitize_name(name)?.join(format!("v{version}"));
        let mut user_path = root.clone();
        user_path.push("cosmic");
        user_path.push(relative);
        fs::create_dir_all(&user_path)?;
        Ok(Self {
            system_path: None,
            user_path: Some(user_path),
            previous: if version > 1 && look_for_previous {
                Self::with_custom_path_inner(name, version - 1, root, false)
                    .ok()
                    .map(Box::new)
            } else {
                None
            },
        })
    }

    /// This user's state (the part that changes without user action), which
    /// lives under the XDG state directory rather than the config one.
    pub fn new_state(name: &str, version: u64) -> Result<Self, Error> {
        Self::new_state_inner(name, version, true)
    }

    fn new_state_inner(name: &str, version: u64, look_for_previous: bool) -> Result<Self, Error> {
        let relative = sanitize_name(name)?.join(format!("v{version}"));
        let mut user_path = dirs::state_dir().ok_or(Error::NoConfigDirectory)?;
        user_path.push("cosmic");
        user_path.push(relative);
        fs::create_dir_all(&user_path)?;
        Ok(Self {
            system_path: None,
            user_path: Some(user_path),
            previous: if version > 1 && look_for_previous {
                Self::new_state_inner(name, version - 1, false)
                    .ok()
                    .map(Box::new)
            } else {
                None
            },
        })
    }

    /// Start a transaction, so several keys are written in one rename.
    #[inline]
    pub fn transaction(&self) -> ConfigTransaction<'_> {
        ConfigTransaction {
            config: self,
            updates: Mutex::new(Vec::new()),
        }
    }

    /// The directory holding this user's document.
    #[cfg(feature = "calloop")]
    pub(crate) fn user_directory(&self) -> Result<&Path, Error> {
        self.user_path.as_deref().ok_or(Error::NoConfigDirectory)
    }

    /// The whole user document, as stored. Empty when nothing has been written.
    #[cfg(feature = "calloop")]
    pub(crate) fn document(&self) -> Result<BTreeMap<String, String>, Error> {
        let user_path = self.user_directory()?;
        match fs::read_to_string(user_path.join(DOCUMENT)) {
            Ok(text) => ron::from_str(&text).map_err(Error::RonSpanned),
            Err(why) if why.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(why) => Err(Error::Io(why)),
        }
    }
}

impl ConfigGet for Config {
    fn get<T: DeserializeOwned>(&self, key: &str) -> Result<T, Error> {
        match self.get_local(key) {
            Ok(value) => Ok(value),
            Err(Error::NotFound) => self.get_system_default(key),
            Err(why) => Err(why),
        }
    }

    fn get_local<T: DeserializeOwned>(&self, key: &str) -> Result<T, Error> {
        sanitize_name(key)?;
        if let Some(user_path) = self.user_path.as_ref()
            && let Some(text) = stored_value(user_path, key)?
        {
            return ron::from_str(&text).map_err(Error::RonSpanned);
        }
        match self.previous.as_ref() {
            Some(previous) => previous.get_local(key),
            None => Err(Error::NotFound),
        }
    }

    fn get_system_default<T: DeserializeOwned>(&self, key: &str) -> Result<T, Error> {
        let Some(system_path) = self.system_path.as_ref() else {
            return Err(Error::NoConfigDirectory);
        };
        match stored_value(system_path, key)? {
            Some(text) => ron::from_str(&text).map_err(Error::RonSpanned),
            None => Err(Error::NotFound),
        }
    }
}

/// One key's RON text from the document in `dir`, or `None` when the document
/// or the key is absent.
fn stored_value(dir: &Path, key: &str) -> Result<Option<String>, Error> {
    let text = match fs::read_to_string(dir.join(DOCUMENT)) {
        Ok(text) => text,
        Err(why) if why.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(why) => return Err(Error::GetKey(key.to_owned(), why)),
    };
    let document: BTreeMap<String, String> = ron::from_str(&text)?;
    Ok(document.get(key).cloned())
}

impl ConfigSet for Config {
    fn set<T: Serialize>(&self, key: &str, value: T) -> Result<(), Error> {
        let tx = self.transaction();
        tx.set(key, value)?;
        tx.commit()
    }
}

#[must_use = "config transactions must be committed"]
pub struct ConfigTransaction<'a> {
    config: &'a Config,
    updates: Mutex<Vec<(String, String)>>,
}

impl ConfigTransaction<'_> {
    /// Apply every pending key in one atomic write.
    pub fn commit(self) -> Result<(), Error> {
        let updates = self
            .updates
            .into_inner()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if updates.is_empty() {
            return Ok(());
        }
        let user_path = self
            .config
            .user_path
            .as_ref()
            .ok_or(Error::NoConfigDirectory)?;
        let mut document = match fs::read_to_string(user_path.join(DOCUMENT)) {
            Ok(text) => ron::from_str(&text)?,
            Err(why) if why.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(why) => return Err(Error::Io(why)),
        };
        for (key, text) in updates {
            document.insert(key, text);
        }
        let text = ron::ser::to_string_pretty(&document, ron::ser::PrettyConfig::new())?;
        atomicwrites::AtomicFile::new(
            user_path.join(DOCUMENT),
            atomicwrites::OverwriteBehavior::AllowOverwrite,
        )
        .write(|file| file.write_all(text.as_bytes()))?;
        Ok(())
    }
}

impl ConfigSet for ConfigTransaction<'_> {
    fn set<T: Serialize>(&self, key: &str, value: T) -> Result<(), Error> {
        sanitize_name(key)?;
        let text = ron::ser::to_string_pretty(&value, ron::ser::PrettyConfig::new())?;
        let mut updates = self
            .updates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        updates.push((key.to_owned(), text));
        Ok(())
    }
}

pub trait CosmicConfigEntry
where
    Self: Sized,
{
    const VERSION: u64;

    fn write_entry(&self, config: &Config) -> Result<(), Error>;
    fn get_entry(config: &Config) -> Result<Self, (Vec<Error>, Self)>;

    /// Re-read the keys in `changed_keys`, returning the ones whose value
    /// actually differs from this entry.
    fn update_keys<T: AsRef<str>>(
        &mut self,
        config: &Config,
        changed_keys: &[T],
    ) -> (Vec<Error>, Vec<&'static str>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cosmic_config_derive::CosmicConfigEntry;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("cosmic-config-{}-{n}", std::process::id()));
            fs::create_dir_all(&path).expect("temporary directory");
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).ok();
        }
    }

    #[derive(Debug, Default, PartialEq, CosmicConfigEntry)]
    #[version = 3]
    struct Entry {
        width: u32,
        title: String,
    }

    #[test]
    fn set_then_get_round_trips() {
        let dir = TempDir::new();
        let config = Config::with_custom_path("test.app", 1, dir.0.clone()).expect("config");
        config.set("name", "value").expect("set");
        assert_eq!(config.get::<String>("name").expect("get"), "value");
    }

    #[test]
    fn a_missing_key_is_not_a_failure() {
        let dir = TempDir::new();
        let config = Config::with_custom_path("test.app", 1, dir.0.clone()).expect("config");
        let why = config
            .get_local::<String>("absent")
            .expect_err("key is absent");
        assert!(matches!(why, Error::NotFound));
        assert!(!why.is_err());

        // Without a system default to fall back to, `get` reports the missing
        // directory, which the entry derive treats as "keep the default" too.
        let why = config.get::<String>("absent").expect_err("no defaults");
        assert!(matches!(why, Error::NoConfigDirectory));
        assert!(!why.is_err());
    }

    #[test]
    #[cfg(feature = "calloop")]
    fn the_keys_share_one_document() {
        let dir = TempDir::new();
        let config = Config::with_custom_path("test.app", 1, dir.0.clone()).expect("config");
        config.set("alpha", 1u32).expect("set alpha");
        config.set("beta", 2u32).expect("set beta");
        let document = dir
            .0
            .join("cosmic")
            .join("test.app")
            .join("v1")
            .join(DOCUMENT);
        assert!(document.is_file(), "expected one document at {document:?}");
        let stored = config.document().expect("document");
        assert_eq!(stored.len(), 2);
    }

    #[test]
    fn a_transaction_publishes_every_key() {
        let dir = TempDir::new();
        let config = Config::with_custom_path("test.app", 1, dir.0.clone()).expect("config");
        let tx = config.transaction();
        tx.set("alpha", 1u32).expect("set alpha");
        tx.set("beta", 2u32).expect("set beta");
        tx.commit().expect("commit");
        assert_eq!(config.get::<u32>("alpha").expect("get alpha"), 1);
        assert_eq!(config.get::<u32>("beta").expect("get beta"), 2);
    }

    #[test]
    fn a_newer_version_reads_the_previous_one() {
        let dir = TempDir::new();
        let v1 = Config::with_custom_path("test.app", 1, dir.0.clone()).expect("v1");
        v1.set("shift", 7u32).expect("set");
        let v2 = Config::with_custom_path("test.app", 2, dir.0.clone()).expect("v2");
        assert_eq!(v2.get::<u32>("shift").expect("get"), 7);
    }

    #[test]
    fn a_derived_entry_writes_and_reads_each_field() {
        let dir = TempDir::new();
        let config = Config::with_custom_path("test.entry", 3, dir.0.clone()).expect("config");
        let entry = Entry {
            width: 80,
            title: "hi".to_owned(),
        };
        entry.write_entry(&config).expect("write");
        assert_eq!(Entry::get_entry(&config).expect("read"), entry);
        assert_eq!(<Entry as CosmicConfigEntry>::VERSION, 3);
    }

    #[test]
    fn update_keys_reports_only_changed_fields() {
        let dir = TempDir::new();
        let config = Config::with_custom_path("test.entry", 3, dir.0.clone()).expect("config");
        let mut entry = Entry::default();
        entry.write_entry(&config).expect("write");
        config.set("width", 100u32).expect("set width");
        let (errors, keys) = entry.update_keys(&config, &["width", "title"]);
        assert!(errors.is_empty());
        assert_eq!(keys, vec!["width"]);
        assert_eq!(entry.width, 100);
    }

    #[test]
    fn a_setter_writes_only_when_the_value_changes() {
        let dir = TempDir::new();
        let config = Config::with_custom_path("test.entry", 3, dir.0.clone()).expect("config");
        let mut entry = Entry::default();
        assert!(entry.set_width(&config, 40).expect("set"));
        assert!(!entry.set_width(&config, 40).expect("set again"));
        assert_eq!(config.get::<u32>("width").expect("get"), 40);
    }

    #[test]
    fn a_name_with_a_parent_component_is_rejected() {
        let dir = TempDir::new();
        let why =
            Config::with_custom_path("../escape", 1, dir.0.clone()).expect_err("invalid name");
        assert!(matches!(why, Error::InvalidName(_)));
    }
}
