use std::collections::BTreeMap;
use std::error::Error;

use serde::{Deserialize, Serialize};

use super::NextCall;

/// Lean `Pack.Operations.Actor`: who signs a pack operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackActor {
    User,
    Agent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackOperation {
    Install,
    Update,
    Remove,
    Wire,
}

impl PackOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Update => "update",
            Self::Remove => "remove",
            Self::Wire => "wire",
        }
    }

    pub fn for_verb(verb: &str) -> Option<Self> {
        [Self::Install, Self::Update, Self::Remove, Self::Wire]
            .into_iter()
            .find(|operation| operation.as_str() == verb)
    }
}

/// Lean `Pack.Operations.Edited`: what a change does with documents edited
/// since the pack was installed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum EditedDocuments {
    #[default]
    Refuse,
    Overwrite,
    Keep,
}

impl EditedDocuments {
    /// The decision a caller names. Absent is `Refuse`.
    pub fn decision(word: Option<&str>) -> Result<Self, PackOperationError> {
        match word {
            None => Ok(Self::Refuse),
            Some("overwrite") => Ok(Self::Overwrite),
            Some("keep") => Ok(Self::Keep),
            Some(other) => Err(PackOperationError::EditedWord(other.to_owned())),
        }
    }
}

/// Lean `Pack.Operations.Digest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestExpectation {
    Absent,
    Matching,
    Differing,
}

impl DigestExpectation {
    pub fn of(expected: Option<&str>, resolved: &str) -> Self {
        match expected {
            None => Self::Absent,
            Some(expected) if expected == resolved => Self::Matching,
            Some(_) => Self::Differing,
        }
    }
}

/// Lean `Pack.Operations.DigestAdmission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestAdmission {
    Admitted,
    PreviewRequired,
    Changed,
}

/// Lean `Pack.Operations.EditedAdmission`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditedAdmission {
    Admitted,
    DecisionRequired,
}

/// Lean `Pack.Operations.VariableSource`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableSource {
    Scoped,
    Shared,
    Environment,
    Unset,
}

/// Lean `Pack.Operations.RootReader`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootReader {
    NotNeeded,
    Published,
    Unpublished,
}

/// Lean `Pack.Operations.ReaderAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderAction {
    None,
    Grant,
    ReportGrantStep,
    ReportPublishStep,
}

/// The install variables a caller supplies: `NAME=VALUE` for every pack of
/// the install, `NAMESPACE/PACK:NAME=VALUE` for that pack alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackVariables {
    pub shared: BTreeMap<String, String>,
    pub scoped: BTreeMap<String, BTreeMap<String, String>>,
}

impl PackVariables {
    pub fn is_empty(&self) -> bool {
        self.shared.is_empty() && self.scoped.is_empty()
    }

    pub fn len(&self) -> usize {
        self.shared.len() + self.scoped.values().map(BTreeMap::len).sum::<usize>()
    }
}

/// One requested install, update or inspection of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackChangeInput {
    pub package: String,
    pub variables: PackVariables,
    /// Declared inference slot to profile ID.
    pub inference_slots: BTreeMap<String, String>,
    pub expected_digest: Option<String>,
}

impl PackChangeInput {
    pub fn of(package: &str) -> Self {
        Self {
            package: package.to_owned(),
            variables: PackVariables::default(),
            inference_slots: BTreeMap::new(),
            expected_digest: None,
        }
    }
}

/// Which admission check refused a pack operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackRefusal {
    PreviewRequired,
    DigestChanged,
    EditedUndecided,
    InvalidInput,
}

impl PackRefusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PreviewRequired => "preview_required",
            Self::DigestChanged => "digest_changed",
            Self::EditedUndecided => "edited_undecided",
            Self::InvalidInput => "invalid_input",
        }
    }
}

/// A preview's answer: the receipt the commit would return, or the refusal it
/// would raise with that error's message and next call.
#[derive(Debug, Clone, PartialEq)]
pub enum PackOutcome<R> {
    Admitted(R),
    Refused {
        refusal: PackRefusal,
        message: String,
        next_call: NextCall,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum PackOperationError {
    #[error("pack name must not be blank")]
    PackageBlank,
    #[error("pack {} writes the artifact a preview pinned, and this call names no digest; nothing was written. Preview it, then repeat the call with the digest the preview returns", .0.as_str())]
    PreviewRequired(PackOperation),
    #[error("the pack's digest is {resolved}, not the expected {expected}; nothing was written; preview again, then repeat the call with what it reports")]
    DigestChanged { expected: String, resolved: String },
    #[error(
        "nothing was changed: {} of {coordinate} {} edited since it was installed. Overwrite {}; keep {}",
        .documents.join(", "),
        if .documents.len() == 1 { "was" } else { "were" },
        if *.removal { "deletes them with the rest of the pack" } else { "replaces them with the pack's versions and the live edits are lost" },
        if *.removal { "leaves them on the node and removes the rest of the pack" } else { "leaves them as edited and installs the rest of the pack" }
    )]
    EditedUndecided {
        coordinate: String,
        documents: Vec<String>,
        removal: bool,
    },
    #[error("the edited decision is \"overwrite\" or \"keep\", not {0:?}; a preview says what each does")]
    EditedWord(String),
    #[error("install variable {0:?} must be NAME=VALUE, or NAMESPACE/PACK:NAME=VALUE for one pack of the install")]
    VariableMalformed(String),
    #[error("invalid or protected install variable name {0:?}; a name is upper-case letters, digits and underscores, at most 128 bytes")]
    VariableName(String),
    #[error("inference variable {0:?} is unsupported; bind declared inference slots to profile IDs instead")]
    VariableInference(String),
    #[error("install variable {0:?} is larger than 4096 bytes")]
    VariableTooLarge(String),
    #[error("install variable {0:?} was provided more than once")]
    VariableDuplicate(String),
    #[error("install variables exceed the 32-entry limit")]
    TooManyVariables,
    #[error("inference slot bindings exceed the 32-entry limit")]
    TooManySlots,
    #[error("invalid inference slot binding {0:?}; expected NAME=PROFILE_ID with neither blank")]
    SlotMalformed(String),
    #[error("inference slot {0:?} was bound more than once")]
    SlotDuplicate(String),
    /// Another owner's failure (the pack engine, the registry, the store),
    /// unchanged.
    #[error(transparent)]
    Passthrough(Box<dyn Error + Send + Sync>),
}

impl PackOperationError {
    /// The call that lets the caller correct this failure of `operation`.
    pub fn recovery(&self, operation: PackOperation) -> NextCall {
        match self {
            Self::PreviewRequired(_)
            | Self::DigestChanged { .. }
            | Self::EditedUndecided { .. } => NextCall::Preview {
                resource: "pack",
                verb: operation.as_str(),
            },
            Self::PackageBlank => NextCall::List { resource: "pack" },
            Self::EditedWord(_)
            | Self::VariableMalformed(_)
            | Self::VariableName(_)
            | Self::VariableInference(_)
            | Self::VariableTooLarge(_)
            | Self::VariableDuplicate(_)
            | Self::TooManyVariables
            | Self::TooManySlots
            | Self::SlotMalformed(_)
            | Self::SlotDuplicate(_)
            | Self::Passthrough(_) => NextCall::VerbHelp {
                resource: "pack",
                verb: operation.as_str(),
            },
        }
    }

    pub fn refusal(&self) -> Option<PackRefusal> {
        match self {
            Self::PreviewRequired(_) => Some(PackRefusal::PreviewRequired),
            Self::DigestChanged { .. } => Some(PackRefusal::DigestChanged),
            Self::EditedUndecided { .. } => Some(PackRefusal::EditedUndecided),
            Self::PackageBlank
            | Self::EditedWord(_)
            | Self::VariableMalformed(_)
            | Self::VariableName(_)
            | Self::VariableInference(_)
            | Self::VariableTooLarge(_)
            | Self::VariableDuplicate(_)
            | Self::TooManyVariables
            | Self::TooManySlots
            | Self::SlotMalformed(_)
            | Self::SlotDuplicate(_) => Some(PackRefusal::InvalidInput),
            Self::Passthrough(_) => None,
        }
    }

    /// An admission refusal becomes the successful refused answer of a
    /// preview; any other failure stays an error.
    pub fn preview_outcome<R>(self, operation: PackOperation) -> Result<PackOutcome<R>, Self> {
        match self.refusal() {
            Some(refusal) => Ok(PackOutcome::Refused {
                refusal,
                next_call: self.recovery(operation),
                message: crate::error_chain::ErrorChain(&self).to_string(),
            }),
            None => Err(self),
        }
    }

    /// The other owner's failure a `Passthrough` carries. `source()` skips it.
    pub fn passthrough(&self) -> Option<&(dyn Error + Send + Sync + 'static)> {
        match self {
            Self::Passthrough(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}
