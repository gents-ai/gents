use serde::{Deserialize, Serialize};

use super::agent::NextCall;

/// Lean `SchemaInstall.Live`: a declared collection against the node's live
/// collection of that name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveCollection {
    Missing,
    Exact,
    Extra,
    Pending,
    Conflicting,
}

/// Lean `SchemaInstall.Decision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaInstallDecision {
    Publish,
    Satisfied,
    AwaitsFields,
    FieldsPending,
    Mismatch,
}

impl SchemaInstallDecision {
    /// The call that lets the caller correct a refused install.
    pub fn recovery(self) -> Option<NextCall> {
        match self {
            Self::FieldsPending => Some(NextCall::VerbHelp {
                resource: "collection",
                verb: "update",
            }),
            Self::Mismatch => Some(NextCall::Inspect {
                resource: "collection",
            }),
            Self::Publish | Self::Satisfied | Self::AwaitsFields => None,
        }
    }
}
