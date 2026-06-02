use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimProjectionCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) request_state: String,
    pub(crate) response_status: Option<String>,
    pub(crate) local_interrupt_acked: bool,
    pub(crate) projected_phase: String,
    pub(crate) terminal: bool,
    pub(crate) effectively_terminal: bool,
    pub(crate) interruptible_request_state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanCodexShimTurnLifecycleCase {
    pub(crate) witness: String,
    pub(crate) lean_theorems: Vec<String>,
    pub(crate) action: String,
    pub(crate) pre_phase: String,
    pub(crate) post_phase: String,
    pub(crate) pre_lex_ord: usize,
    pub(crate) post_lex_ord: usize,
    pub(crate) monotonic: bool,
}
