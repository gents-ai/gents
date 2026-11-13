//! The Lean client-shell session-turn row (`Proofs.Conformance.ClientShell`)
//! that session-turn families carry and the desktop consumers recode into.
//! The decoder generator emits only types a snapshot key reaches, and no key
//! reaches this one.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSessionTurnRow {
    pub doc: usize,
    pub request: usize,
    pub requester: usize,
    pub state: String,
    pub folded_into: Option<usize>,
    pub replaced_by: Option<usize>,
    pub queued_after: Option<usize>,
    pub retry_parent: Option<usize>,
    pub retry_parent_doc: Option<usize>,
    pub completed: bool,
    pub public_input: bool,
    pub background_wake: bool,
    pub authored_input_materialized: bool,
}
