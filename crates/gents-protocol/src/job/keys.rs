//! Pure encoders for plugin-job spawn keys. They equal Lean `SpawnKey.retryKey`,
//! `SpawnKey.messageKey` and `SpawnKey.sessionId` byte for byte.
//!
//! `decode_job_retry_key` inverts `job_spawn_key` as Lean `decodeRetryKey?`
//! does, so a child request's `retry_key` yields its slot and attempt. Session
//! ids are never parsed.

/// Lean `component`: the length prefix counts characters, as `String.length` does.
fn component(value: &str) -> String {
    format!("{}:{}", value.chars().count(), value)
}

fn suffix(root: &str, slot: &str, attempt: u32) -> String {
    component(root) + &component(slot) + &component(&attempt.to_string())
}

pub fn job_spawn_key(root: &str, slot: &str, attempt: u32) -> String {
    format!("job:{}", suffix(root, slot, attempt))
}

pub fn job_message_key(root: &str, slot: &str, attempt: u32, n: u32) -> String {
    job_spawn_key(root, slot, attempt) + &component("m") + &component(&n.to_string())
}

pub fn job_session_id(root: &str, slot: &str, attempt: u32) -> String {
    format!("job-session:{}", suffix(root, slot, attempt))
}

/// Lean `takeComponent`: the length prefix counts characters.
fn take_component(s: &str) -> Option<(&str, &str)> {
    let (len, rest) = s.split_once(':')?;
    let n: usize = len.parse().ok()?;
    let end = rest.char_indices().nth(n).map_or(rest.len(), |(i, _)| i);
    (rest[..end].chars().count() == n).then(|| rest.split_at(end))
}

/// The slot and attempt of a key `job_spawn_key` emits, as Lean
/// `decodeRetryKey?`; `None` for any other string.
pub fn decode_job_retry_key(key: &str) -> Option<(String, u32)> {
    let rest = key.strip_prefix("job:")?;
    let (root, rest) = take_component(rest)?;
    let (slot, rest) = take_component(rest)?;
    let (attempt, rest) = take_component(rest)?;
    let attempt: u32 = attempt.parse().ok()?;
    (rest.is_empty() && job_spawn_key(root, slot, attempt) == key)
        .then(|| (slot.to_string(), attempt))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLES: [(&str, &str, u32); 4] = [
        ("r", "0:a", 0),
        ("root:with:colons", "12:b:c", 3),
        ("räk", "日本:語", u32::MAX),
        ("", "", 1),
    ];

    #[test]
    fn decode_inverts_spawn_key() {
        for (root, slot, attempt) in SAMPLES {
            let key = job_spawn_key(root, slot, attempt);
            assert_eq!(
                decode_job_retry_key(&key),
                Some((slot.to_string(), attempt)),
                "{key}"
            );
        }
    }

    #[test]
    fn decode_refuses_message_keys_and_session_ids() {
        for (root, slot, attempt) in SAMPLES {
            assert_eq!(
                decode_job_retry_key(&job_message_key(root, slot, attempt, 1)),
                None
            );
            assert_eq!(
                decode_job_retry_key(&job_session_id(root, slot, attempt)),
                None
            );
        }
    }

    #[test]
    fn decode_refuses_malformed_keys() {
        for key in [
            "",
            "job:",
            "job:1:r",
            "job:1:r3:0:a",
            "job:1:r3:0:a1:x",
            "job:1:r3:0:a2:07",
            "job:01:r3:0:a1:0",
            "job:1:r3:0:a1:0x",
            "job:9:r3:0:a1:0",
            "jobs:1:r3:0:a1:0",
        ] {
            assert_eq!(decode_job_retry_key(key), None, "{key}");
        }
    }
}
