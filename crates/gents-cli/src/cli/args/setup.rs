use std::path::PathBuf;

use clap::ValueEnum;
use gents::backend_registry::DEFAULT_MAX_QUEUE_DEPTH;

use super::{BackendPresetArg, OpenAiWireApiArg, ToolPackageArg};

#[derive(clap::Args)]
pub(crate) struct ProvisionArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        value_name = "ROOT",
        help = "Portable manifest root to bind to this home and apply"
    )]
    pub(crate) root: PathBuf,
    #[arg(
        long,
        help = "Local display name and default key filename when the home has not been initialized. Defaults to the manifest root directory name."
    )]
    pub(crate) node_name: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Create a local file-key identity when the home is uninitialized. Production hosts should bootstrap identity first."
    )]
    pub(crate) bootstrap_file_identity: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Create/load a macOS Secure Enclave identity when the home is uninitialized."
    )]
    pub(crate) bootstrap_macos_secure_enclave: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Create/load a macOS login-keychain software identity when the home is uninitialized."
    )]
    pub(crate) bootstrap_macos_keychain: bool,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for the macOS keychain identity."
    )]
    pub(crate) keychain_label: Option<String>,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for the macOS Secure Enclave identity."
    )]
    pub(crate) secure_enclave_label: Option<String>,
    /// Keychain on macOS and a file elsewhere when omitted. Tests pass
    /// `file` so they never write login-keychain items.

    #[arg(long, value_enum, hide = true)]
    pub(crate) store_key_custody: Option<StoreKeyCustodyArg>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum IdentityBackendArg {
    File,
    MacosKeychain,
    MacosSecureEnclave,
}

/// Custody of a new home's store encryption key (`gents::store_key`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum StoreKeyCustodyArg {
    File,
    Keychain,
}

pub(crate) fn store_key_custody(
    arg: Option<StoreKeyCustodyArg>,
) -> gents::store_key::StoreKeyCustodyChoice {
    match arg {
        Some(StoreKeyCustodyArg::File) => gents::store_key::StoreKeyCustodyChoice::File,
        Some(StoreKeyCustodyArg::Keychain) => gents::store_key::StoreKeyCustodyChoice::Keychain,
        None => gents::store_key::StoreKeyCustodyChoice::default(),
    }
}

#[derive(clap::Args)]
pub(crate) struct InitArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Delete the existing home directory before re-initializing it"
    )]
    pub(crate) dangerously_overwrite: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Clear persisted local runtime state after initialization"
    )]
    pub(crate) reset: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Create/load identity and write init.json without seeding runtime config documents"
    )]
    pub(crate) identity_only: bool,
    #[arg(long, default_value = crate::DEFAULT_NODE_NAME, help = "Local display name and default key filename. The node DID is derived from the identity key.")]
    pub(crate) node_name: String,
    #[arg(long)]
    pub(crate) key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        default_value_t = IdentityBackendArg::File,
        help = "Local identity backend for init metadata."
    )]
    pub(crate) identity_backend: IdentityBackendArg,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for --identity-backend macos-keychain."
    )]
    pub(crate) keychain_label: Option<String>,
    #[arg(
        long,
        value_name = "LABEL",
        help = "Keychain label for --identity-backend macos-secure-enclave."
    )]
    pub(crate) secure_enclave_label: Option<String>,
    /// Keychain on macOS and a file elsewhere when omitted. Tests pass
    /// `file` so they never write login-keychain items.

    #[arg(long, value_enum, hide = true)]
    pub(crate) store_key_custody: Option<StoreKeyCustodyArg>,
    #[arg(
        long = "inference-url",
        value_name = "INFERENCE_URL",
        help = "Inference backend base URL, usually including /v1. Falls back to INFERENCE_ENDPOINT, then the local llama-server default."
    )]
    pub(crate) inference_endpoint: Option<String>,
    #[arg(
        long,
        help = "Optional backend document id. Defaults to <node-name>-backend"
    )]
    pub(crate) backend_id: Option<String>,
    #[arg(
        long,
        help = "Optional backend display name. Defaults to the backend id"
    )]
    pub(crate) backend_name: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "Backend preset with provider/auth defaults for common local and hosted backends"
    )]
    pub(crate) backend_preset: Option<BackendPresetArg>,
    #[arg(
        long,
        help = "Backend provider kind. OpenAiCompatible covers OpenAI-style local and hosted endpoints"
    )]
    pub(crate) provider_kind: Option<String>,
    #[arg(
        long,
        value_enum,
        help = "OpenAI-style wire API for OpenAiCompatible backends: responses or chat-completions"
    )]
    pub(crate) openai_wire_api: Option<OpenAiWireApiArg>,
    #[arg(long, help = "Raw API key stored directly in the backend document")]
    pub(crate) api_key: Option<String>,
    #[arg(long, help = "Environment variable name holding the backend API key")]
    pub(crate) api_key_env_var: Option<String>,
    #[arg(
        long,
        help = "Model id to bind to the default agent. Required for presets without a local default model"
    )]
    pub(crate) model_name: Option<String>,
    #[arg(long, help = "Override the backend's default concurrent request limit")]
    pub(crate) max_concurrent: Option<i64>,
    #[arg(long, default_value_t = DEFAULT_MAX_QUEUE_DEPTH)]
    pub(crate) max_queue_depth: i64,
    #[arg(
        long = "write",
        default_value_t = false,
        help = "Bootstrap write-capable tools, sandboxed and scoped to the tool root, instead of the safe read-only default"
    )]
    pub(crate) write_tools: bool,
    #[arg(
        long,
        default_value_t = false,
        conflicts_with = "write_tools",
        help = "Bootstrap write-capable tools with UNRESTRICTED bash: no sandbox, full host access as your user"
    )]
    pub(crate) yolo: bool,
    #[arg(
        long,
        value_enum,
        help = "Bootstrap a named tool package. Defaults to readonly; --write and --yolo are shorthands for the write and yolo packages"
    )]
    pub(crate) tool_package: Option<ToolPackageArg>,
    #[arg(
        long,
        default_value_t = false,
        help = "Seed the default agent as The Engineer: setup prompt and tag, the write package unless one is chosen, and inference off until a model is selected. Every init enables the self-config tools"
    )]
    pub(crate) engineer: bool,
    #[arg(
        long,
        help = "Root directory for local file/bash tools. Defaults to the current working directory"
    )]
    pub(crate) tool_root: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Enable the feature-gated per-agent memory tool in the default Tools"
    )]
    pub(crate) enable_memory: bool,
    #[arg(
        long,
        default_value_t = false,
        help = "Disable the read-only defra_query tool even when the tool package enables it (only the introspection package does; defra_query is otherwise opt-in)"
    )]
    pub(crate) disable_defra_query: bool,
    #[arg(
        long,
        default_value_t = false,
        conflicts_with = "disable_defra_query",
        help = "Enable the read-only defra_query tool regardless of the tool package (pair with --defra-query-collection to scope it, e.g. the \"agent-config\" preset)"
    )]
    pub(crate) enable_defra_query: bool,
    #[arg(
        long = "defra-query-collection",
        help = "Restrict the defra_query tool to these collections when a package enables it (repeatable); omit for all collections"
    )]
    pub(crate) defra_query_collections: Vec<String>,
}

impl InitArgs {
    pub(crate) fn resolved_inference_endpoint(&self) -> Option<&str> {
        self.inference_endpoint.as_deref()
    }

    pub(crate) fn preset_default_model_name(&self) -> Option<&'static str> {
        self.backend_preset
            .and_then(BackendPresetArg::default_model_name)
    }
}

#[derive(clap::Args)]
pub(crate) struct ResetArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
}
