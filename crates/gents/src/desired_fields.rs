//! Marker traits partitioning document fields by ownership.
//!
//! Apply-side writers (CLI manifest apply) produce values implementing
//! [`DesiredFields`]. Runtime writers (reconcile, scheduler, lifecycle)
//! produce values implementing [`LiveFields`]. The traits are marker-only;
//! their purpose is to make the apply-vs-runtime field partition
//! unrepresentable to cross at the API boundary.

/// A value that represents only operator-owned (desired-state) document fields.
///
/// Implementations must not contain any field written by the runtime —
/// `next_run_at`, `last_probe`, `probe_status`, `run_count`, etc.
pub trait DesiredFields {
    /// Stable collection tag (snake_case). Mirrors the Rust
    /// `gents_cli::collection::Collection::Display` variant names
    /// and the Lean `ApplyReconcile.Collection` constructors.
    fn collection_tag(&self) -> &'static str;
}

/// A value that represents only runtime-owned (live-state) document fields.
///
/// Reserved for runtime-side writers to adopt. Not currently enforced on the
/// runtime half — see the spec non-goals and the `LiveFields` adoption
/// follow-on.
pub trait LiveFields {
    /// Stable collection tag (snake_case).
    fn collection_tag(&self) -> &'static str;
}
