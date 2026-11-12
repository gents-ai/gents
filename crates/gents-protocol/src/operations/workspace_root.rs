use std::error::Error;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::NextCall;
use super::StringUpdate;

pub const RESOURCE: &str = "workspace-root";

/// Lean `WorkspaceRootOperations.Stored`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredRoot {
    Absent,
    Enabled,
    Disabled,
}

/// Lean `WorkspaceRootOperations.Toggle`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnabledUpdate {
    #[default]
    Omitted,
    Enable,
    Disable,
}

impl From<Option<bool>> for EnabledUpdate {
    fn from(value: Option<bool>) -> Self {
        match value {
            None => Self::Omitted,
            Some(true) => Self::Enable,
            Some(false) => Self::Disable,
        }
    }
}

/// Lean `WorkspaceRootOperations.NameUpdate`: which of the three states a
/// patched `display_name` is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameUpdate {
    Omitted,
    Clear,
    Set,
}

impl From<&StringUpdate> for NameUpdate {
    fn from(value: &StringUpdate) -> Self {
        match value {
            StringUpdate::Omitted => Self::Omitted,
            StringUpdate::Clear => Self::Clear,
            StringUpdate::Set(_) => Self::Set,
        }
    }
}

/// Lean `WorkspaceRootOperations.enabledAfter`.
pub fn enabled_after(stored: StoredRoot, toggle: EnabledUpdate) -> bool {
    match (toggle, stored) {
        (EnabledUpdate::Enable, _) => true,
        (EnabledUpdate::Disable, _) => false,
        (EnabledUpdate::Omitted, StoredRoot::Disabled) => false,
        (EnabledUpdate::Omitted, StoredRoot::Absent | StoredRoot::Enabled) => true,
    }
}

/// Lean `WorkspaceRootOperations.writesDisplayName`.
pub fn writes_display_name(stored: StoredRoot, name: NameUpdate) -> bool {
    !matches!(
        (name, stored),
        (
            NameUpdate::Omitted,
            StoredRoot::Enabled | StoredRoot::Disabled
        )
    )
}

/// Lean `WorkspaceRootOperations.ReaderSubject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderSubject {
    EnabledAgent,
    DisabledAgent,
    UnknownActor,
    NodeActor,
}

/// Lean `WorkspaceRootOperations.ReaderChange`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderChange {
    Grant,
    Revoke,
}

/// Lean `WorkspaceRootOperations.ReaderDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderDecision {
    Admitted,
    AgentUnavailable,
    NodeActor,
}

/// Lean `WorkspaceRootOperations.readerDecision`.
pub fn reader_decision(change: ReaderChange, subject: ReaderSubject) -> ReaderDecision {
    match (subject, change) {
        (ReaderSubject::NodeActor, _) => ReaderDecision::NodeActor,
        (ReaderSubject::EnabledAgent, _) => ReaderDecision::Admitted,
        (_, ReaderChange::Revoke) => ReaderDecision::Admitted,
        (_, ReaderChange::Grant) => ReaderDecision::AgentUnavailable,
    }
}

/// Lean `WorkspaceRootOperations.Refusal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRootRefusal {
    PathNotAbsolute,
    RootMissing,
    ForeignRoot,
    NodeUnavailable,
    AgentUnavailable,
    NodeActor,
}

/// Lean `WorkspaceRootOperations.Recovery`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRootRecovery {
    VerbHelp,
    ListRoots,
    ListAgents,
    InspectNode,
}

impl WorkspaceRootRefusal {
    /// Lean `WorkspaceRootOperations.Refusal.recovery`.
    pub fn recovery(self) -> WorkspaceRootRecovery {
        match self {
            Self::PathNotAbsolute => WorkspaceRootRecovery::VerbHelp,
            Self::RootMissing | Self::ForeignRoot => WorkspaceRootRecovery::ListRoots,
            Self::NodeUnavailable => WorkspaceRootRecovery::InspectNode,
            Self::AgentUnavailable | Self::NodeActor => WorkspaceRootRecovery::ListAgents,
        }
    }
}

impl WorkspaceRootRecovery {
    pub fn next_call(self, verb: WorkspaceRootVerb) -> NextCall {
        match self {
            Self::VerbHelp => NextCall::VerbHelp {
                resource: RESOURCE,
                verb: verb.as_str(),
            },
            Self::ListRoots => NextCall::List { resource: RESOURCE },
            Self::ListAgents => NextCall::List { resource: "agent" },
            Self::InspectNode => NextCall::Inspect { resource: "node" },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceRootVerb {
    Publish,
    GrantReader,
    RevokeReader,
    Remove,
    Show,
}

impl WorkspaceRootVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::GrantReader => "grant-reader",
            Self::RevokeReader => "revoke-reader",
            Self::Remove => "remove",
            Self::Show => "show",
        }
    }
}

/// Publishes a root for a Node, or patches the root already published at the
/// path. Omitted fields of an existing root are preserved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceRootPublish {
    pub node_did: String,
    /// Absolute; normalized by [`normalize_root_path`] before it is compared
    /// or stored. The directory need not exist yet.
    pub root_path: String,
    pub display_name: StringUpdate,
    pub enabled: EnabledUpdate,
    /// Agents granted reader in the same transaction. A publication never
    /// revokes: an Agent absent from this list keeps the relation it has.
    pub reader_agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceRootReceipt {
    pub doc_id: String,
    pub root_path: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub created: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub granted_agents: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRootReaderInput {
    pub root_path: String,
    pub agent_did: String,
    pub change: ReaderChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceRootReaderReceipt {
    pub root_path: String,
    pub agent_did: String,
    pub change: ReaderChange,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkspaceRootRemoved {
    pub root_path: String,
    pub doc_id: String,
}

/// A published root as its readers see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRootView {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub root_path: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(
        default = "enabled_when_absent",
        deserialize_with = "enabled_unless_false"
    )]
    pub enabled: bool,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn enabled_when_absent() -> bool {
    true
}

fn enabled_unless_false<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(deserializer)?.unwrap_or(true))
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRootError {
    #[error("workspace root path {0:?} is not absolute; the directory need not exist yet, but its path must start at the filesystem root")]
    PathNotAbsolute(String),
    #[error("no published workspace root at {0:?} is visible to this actor")]
    RootMissing(String),
    #[error("the workspace root at {0:?} belongs to another Node")]
    ForeignRoot(String),
    #[error("Node {0:?} is missing or not visible to this actor")]
    NodeUnavailable(String),
    #[error(
        "agent {0:?} is missing or disabled; a root reader is an enabled Agent of the root's Node"
    )]
    AgentUnavailable(String),
    #[error("the Node actor cannot be a root reader; name the Agent that runs in the root")]
    NodeActor,
    /// Another owner's failure (identity, the store, ACP), unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl WorkspaceRootError {
    pub fn refusal(&self) -> Option<WorkspaceRootRefusal> {
        match self {
            Self::PathNotAbsolute(_) => Some(WorkspaceRootRefusal::PathNotAbsolute),
            Self::RootMissing(_) => Some(WorkspaceRootRefusal::RootMissing),
            Self::ForeignRoot(_) => Some(WorkspaceRootRefusal::ForeignRoot),
            Self::NodeUnavailable(_) => Some(WorkspaceRootRefusal::NodeUnavailable),
            Self::AgentUnavailable(_) => Some(WorkspaceRootRefusal::AgentUnavailable),
            Self::NodeActor => Some(WorkspaceRootRefusal::NodeActor),
            Self::Passthrough(_) => None,
        }
    }

    /// The call that lets the caller correct this failure of `verb`.
    pub fn recovery(&self, verb: WorkspaceRootVerb) -> NextCall {
        self.refusal()
            .map_or(
                WorkspaceRootRecovery::VerbHelp,
                WorkspaceRootRefusal::recovery,
            )
            .next_call(verb)
    }

    pub fn passthrough(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        match self {
            Self::Passthrough(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

/// The one spelling of a root path: absolute, with `.` and `..` resolved
/// lexically. Symlinks are not resolved and the path need not exist, so an
/// operator can publish a root before creating it.
pub fn normalize_root_path(path: &str) -> Result<String, WorkspaceRootError> {
    let given = Path::new(path);
    if !given.is_absolute() {
        return Err(WorkspaceRootError::PathNotAbsolute(path.to_owned()));
    }
    let mut normalized = PathBuf::new();
    for component in given.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    Ok(normalized
        .to_str()
        .expect("components of a str path are str")
        .to_owned())
}
