//! Configuration optimization (#1455), a consumer of the eval core contract.
//! This module currently holds the pure promotion policy. The job record, the
//! driver and promotion follow once the eval runner exists.

pub mod policy;

pub use policy::{
    alpha_effective_ppm, decide, decide_gates, evidence_from_pairs, permutation_p_ppm,
    CaseEvidence, Decision, DecisionReport, Evidence, Gates, InconclusiveReason, Mode, PolicyV2,
    RejectReason, TokenTotals, POLICY_VERSION,
};
