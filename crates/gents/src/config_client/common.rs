use std::sync::atomic::{AtomicI64, Ordering};

// Process-local monotonicity closes the practical collision window for rapid
// retries, concurrent apply workers, and wall-clock rollback while the process
// is alive. A process restart cannot make a system clock globally monotonic;
// DefraDB's unique/live checks still reject a collision safely, and a retry
// later in that process mints from a new observation. The i64 nanosecond
// representation reaches its chrono boundary in 2262.
static LAST_RECREATE_IDENTITY_NANOS: AtomicI64 = AtomicI64::new(i64::MIN);

/// Mint the timestamp carried only by document add/recreate branches.
pub fn mint_recreate_identity_timestamp() -> String {
    let now = chrono::Utc::now()
        .timestamp_nanos_opt()
        .expect("current time must fit chrono's nanosecond timestamp range");
    let mut observed = LAST_RECREATE_IDENTITY_NANOS.load(Ordering::Relaxed);
    loop {
        let candidate = now.max(observed.saturating_add(1));
        match LAST_RECREATE_IDENTITY_NANOS.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => {
                return chrono::DateTime::<chrono::Utc>::from_timestamp_nanos(candidate)
                    .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
            }
            Err(actual) => observed = actual,
        }
    }
}

/// Mint a fresh document identity for an apply-owned create.
///
/// Deletion is terminal in DefraDB and docIDs are content-addressed, so
/// recreating a row whose manifest content is IDENTICAL to the tombstoned one
/// would regenerate the tombstoned docID. Every apply-controlled collection
/// carries `updated_at`, so stamping the add branch gives each incarnation a
/// distinct identity without changing a live row's update payload.
pub fn mint_recreate_identity(add_doc: &serde_json::Value) -> serde_json::Value {
    let mut doc = add_doc.clone();
    if let Some(map) = doc.as_object_mut() {
        map.insert(
            "updated_at".to_string(),
            serde_json::Value::String(mint_recreate_identity_timestamp()),
        );
    }
    doc
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn recreate_identity_preserves_content_and_stamps_updated_at() {
        let original = json!({
            "tools_id": "tools-a",
            "created_at": "2026-01-01T00:00:00Z"
        });

        let minted = mint_recreate_identity(&original);

        assert_eq!(minted.get("tools_id"), original.get("tools_id"));
        assert_eq!(minted.get("created_at"), original.get("created_at"));
        assert!(minted.get("updated_at").and_then(Value::as_str).is_some());
        assert!(original.get("updated_at").is_none());
    }

    #[test]
    fn recreate_identity_timestamps_are_distinct_and_monotonic() {
        let first = mint_recreate_identity_timestamp();
        let second = mint_recreate_identity_timestamp();
        let first = chrono::DateTime::parse_from_rfc3339(&first).unwrap();
        let second = chrono::DateTime::parse_from_rfc3339(&second).unwrap();

        assert!(second > first);
    }
}
