/// Lean `RuntimeReconcile.Placement.Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ProposalOutcome {
    Drop,
    AdvanceBasis,
    Apply,
}

/// Lean `RuntimeReconcile.Placement.proposalOutcome`.
pub(super) fn proposal_outcome(
    admitted: bool,
    fingerprint_changed: bool,
    parked: bool,
) -> ProposalOutcome {
    if !admitted {
        ProposalOutcome::Drop
    } else if fingerprint_changed || parked {
        ProposalOutcome::Apply
    } else {
        ProposalOutcome::AdvanceBasis
    }
}
