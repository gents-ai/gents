//! The desktop shell's pack watches: per-window cancellation tokens and the
//! locked delivery to a window's IPC channel. The watch itself is
//! `gents_desktop_core::pack_watch`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub(crate) struct UxWatchRegistry {
    entries: Arc<Mutex<BTreeMap<String, BTreeMap<String, CancellationToken>>>>,
}

impl UxWatchRegistry {
    pub(crate) fn insert(&self, window: &str, id: &str) -> anyhow::Result<CancellationToken> {
        let mut entries = self.entries.lock().expect("ux watch registry poisoned");
        let window_entries = entries.entry(window.to_owned()).or_default();
        anyhow::ensure!(
            window_entries.len() < 16,
            "too many active pack observations"
        );
        anyhow::ensure!(
            !window_entries.contains_key(id),
            "duplicate pack observation"
        );
        let token = CancellationToken::new();
        window_entries.insert(id.to_owned(), token.clone());
        Ok(token)
    }

    pub(crate) fn cancel(&self, window: &str, id: &str) {
        let mut entries = self.entries.lock().expect("ux watch registry poisoned");
        if let Some(window_entries) = entries.get_mut(window) {
            if let Some(token) = window_entries.remove(id) {
                token.cancel();
            }
            if window_entries.is_empty() {
                entries.remove(window);
            }
        }
    }

    pub(crate) fn cancel_window(&self, window: &str) {
        let mut entries = self.entries.lock().expect("ux watch registry poisoned");
        if let Some(window_entries) = entries.remove(window) {
            for token in window_entries.into_values() {
                token.cancel();
            }
        }
    }

    /// Sends only while the watch is still registered, holding the registry
    /// lock across the send: once a cancellation (unwatch, window destruction,
    /// or a selection change, which cancels before it writes the selection)
    /// returns, no event of the cancelled watch reaches the window. TLA
    /// `PackUxWatch` `AtomicDeliver`.
    pub(crate) fn deliver<F>(&self, window: &str, id: &str, send: F) -> bool
    where
        F: FnOnce() -> bool,
    {
        let entries = self.entries.lock().expect("ux watch registry poisoned");
        let live = entries
            .get(window)
            .and_then(|window_entries| window_entries.get(id))
            .is_some_and(|token| !token.is_cancelled());
        live && send()
    }
}

#[cfg(test)]
#[path = "ux_watch/tests.rs"]
mod tests;
