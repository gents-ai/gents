//! Marker traits partitioning document fields by ownership.

/// A value that represents only operator-owned (desired-state) document fields.
///
/// Implementations must not contain any field written by the runtime —
/// `next_run_at`, `last_probe`, `probe_status`, `run_count`, etc.
pub trait DesiredFields {
    fn collection_tag(&self) -> &'static str;
}

pub trait LiveFields {
    fn collection_tag(&self) -> &'static str;
}
