use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

const MAX_EVENT_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn take_event(buffer: &mut Vec<u8>) -> anyhow::Result<Option<serde_json::Value>> {
    let delimiter = [
        buffer
            .windows(2)
            .position(|part| part == b"\n\n")
            .map(|at| (at, 2)),
        buffer
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .map(|at| (at, 4)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(at, _)| *at);
    let Some((end, width)) = delimiter else {
        anyhow::ensure!(
            buffer.len() <= MAX_EVENT_BYTES,
            "pack observation event is too large"
        );
        return Ok(None);
    };
    anyhow::ensure!(
        end <= MAX_EVENT_BYTES,
        "pack observation event is too large"
    );
    let bytes: Vec<_> = buffer.drain(..end + width).collect();
    let text = std::str::from_utf8(&bytes[..end])?;
    let mut kind = "message";
    let mut data = Vec::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            kind = value.trim();
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push(value.strip_prefix(' ').unwrap_or(value));
        }
    }
    match kind {
        "next" => Ok(Some(serde_json::from_str(&data.join("\n"))?)),
        "error" => anyhow::bail!("pack observation is unavailable"),
        _ => anyhow::bail!("unexpected pack observation event"),
    }
}

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_scoped_to_the_window_and_observation() {
        let registry = UxWatchRegistry::default();
        let a = registry.insert("one", "a").unwrap();
        let b = registry.insert("one", "b").unwrap();
        let other = registry.insert("two", "a").unwrap();
        registry.cancel("two", "b");
        assert!(!b.is_cancelled());
        registry.cancel("one", "a");
        assert!(a.is_cancelled());
        assert!(!other.is_cancelled());
        registry.cancel_window("one");
        assert!(b.is_cancelled());
        assert!(!other.is_cancelled());
    }

    #[test]
    fn observations_are_bounded_and_ids_cannot_be_reused() {
        let registry = UxWatchRegistry::default();
        for index in 0..16 {
            registry.insert("one", &index.to_string()).unwrap();
        }
        assert!(registry.insert("one", "extra").is_err());
        assert!(registry.insert("one", "0").is_err());
        registry.cancel("one", "0");
        assert!(registry.insert("one", "extra").is_ok());
    }

    #[test]
    fn decodes_split_events_and_rejects_errors_and_large_frames() {
        let mut buffer = b"event: next\ndata: {\"value\":".to_vec();
        assert!(take_event(&mut buffer).unwrap().is_none());
        buffer.extend_from_slice(b"1}\n\nevent: error\ndata: denied\n\n");
        assert_eq!(
            take_event(&mut buffer).unwrap(),
            Some(serde_json::json!({"value": 1}))
        );
        assert!(take_event(&mut buffer).is_err());
        let mut too_large = vec![b'x'; MAX_EVENT_BYTES + 1];
        assert!(take_event(&mut too_large).is_err());
    }
}
