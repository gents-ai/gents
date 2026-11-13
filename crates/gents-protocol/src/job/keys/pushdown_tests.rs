use super::{decode_job_retry_key, job_message_key, job_session_id, job_spawn_key};

/// Lean `SpawnKey.retryKey`, `messageKey` and `sessionId`: the protocol
/// encoders reproduce the Lean strings byte for byte. The protocol's
/// `decode_job_retry_key` is the one Rust decoder; it agrees with
/// `decodeRetryKey?` on the retry key and on the message keys, which are not
/// retry keys.
#[test]
fn generated_plugin_job_key_cases_match_protocol_encoders() {
    use gents_lean_vocab::families::plugin_job_spawn_keys::{DecodedKey, KeyOutput, MessageKey};
    gents_lean_vocab::families::plugin_job_spawn_keys::replay(|case| {
        let (root, slot) = (&case.root, &case.slot);
        let attempt = u32::try_from(case.attempt).unwrap();
        let retry_key = job_spawn_key(root, slot, attempt);
        let message_keys = case
            .messages
            .iter()
            .map(|&n| MessageKey {
                n,
                key: job_message_key(root, slot, attempt, u32::try_from(n).unwrap()),
            })
            .collect::<Vec<_>>();
        KeyOutput {
            session_id: job_session_id(root, slot, attempt),
            decoded: decode_job_retry_key(&retry_key).map(|(slot, attempt)| DecodedKey {
                slot,
                attempt: u64::from(attempt),
            }),
            message_keys_decode: message_keys
                .iter()
                .any(|message| decode_job_retry_key(&message.key).is_some()),
            message_keys,
            retry_key,
        }
    });
}
