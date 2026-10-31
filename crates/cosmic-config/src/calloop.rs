//! A calloop source that reports changes to a config document.
//!
//! The port has no inotify, so instead of subscribing to filesystem events the
//! source polls the document from a timer and compares the key maps. That is
//! what the event loop needs: one callback per batch of changed keys.

use std::collections::BTreeMap;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};

use crate::{Config, Error};

/// How often the document is compared with the last reported state.
const POLL: Duration = Duration::from_millis(250);

pub struct ConfigWatchSource {
    config: Config,
    timer: Timer,
    last: BTreeMap<String, String>,
}

impl ConfigWatchSource {
    pub fn new(config: &Config) -> Result<Self, Error> {
        config.user_directory()?;
        Ok(Self {
            config: config.clone(),
            timer: Timer::from_duration(POLL),
            last: config.document()?,
        })
    }
}

impl calloop::EventSource for ConfigWatchSource {
    type Event = (Config, Vec<String>);
    type Metadata = ();
    type Ret = ();
    type Error = std::io::Error;

    fn process_events<F>(
        &mut self,
        readiness: calloop::Readiness,
        token: calloop::Token,
        mut callback: F,
    ) -> Result<calloop::PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let config = &self.config;
        let last = &mut self.last;
        let mut changed = Vec::new();
        let action = self.timer.process_events(readiness, token, |_, _| {
            if let Ok(document) = config.document() {
                changed = difference(last, &document);
                last.clone_from(&document);
            }
            TimeoutAction::ToDuration(POLL)
        })?;
        if !changed.is_empty() {
            callback((config.clone(), changed), &mut ());
        }
        Ok(action)
    }

    fn register(
        &mut self,
        poll: &mut calloop::Poll,
        token_factory: &mut calloop::TokenFactory,
    ) -> Result<(), calloop::Error> {
        self.timer.register(poll, token_factory)
    }

    fn reregister(
        &mut self,
        poll: &mut calloop::Poll,
        token_factory: &mut calloop::TokenFactory,
    ) -> Result<(), calloop::Error> {
        self.timer.reregister(poll, token_factory)
    }

    fn unregister(&mut self, poll: &mut calloop::Poll) -> Result<(), calloop::Error> {
        self.timer.unregister(poll)
    }
}

/// The keys that were added, removed or rewritten between two documents.
fn difference(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Vec<String> {
    let mut changed = Vec::new();
    for (key, value) in new {
        if old.get(key) != Some(value) {
            changed.push(key.clone());
        }
    }
    for key in old.keys() {
        if !new.contains_key(key) {
            changed.push(key.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn an_unchanged_document_reports_nothing() {
        let old = document(&[("a", "1"), ("b", "2")]);
        assert!(difference(&old, &old).is_empty());
    }

    #[test]
    fn a_rewritten_key_is_reported() {
        let old = document(&[("a", "1")]);
        let new = document(&[("a", "3")]);
        assert_eq!(difference(&old, &new), vec!["a"]);
    }

    #[test]
    fn an_added_and_a_removed_key_are_both_reported() {
        let old = document(&[("gone", "1")]);
        let new = document(&[("added", "2")]);
        let mut changed = difference(&old, &new);
        changed.sort();
        assert_eq!(changed, vec!["added", "gone"]);
    }
}
