//! Marker traits partitioning document fields by ownership.

/// Implementations must not contain any field written by the runtime —
pub trait DesiredFields {
    fn collection_tag(&self) -> &'static str;
}

pub trait LiveFields {
    fn collection_tag(&self) -> &'static str;
}
