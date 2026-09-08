use serde::{Deserialize, Serialize};

/// The enum serializes into the `derivation_json` column
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BudgetDerivation {
    /// Top-level. Allocation came from config or the requester.
    Root,
    /// Divide evenly; no reserve.
    EvenSplit { siblings: u32, ordinal: u32 },
    /// Basis points. Draw against a reserve held by the parent.
    Reserved { reserve_fraction_bp: u32 },
    /// Explicit grant from a top-up request.
    Grant {
        granted_by_budget_id: String,
        sequence: u32,
    },
}
