use serde::Deserialize;

use super::{required_nullable, LeanCanonicalCoordinate, LeanClaudeTaggedReplayRow};

/// One create-and-compare delivery evaluated by
/// `Compaction.DurableReduction.persist` in Lean.
#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanDurableReductionCase {
    pub(crate) name: String,
    pub(crate) request_doc_id: u64,
    pub(crate) turn_index: usize,
    pub(crate) ordinal: usize,
    pub(crate) checkpoint: u64,
    pub(crate) claim_commit: u64,
    pub(crate) prior_checkpoint: Option<u64>,
    pub(crate) prior_claim_commit: Option<u64>,
    pub(crate) pair_closed: bool,
    pub(crate) inference_cites: bool,
    pub(crate) inference_supported: bool,
    pub(crate) title_cites: bool,
    pub(crate) outcome: String,
    pub(crate) durable_after: bool,
    pub(crate) send_permitted: bool,
    pub(crate) consumed: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanFullInputReductionKey {
    pub(crate) agent_did: u64,
    pub(crate) session_id: u64,
    pub(crate) request_doc_id: u64,
    pub(crate) turn_index: usize,
    pub(crate) ordinal: usize,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanFullInputProjection {
    pub(crate) value: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) tagged_rows: Option<Vec<LeanClaudeTaggedReplayRow>>,
    pub(crate) retired: Vec<LeanCanonicalCoordinate>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanFullInputFact {
    pub(crate) claim_commit: u64,
    pub(crate) source_boundary: u64,
    pub(crate) source_projection: LeanFullInputProjection,
    pub(crate) checkpoint: LeanFullInputProjection,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) producer_call: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) parent: Option<LeanFullInputReductionKey>,
    pub(crate) pair_closed: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanFullInputPrior {
    pub(crate) key: LeanFullInputReductionKey,
    pub(crate) fact: LeanFullInputFact,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanFullInputSourceObservation {
    pub(crate) tag: LeanCanonicalCoordinate,
    pub(crate) agent_did: u64,
    pub(crate) session_id: u64,
    pub(crate) source_boundary: u64,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanFullInputCaptureKind {
    Inference,
    Title,
    Compaction,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanFullInputCaptureCitation {
    pub(crate) kind: LeanFullInputCaptureKind,
    pub(crate) supported: bool,
    pub(crate) reduction_keys: Vec<LeanFullInputReductionKey>,
}

/// Model-derived full-input rewrite admission, immutable Fact persistence,
/// and consumed-lineage retirement. No native consumer is claimed yet.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDurableFullInputRewriteCase {
    pub(crate) name: String,
    pub(crate) key: LeanFullInputReductionKey,
    pub(crate) lineage: Vec<LeanFullInputReductionKey>,
    #[serde(deserialize_with = "required_nullable")]
    pub(crate) prior: Option<LeanFullInputPrior>,
    pub(crate) observations: Vec<LeanFullInputSourceObservation>,
    pub(crate) fact: LeanFullInputFact,
    pub(crate) captures: Vec<LeanFullInputCaptureCitation>,
    pub(crate) prior_consumed: bool,
    pub(crate) outcome: String,
    pub(crate) retired: Vec<LeanCanonicalCoordinate>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDurableSessionRewriteAction {
    pub(crate) key: LeanFullInputReductionKey,
    pub(crate) lineage: Vec<LeanFullInputReductionKey>,
    pub(crate) observations: Vec<LeanFullInputSourceObservation>,
    pub(crate) fact: LeanFullInputFact,
    pub(crate) cursor: usize,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDurableSessionCursorEntry {
    pub(crate) key: LeanFullInputReductionKey,
    pub(crate) cursor: usize,
}

/// Model-derived atomic session cursor and immutable full-input Fact join.
/// The decoder makes no native transaction-conformance claim on its own.
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanDurableSessionRewriteCase {
    pub(crate) name: String,
    pub(crate) actions: Vec<LeanDurableSessionRewriteAction>,
    pub(crate) outcomes: Vec<String>,
    pub(crate) entries: Vec<LeanDurableSessionCursorEntry>,
    pub(crate) stored_keys: Vec<LeanFullInputReductionKey>,
}
