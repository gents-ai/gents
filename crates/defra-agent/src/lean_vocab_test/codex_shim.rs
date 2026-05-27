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
}
