use std::collections::BTreeSet;
use std::error::Error;

use serde::{Deserialize, Serialize};

use super::document::{DocumentDeleteError, DocumentOperationError, DocumentPatch, WriteMode};
use super::NextCall;

pub const RESOURCE: &str = "tools";

/// The save of a Tools document: a patch of its top-level groups.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolsSaveInput {
    pub tools_id: String,
    pub mode: WriteMode,
    pub patch: DocumentPatch,
    /// The groups whose stored settings this save may drop.
    pub allow_drop: BTreeSet<String>,
    /// Accepts that the save reaches every Agent that selects these Tools.
    pub shared_acknowledged: bool,
}

/// Lean `SelfConfig.ToolsSave.Reach`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsReach {
    Unselected,
    One,
    Several,
}

/// Lean `SelfConfig.ToolsSave.SharedOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SharedOutcome {
    Proceed,
    RefusedUnacknowledged,
    RefusedWithoutCatalog,
}

/// Lean `SelfConfig.ToolsSave.Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolsSaveOutcome {
    Saved,
    RefusedShared,
    RefusedGrantNotHeld,
    RefusedDropsSettings,
    RefusedPluginNotInstalled,
    RefusedLosesOwnControl,
}

/// Lean `SelfConfig.ToolsSave.SettingChange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingChange {
    Kept,
    Changed,
    Removed,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolsOperationError {
    #[error("these Tools are selected by more than one agent ({}); the save would change every one of them. Acknowledge the shared edit to proceed, or clone the agent to customize it on its own", agents.join(", "))]
    SharedUnacknowledged { agents: Vec<String> },
    #[error("acknowledging a shared edit waives only the sharing refusal; changing Tools that other agents select needs the agent catalog (manage_agents on the Node), because the write changes their configuration too (agents: {})", agents.join(", "))]
    SharedWithoutCatalog { agents: Vec<String> },
    #[error("operator-managed {} cannot be self-granted: the acting agent does not hold {}. Save Tools without it or ask the operator to grant it", grants.join(", "), if grants.len() == 1 { "it" } else { "them" })]
    GrantNotHeld { grants: Vec<String> },
    #[error("{source}")]
    LosesOwnControl {
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("this would drop existing settings from these Tools: {}. Send them back in the save, or allow the drop for {}", dropped.join(", "), groups.join(","))]
    DropsSettings {
        dropped: Vec<String>,
        groups: Vec<String>,
    },
    #[error("nothing was changed: these Tools would select plugin {plugin}, which is not installed on this node ({reason}). An agent whose Tools select a plugin that is not installed cannot load its tools and fails its next request. Install the pack that ships {plugin}, then select it")]
    PluginNotInstalled { plugin: String, reason: String },
    #[error(transparent)]
    Document(#[from] DocumentOperationError),
    #[error(transparent)]
    Delete(#[from] DocumentDeleteError),
}

impl ToolsOperationError {
    pub fn recovery(&self) -> NextCall {
        match self {
            Self::SharedUnacknowledged { .. } | Self::DropsSettings { .. } => NextCall::VerbHelp {
                resource: RESOURCE,
                verb: "update",
            },
            Self::SharedWithoutCatalog { .. } => NextCall::Inspect { resource: "agent" },
            Self::GrantNotHeld { .. } | Self::LosesOwnControl { .. } => {
                NextCall::Inspect { resource: RESOURCE }
            }
            Self::PluginNotInstalled { .. } => NextCall::List { resource: "pack" },
            Self::Document(error) => error.recovery(RESOURCE),
            Self::Delete(error) => error.recovery(RESOURCE),
        }
    }

    /// Lean `SelfConfig.ToolsSave.Outcome`: which admission check refused.
    pub fn refusal(&self) -> Option<ToolsSaveOutcome> {
        match self {
            Self::SharedUnacknowledged { .. } | Self::SharedWithoutCatalog { .. } => {
                Some(ToolsSaveOutcome::RefusedShared)
            }
            Self::GrantNotHeld { .. } => Some(ToolsSaveOutcome::RefusedGrantNotHeld),
            Self::LosesOwnControl { .. } => Some(ToolsSaveOutcome::RefusedLosesOwnControl),
            Self::DropsSettings { .. } => Some(ToolsSaveOutcome::RefusedDropsSettings),
            Self::PluginNotInstalled { .. } => Some(ToolsSaveOutcome::RefusedPluginNotInstalled),
            Self::Document(_) | Self::Delete(_) => None,
        }
    }
}
