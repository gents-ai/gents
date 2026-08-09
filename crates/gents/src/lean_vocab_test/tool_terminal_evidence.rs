use serde::Deserialize;

/// Generated witness for terminal output-or-typed-omission closure.
#[derive(Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolTerminalEvidenceCase {
    pub(crate) name: String,
    pub(crate) operation: String,
    pub(crate) disposition: String,
    pub(crate) evidence_kind: String,
    pub(crate) terminal_phase: String,
    pub(crate) omission_reason: String,
    pub(crate) exact_projection: bool,
    pub(crate) evidence_closed: bool,
    pub(crate) mutually_exclusive: bool,
    pub(crate) owner_preserved: bool,
    pub(crate) phase_reason_valid: bool,
    pub(crate) exact_approval_bound: bool,
    pub(crate) immutable_noop: bool,
}
