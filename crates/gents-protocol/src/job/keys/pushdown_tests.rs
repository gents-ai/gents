use std::collections::BTreeSet;

use super::{decode_job_retry_key, job_message_key, job_session_id, job_spawn_key};

/// Lean `SpawnKey.retryKey`, `messageKey` and `sessionId`: the protocol
/// encoders reproduce the Lean strings byte for byte. The protocol's
/// `decode_job_retry_key` is the one Rust decoder; it agrees with
/// `decodeRetryKey?` on the retry key and on the message keys, which are not
/// retry keys.
#[test]
fn generated_plugin_job_key_cases_match_protocol_encoders() {
    let cases = gents_lean_vocab::lean_plugin_job_key_cases();
    assert_eq!(
        cases.len(),
        5,
        "Lean pins five key rows in `key_regressions`"
    );
    let mut names = BTreeSet::new();
    for case in cases {
        assert!(names.insert(case.name.as_str()), "duplicate {}", case.name);
        let (root, slot, attempt) = (&case.root, &case.slot, case.attempt);
        let expected = &case.expected;
        assert_eq!(
            job_spawn_key(root, slot, attempt),
            expected.retry_key,
            "{}: retry key",
            case.name
        );
        assert_eq!(
            job_session_id(root, slot, attempt),
            expected.session_id,
            "{}: session id",
            case.name
        );
        let message_keys: Vec<(u32, String)> = case
            .messages
            .iter()
            .map(|&n| (n, job_message_key(root, slot, attempt, n)))
            .collect();
        assert_eq!(
            message_keys,
            expected
                .message_keys
                .iter()
                .map(|m| (m.n, m.key.clone()))
                .collect::<Vec<_>>(),
            "{}: message keys",
            case.name
        );
        assert_eq!(
            decode_job_retry_key(&expected.retry_key),
            expected
                .decoded
                .as_ref()
                .map(|d| (d.slot.clone(), d.attempt)),
            "{}: decoded retry key",
            case.name
        );
        assert_eq!(
            message_keys
                .iter()
                .any(|(_, key)| decode_job_retry_key(key).is_some()),
            expected.message_keys_decode,
            "{}: message keys decode",
            case.name
        );
    }
}
