use std::error::Error;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::agent::NextCall;
use super::document::{DocumentOperationError, DocumentPatch};
use super::StringUpdate;

pub const RESOURCE: &str = "skill";

/// The edit of a Skill. An omitted field keeps its stored value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillEdit {
    pub name: StringUpdate,
    pub description: StringUpdate,
    pub instructions: StringUpdate,
    pub display_name: StringUpdate,
    /// Replaces the whole list; an empty list clears it.
    pub tool_refs: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

impl SkillEdit {
    pub fn enabled(enabled: bool) -> Self {
        Self {
            enabled: Some(enabled),
            ..Self::default()
        }
    }

    pub fn patch(&self) -> DocumentPatch {
        let mut patch = DocumentPatch::new();
        for (field, update) in [
            ("name", &self.name),
            ("description", &self.description),
            ("instructions", &self.instructions),
            ("display_name", &self.display_name),
        ] {
            match update {
                StringUpdate::Omitted => {}
                StringUpdate::Clear => patch.push((field.to_owned(), None)),
                StringUpdate::Set(value) => patch.push((field.to_owned(), Some(json!(value)))),
            }
        }
        if let Some(tool_refs) = &self.tool_refs {
            patch.push((
                "tool_refs".to_owned(),
                (!tool_refs.is_empty()).then(|| json!(tool_refs)),
            ));
        }
        if let Some(enabled) = self.enabled {
            patch.push(("enabled".to_owned(), Some(Value::Bool(enabled))));
        }
        patch
    }
}

/// Lean `DocumentOperations.ImportOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillImportOutcome {
    Created,
    Replaced,
    RefusedOutsideRoot,
    RefusedExists,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillOperationError {
    #[error("skill {skill_id:?} already exists; importing over it replaces its instructions and tool references, so it takes an explicit overwrite")]
    ImportExists { skill_id: String },
    #[error("skill source {path:?} is outside this agent's tool root; an agent imports only from inside its root")]
    OutsideToolRoot {
        path: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("skill import requires effective file read permission on the invoking agent")]
    FileReadNotGranted,
    #[error("skill import requires an effective tool root")]
    ToolRootMissing,
    #[error("skill source {path:?} could not be read: {source}")]
    Source {
        path: String,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error(transparent)]
    Document(#[from] DocumentOperationError),
}

impl SkillOperationError {
    pub fn recovery(&self) -> NextCall {
        match self {
            Self::ImportExists { .. } | Self::Source { .. } => NextCall::VerbHelp {
                resource: RESOURCE,
                verb: "import",
            },
            Self::OutsideToolRoot { .. } | Self::FileReadNotGranted | Self::ToolRootMissing => {
                NextCall::Inspect { resource: "tools" }
            }
            Self::Document(error) => error.recovery(RESOURCE),
        }
    }

    /// Lean `DocumentOperations.ImportOutcome`: which import refusal this is.
    pub fn import_refusal(&self) -> Option<SkillImportOutcome> {
        match self {
            Self::ImportExists { .. } => Some(SkillImportOutcome::RefusedExists),
            Self::OutsideToolRoot { .. } => Some(SkillImportOutcome::RefusedOutsideRoot),
            _ => None,
        }
    }
}
