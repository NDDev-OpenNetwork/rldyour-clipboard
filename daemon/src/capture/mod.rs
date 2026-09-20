//! Native clipboard capture, where the platform allows it.
//!
//! On macOS and Windows the daemon can watch the clipboard itself, so it does:
//! a tray client that had to relay every copy would be a second process in the
//! path for no gain.
//!
//! Linux is different, and deliberately so. Mutter implements neither
//! `wlr-data-control` nor `ext-data-control-v1` and its maintainers have said
//! it will not — reading another application's clipboard is treated as
//! something a compositor should not hand out. The only code that can see the
//! selection under GNOME is code running inside the shell, so there the GNOME
//! Shell extension captures and streams entries in over the socket. This
//! module is empty on Linux for that reason, not for lack of an
//! implementation.

use crate::outbox::Watchers;
use crate::proto::Response;
use crate::session::now;
use crate::store::{Accepted, Store};
use std::sync::Arc;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Starts watching the clipboard, if this platform lets the daemon do it.
///
/// Returns whether a watcher was started, which is only used to say so in the
/// log: a platform without one is the expected state, not a degraded one.
pub fn spawn(store: Arc<Store>, watchers: Arc<Watchers>) -> bool {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let spawned = std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                let recorder = Recorder { store, watchers };
                #[cfg(target_os = "macos")]
                macos::watch(&recorder);
                #[cfg(target_os = "windows")]
                windows::watch(&recorder);
            });

        if spawned.is_err() {
            eprintln!("rldyour-clipboardd: could not start the clipboard watcher");
            return false;
        }
        return true;
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        // Silences the unused bindings on Linux without a second cfg block
        // around every parameter.
        let _ = (store, watchers);
        false
    }
}

/// What a platform watcher hands one clipboard event to.
///
/// Keeping the archive behind this means a backend deals only in
/// `(mime, bytes)` pairs and never learns how entries are stored, hashed or
/// announced.
#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
pub struct Recorder {
    store: Arc<Store>,
    watchers: Arc<Watchers>,
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
impl Recorder {
    /// Archives one clipboard event holding every representation given.
    ///
    /// Secrets and empty events are dropped by the store, so a backend can
    /// pass on whatever the platform offered without deciding anything.
    pub fn record(&self, parts: Vec<(String, Vec<u8>)>, source: Option<&str>) {
        if parts.is_empty() {
            return;
        }

        let mut accepted = Vec::with_capacity(parts.len());
        for (mime, content) in parts {
            match self.store.accept(
                &mime,
                &mut std::io::Cursor::new(&content),
                content.len() as u64,
            ) {
                Ok(part) => accepted.push(part),
                // One representation the archive will not take must not cost
                // the entry its others.
                Err(error) => {
                    eprintln!("rldyour-clipboardd: skipping {mime}: {error}");
                }
            }
        }

        self.commit(accepted, source);
    }

    fn commit(&self, accepted: Vec<Accepted>, source: Option<&str>) {
        match self.store.commit(&accepted, source, now()) {
            Ok(Some(done)) => {
                for evicted in done.evicted {
                    self.watchers
                        .broadcast(&Response::Removed { entry: evicted });
                }
                let event = if done.created {
                    Response::Added {
                        entry: done.summary,
                    }
                } else {
                    Response::Updated {
                        entry: done.summary,
                    }
                };
                self.watchers.broadcast(&event);
            }
            // Empty, or a secret the store refused. Nothing to announce.
            Ok(None) => {}
            Err(error) => eprintln!("rldyour-clipboardd: could not archive a copy: {error}"),
        }
    }
}
