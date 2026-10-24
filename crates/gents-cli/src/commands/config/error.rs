use std::path::PathBuf;

use gents::backend_registry::BackendRegistryError;
use gents::document_config::DocumentConfigError;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigCommandError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
    #[error(transparent)]
    Validate(Box<DocumentConfigError>),
    #[error(transparent)]
    Backend(#[from] BackendRegistryError),
    #[error("config owner missing")]
    ConfigOwnerMissing,
    #[error("manifest contains concrete node DID(s) that do not match resolved runtime DID {target_did}: {dids}; pass --force-rebind-concrete-did to rebind them")]
    ConcreteNodeDidMismatch { target_did: String, dids: String },
    #[error("target owner must not be blank")]
    RebindTargetBlank,
    #[error("decoding canonical InferenceProfile document")]
    DecodeInferenceProfile(#[source] serde_json::Error),
    #[error("account {label:?} has no backend; signing in again does not restore it, so create one with `gents config backend set`")]
    AccountNoBackendAfterSignIn { label: String },
    #[error("account {label:?} has no backend; create one with `gents config backend set`")]
    AccountWithoutBackend { label: String },
    #[error("account {label:?} runs several backends: {backends}; name one as backend_id in the profile file")]
    AccountRunsSeveralBackends { label: String, backends: String },
    #[error(transparent)]
    ManifestRoot(#[from] crate::desired_state::write::WriteError),
    #[error("--path must be valid UTF-8 to be stored as a root_path, got {}", path.display())]
    PathNotUtf8 { path: PathBuf },
    #[error("expected one WorkspaceRoot for {id:?}, found {found}")]
    WorkspaceRootNotUnique { id: String, found: usize },
    #[error("no WorkspaceRoot document with root_path {root_path:?}")]
    WorkspaceRootMissing { root_path: String },
    #[error("desired-state manifest validation failed")]
    ManifestValidationFailed,
    #[error("node deletion is not a config document removal")]
    NodeRemovalNotDocument,
    #[error("--path must be an absolute path, got {}; the workspace root does not need to exist yet, but its path must be absolute", path.display())]
    WorkspaceRootPathNotAbsolute { path: PathBuf },
    #[error("parsing stored interface_json")]
    InterfaceJson(#[source] serde_json::Error),
    #[error("--session-id must not be empty")]
    SessionIdBlank,
    #[error("Task {task_id} declares a durable goal; pass --session-id with a stable invocation identity so retries converge")]
    GoalSessionIdRequired { task_id: String },
    #[error("conflicting task ids provided: positional={positional} and --task-id={flag}\nNext:\n  1. Pass the task id once: `gents task {command} TASK_ID`\n  2. Or use `--task-id TASK_ID`, but not both")]
    ConflictingTaskIds {
        positional: String,
        flag: String,
        command: String,
    },
    #[error("missing task id\nNext:\n  1. Pass it positionally: `gents task {command} TASK_ID`\n  2. Or use `--task-id TASK_ID`")]
    MissingTaskId { command: String },
}

impl From<DocumentConfigError> for ConfigCommandError {
    fn from(error: DocumentConfigError) -> Self {
        Self::Validate(Box::new(error))
    }
}
