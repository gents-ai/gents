use serde::Deserialize;

/// Generated witness for the proof-level invocation/execution/output split.
#[derive(Debug, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanToolExecutionSplitCase {
    pub(crate) name: String,
    pub(crate) operation: String,
    pub(crate) disposition: String,
    pub(crate) exact_projection: bool,
    pub(crate) output_pins_running: bool,
    pub(crate) terminal_output_closed: bool,
    pub(crate) owner_preserved: bool,
    pub(crate) approval_pins_held: bool,
    pub(crate) immutable_noop: bool,
}
