use std::path::PathBuf;

use gents::backend_registry::BackendRegistryError;
use gents::config_client::DesiredStateError;
use gents::document_config::DocumentConfigError;

use crate::config_bundle::ConfigBundleError;
use crate::request_helpers::RequestHelpersError;

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
    #[error(transparent)]
    DesiredStatePlan(#[from] DesiredStateError),
    #[error(transparent)]
    ConfigBundle(#[from] ConfigBundleError),
    #[error(transparent)]
    RequestHelpers(#[from] RequestHelpersError),
    #[error("decoding canonical InferenceBackend document")]
    DecodeInferenceBackend(#[source] serde_json::Error),
    #[error("decoding canonical Tools document")]
    DecodeTools(#[source] serde_json::Error),
    #[error("validated manifest root produced no manifest")]
    ValidatedManifestMissing,
    #[error("no init config found in {}; run `gents init` first", home.display())]
    NoInitConfig { home: PathBuf },
    #[error("ambiguous {collection} {id:?}: {rows} live rows resolve this logical id")]
    AmbiguousLogicalId {
        collection: &'static str,
        id: String,
        rows: usize,
    },
    #[error("missing selected datastore surface {id:?}")]
    MissingSelectedSurface { id: String },
    #[error("initialized home metadata is required for --bind-node-did home; run `gents init --identity-only --home {}` first", home.display())]
    BindHomeNotInitialized { home: PathBuf },
    #[error("initialized home {} has node DID {init_did}, but identity key resolves to {key_did}; rerun `gents init --identity-only` or repair the home identity metadata", home.display())]
    HomeKeyDidMismatch {
        home: PathBuf,
        init_did: String,
        key_did: String,
    },
    #[error("initialized home {} does not contain an node DID or identity key path; rerun `gents init --identity-only`", home.display())]
    HomeWithoutIdentity { home: PathBuf },
    #[error("initialized home {} points to missing identity key {}; rerun `gents init --identity-only`", home.display(), key_path.display())]
    HomeKeyMissing { home: PathBuf, key_path: PathBuf },
    #[error("loading identity key {}", key_path.display())]
    LoadIdentityKey {
        key_path: PathBuf,
        #[source]
        source: gents::identity::IdentityError,
    },
    #[error("querying live node readiness for node DID")]
    QueryLiveReadiness(#[source] gents::config_client::ConfigClientError),
    #[error("live runtime did not publish active node readiness")]
    NoActiveReadiness,
    #[error("live node readiness row did not contain node_did")]
    ReadinessWithoutNodeDid,
    #[error("configuration query returned a foreign owner")]
    ForeignOwnerRow,
    #[error("configuration row has no logical ID")]
    RowWithoutLogicalId,
    #[error("ambiguous scoped configuration {collection} {id:?}")]
    AmbiguousScopedConfiguration {
        collection: &'static str,
        id: String,
    },
    #[error("reading instructions from {}", path.display())]
    ReadSkillInstructions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("no Skill document with skill_id {skill_id:?}")]
    SkillNotFound { skill_id: String },
    #[error("creating {}", path.display())]
    CreateSkillDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}/SKILL.md", dir.display())]
    WriteSkillMd {
        dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}/openai.yaml", dir.display())]
    WriteOpenAiYaml {
        dir: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("lookup Task fire request failed: {errors:?}")]
    TaskFireLookup { errors: Vec<serde_json::Value> },
    #[error("Task fire retry_key resolved to multiple AgentRequest rows")]
    TaskFireRetryKeyAmbiguous,
    #[error("decoding Task fire canonical AgentRequest row")]
    DecodeTaskFireRow(#[source] serde_json::Error),
    #[error("configuration query returned a different logical ID")]
    DifferentLogicalId,
    #[error("not found: {collection} {node_did:?}/{id:?}")]
    DocumentNotFound {
        collection: &'static str,
        node_did: String,
        id: String,
    },
    #[error(transparent)]
    ConfigClient(#[from] gents::config_client::ConfigClientError),
    #[error(transparent)]
    HomeState(#[from] crate::home_state::HomeStateError),
    #[error(transparent)]
    ToolSurface(#[from] gents::tool_surface::ToolSurfaceError),
    #[error(transparent)]
    Graphql(#[from] gents_protocol::graphql::GraphqlError),
    #[error(transparent)]
    PluginStore(#[from] gents::plugin::store::PluginStoreError),
    #[error(transparent)]
    RequestReceipt(#[from] gents::RequestReceiptError),
}

impl From<DocumentConfigError> for ConfigCommandError {
    fn from(error: DocumentConfigError) -> Self {
        Self::Validate(Box::new(error))
    }
}
