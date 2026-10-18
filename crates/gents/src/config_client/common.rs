use std::sync::atomic::{AtomicI64, Ordering};

// Process-local monotonicity closes the practical collision window for rapid
// retries, concurrent apply workers, and wall-clock rollback while the process
// is alive; a restart relies on DefraDB's unique/live checks to reject a
// collision safely. The i64 nanosecond representation reaches its chrono
// boundary in 2262.
static LAST_RECREATE_IDENTITY_NANOS: AtomicI64 = AtomicI64::new(i64::MIN);

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
