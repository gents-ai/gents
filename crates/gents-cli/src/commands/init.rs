use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents::document_config::{
    Agent, AgentContext, AgentTools, BackendAuth, BashTools, BuiltInTools,
    DatastoreToolSurfaceDocument, DatastoreTools, FileTools, HostTools, InferenceBackend,
    InferenceExecution, SurfaceToolDecl, Tools,
};
use gents::self_config::engineer_self_config;
use gents::{
    default_agent_id_for_node, default_inference_profile_id_for_agent, load_agent, load_node,
    load_or_create_macos_keychain_identity, load_or_create_macos_secure_enclave_identity, BashMode,
    Collection, CommandExecutionMode, FileToolMode, InferenceProfile, KeyIdentity, NodeIdentity,
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::*;
use crate::config_writes::ConfigAccess;
use crate::shared::*;
use crate::{
    clear_runtime_state, dangerously_overwrite_home, default_data_dir, default_key_path,
    format_tool_ceiling, format_tool_package, normalize_optional_string, print_json,
    read_init_config, resolve_home_dir, write_init_config, BackendResolutionMode,
    DEFAULT_HTTP_PORT,
};

const STANDARD_READONLY_SYSTEM_PROMPT: &str = r#"You are a terminal-native engineering and operations agent running for the user inside a local DefraDB runtime.

Your job is to help with software work, debugging, codebase inspection, incident triage, release checks, infrastructure investigation, and general computer operations tasks. Build your conclusions from real evidence: inspect files, logs, command output, and tool results before making claims.

Work like a strong command-line operator:
- be concise and factual
- prefer direct answers over long essays
- explain what you found, not what you assume
- propose the next command, file, or check when it helps

You are currently in a read-only operating mode for local tools. You can inspect local state, but you cannot modify files or perform write-capable shell actions. If the user asks for a change, say clearly that the current tool mode is read-only and describe the exact edit or command you would apply if write access were enabled."#;
const STANDARD_READWRITE_SYSTEM_PROMPT: &str = r#"You are a terminal-native engineering and operations agent running for the user inside a local DefraDB runtime.

Your job is to help with software work, debugging, code changes, codebase maintenance, incident triage, release checks, infrastructure investigation, and general computer operations tasks. Build your conclusions from real evidence: inspect files, logs, command output, and tool results before making claims.

Work like a strong command-line operator:
- inspect first, then act
- keep changes focused and easy to explain
- prefer direct answers over long essays
- summarize exactly what changed and why
- avoid broad or risky operations unless the user clearly wants them

You have write-capable local tools. When the user asks you to make a change, you may edit files and use write-capable shell actions deliberately. Read the relevant state first, make the smallest effective change, and report the concrete outcome.

For long-running commands such as builds, test suites, installs, servers, and log tails, prefer spawn_process with tool_name "bash_unrestricted" instead of shell backgrounding with "&". Use list_processes, read_process, wait_process, or cancel_process to inspect, finish, or stop backgrounded work."#;

const ENGINEER_SYSTEM_PROMPT: &str = gents_protocol::ENGINEER_PROMPT;

const YOLO_WARNING: &str = "\
WARNING: --yolo bootstraps UNRESTRICTED tools. The agent can run any command\n\
and write any file your user account can reach — no sandbox, no containment.\n\
Use --write for sandboxed writes scoped to the tool root.";

/// Takes the store lock for the rest of init; an overwrite wipes the home
/// under that same lock, keeping the lock file itself, so no runtime can open
/// the store while it is removed or hold a second lock afterwards.
pub(crate) fn lock_init_store(
    home_dir: &Path,
    data_dir: &Path,
    overwrite: bool,
) -> Result<gents::home::StoreLock> {
    let user_home = std::env::home_dir();
    lock_init_store_for_user(home_dir, data_dir, overwrite, user_home.as_deref())
}

pub(crate) fn lock_init_store_for_user(
    home_dir: &Path,
    data_dir: &Path,
    overwrite: bool,
    user_home: Option<&Path>,
) -> Result<gents::home::StoreLock> {
    let home = if overwrite {
        crate::overwritable_home(home_dir, user_home)?
    } else {
        None
    };
    let Some(home) = home else {
        fs::create_dir_all(data_dir)
            .with_context(|| format!("creating data directory {}", data_dir.display()))?;
        return Ok(gents::home::lock_store(home_dir, data_dir)?);
    };
    let data_dir = overwrite_data_dir(home_dir, &home, data_dir)?;
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let lock = gents::home::lock_store(home_dir, &data_dir)?;
    let overwritten_encryption = read_init_config(&home)
        .ok()
        .flatten()
        .and_then(|config| config.store_encryption)
        .or_else(|| {
            gents::store_key::upgrade::pending_record(&data_dir)
                .ok()
                .flatten()
        });
    dangerously_overwrite_home(&home, lock.path())?;
    if let Some(record) = overwritten_encryption {
        if let Err(error) = record.delete_key(&gents::store_key::home_key_file(&home)) {
            tracing::warn!(home = %home.display(), error = %gents_protocol::error_chain::ErrorChain(&error),
                "could not remove the overwritten home's store key");
        }
    }
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    Ok(lock)
}

/// The data directory an overwrite recreates, on the resolved home. A data
/// directory reached through a symlink or `..` inside the home would be
/// replaced by a different directory than the one locked, so it is refused;
/// one outside the home is not wiped and keeps its lock.
fn overwrite_data_dir(home_dir: &Path, home: &Path, data_dir: &Path) -> Result<PathBuf> {
    use std::path::Component;

    let refuse = || {
        anyhow::anyhow!(
            "refusing to dangerously overwrite {}: its data directory {} is reached through a symlink or `..`; remove the link or pass a plain --data-dir",
            home_dir.display(),
            data_dir.display()
        )
    };
    let Ok(relative) = data_dir.strip_prefix(home_dir) else {
        if fs::canonicalize(data_dir).is_ok_and(|resolved| resolved.starts_with(home)) {
            return Err(refuse());
        }
        return Ok(data_dir.to_path_buf());
    };
    let mut resolved = home.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(name) => resolved.push(name),
            Component::CurDir => continue,
            _ => return Err(refuse()),
        }
        if fs::symlink_metadata(&resolved).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(refuse());
        }
    }
    Ok(resolved)
}

pub(crate) async fn init(args: InitArgs) -> Result<()> {
    print_json(&init_home(args).await?)
}

/// Normal completion drains the embedded node before releasing its store claim;
/// aborting background tasks without awaiting them can retain the database.
async fn init_home(mut args: InitArgs) -> Result<Value> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let tool_package = resolve_initial_tool_package(&args)?;
    validate_init_tool_flags(&args, tool_package)?;
    if matches!(tool_package, ToolPackageArg::Yolo) {
        eprintln!("{YOLO_WARNING}");
    }
    crate::interactive_backend::resolve_backend_interactively(&mut args).await?;
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| default_data_dir(&home_dir));
    let store_lock = lock_init_store(&home_dir, &data_dir, args.dangerously_overwrite)?;

    if args.identity_only {
        if args.identity_backend != IdentityBackendArg::File && args.key_path.is_some() {
            anyhow::bail!("--key-path cannot be used with non-file identity backends");
        }
        let key_path = (args.identity_backend == IdentityBackendArg::File).then(|| {
            args.key_path
                .clone()
                .unwrap_or_else(|| default_key_path(&home_dir, &args.node_name))
        });
        let summary = write_identity_only_home_metadata(IdentityOnlyHomeOptions {
            home: &home_dir,
            node_name: &args.node_name,
            key_path: key_path.as_deref(),
            identity_backend: args.identity_backend,
            keychain_label: args.keychain_label.as_deref(),
            secure_enclave_label: args.secure_enclave_label.as_deref(),
            tool_package,
            tool_root: args.tool_root.as_deref(),
            reset: args.reset,
            store_key_custody: crate::cli::args::store_key_custody(args.store_key_custody),
        })
        .await?;
        let output = json!({
            "status": "initialized",
            "identity_only": true,
            "home": summary.home,
            "node_name": summary.node_name,
            "node_did": summary.node_did,
            "key_path": summary.key_path,
            "tool_package": format_tool_package(summary.tool_package),
            "tool_ceiling": format_tool_ceiling(summary.tool_ceiling),
            "tool_root": summary.tool_root,
            "runtime_state_reset": summary.runtime_state_reset,
            "identity": {
                "node_did": summary.node_did,
                "key_path": summary.key_path,
                "identity_backend": summary.identity_backend,
                "keychain_label": summary.keychain_label,
                "secure_enclave_label": summary.secure_enclave_label,
                "permission_boundary": "This DID and key identify the permission boundary for every action the node runtime performs."
            },
            "next_steps": [
                "gents config apply --root <manifest-root> --home <home> --bind-node-did home",
                "gents server"
            ],
            "init": null
        });
        return Ok(output);
    }

    let initialized_identity = load_or_create_home_identity(HomeIdentityOptions {
        home: &home_dir,
        node_name: &args.node_name,
        key_path: args.key_path.as_deref(),
        identity_backend: args.identity_backend,
        keychain_label: args.keychain_label.as_deref(),
        secure_enclave_label: args.secure_enclave_label.as_deref(),
    })?;
    initialized_identity
        .identity
        .sign(b"gents init identity")
        .await
        .context("creating or loading node identity key")?;

    gents::storage_backend::reject_legacy_store(&data_dir)?;
    let mut stored = StoredInitConfig {
        home: home_dir.to_string_lossy().to_string(),
        node_name: args.node_name.clone(),
        node_did: initialized_identity.identity.did().to_string(),
        key_path: initialized_identity.key_path.clone(),
        identity_backend: initialized_identity.identity_backend.clone(),
        keychain_label: initialized_identity.keychain_label.clone(),
        secure_enclave_label: initialized_identity.secure_enclave_label.clone(),
        tool_package: Some(tool_package),
        tool_ceiling: tool_ceiling_for_package(tool_package),
        tool_root: resolve_tool_root_for_package(tool_package, args.tool_root.as_deref())?
            .map(|path| path.to_string_lossy().to_string()),
        store_encryption: None,
    };
    let store_key = initialize_home_store(
        &home_dir,
        &data_dir,
        crate::cli::args::store_key_custody(args.store_key_custody),
        &mut stored,
    )
    .await?;
    let mut node_builder = gents::store_key::persistent_builder(&data_dir, &store_key)?;
    if let Some(node_identity_did) = initialized_identity.node_identity_did.as_ref() {
        node_builder = node_builder.with_node_identity_did(node_identity_did.clone());
    }
    let node_arc = std::sync::Arc::new(
        node_builder
            .build()
            .await
            .context("building embedded DefraDB node for init")?,
    );
    let access = ConfigAccess::claimed_local(node_arc, store_lock)?;
    let init_node = access
        .local_node()
        .context("init requires claimed local DefraDB access")?
        .clone();
    let result = async {
        gents::migration::ensure_all_runtime_migrations(init_node.clone()).await?;
        gents::store_key::upgrade::finish(&data_dir)?;
        let summary = initialize_runtime_home(
            &access,
            &args,
            initialized_identity.identity.did(),
            tool_package,
        )
        .await?;
        write_init_config(&home_dir, &stored)?;
        let runtime_state_reset = if args.reset {
            clear_runtime_state(&home_dir)?
        } else {
            false
        };

        let codex_login =
            maybe_inline_codex_login(&access, initialized_identity.identity.did(), &summary).await;
        let grok_login =
            maybe_inline_grok_login(&access, initialized_identity.identity.did(), &summary).await;
        let claude_login =
            maybe_inline_claude_login(&access, initialized_identity.identity.did(), &summary).await;

        let output = json!({
            "status": "initialized",
            "home": home_dir,
            "node_name": args.node_name,
            "node_did": initialized_identity.identity.did(),
            "key_path": initialized_identity.key_path,
            "identity_backend": initialized_identity.identity_backend,
            "keychain_label": initialized_identity.keychain_label,
            "secure_enclave_label": initialized_identity.secure_enclave_label,
            "default_agent_id": summary.default_agent_id,
            "tools_id": summary.tools_id,
            "wide_open_preset_id": summary.wide_open_preset_id,
            "inference_profile_id": summary.inference_profile_id,
            "tool_package": format_tool_package(summary.tool_package),
            "tool_ceiling": format_tool_ceiling(summary.tool_ceiling),
            "tool_root": summary.tool_root,
            "enable_memory": summary.enable_memory,
            "enable_defra_query": summary.enable_defra_query,
            "defra_query_collections": summary.defra_query_collections,
            "runtime_state_reset": runtime_state_reset,
            "identity": {
                "node_did": initialized_identity.identity.did(),
                "key_path": stored.key_path,
                "identity_backend": stored.identity_backend,
                "keychain_label": stored.keychain_label,
                "secure_enclave_label": stored.secure_enclave_label,
                "permission_boundary": "This DID and key identify the permission boundary for every action the node runtime performs."
            },
            "codex_login": codex_login
                .outcome()
                .map(crate::commands::codex_login::codex_login_result_json),
            "grok_login": grok_login
                .outcome()
                .map(crate::commands::grok_login::grok_login_result_json),
            "claude_login": claude_login
                .outcome()
                .map(crate::commands::claude_login::claude_login_result_json),
            "next_steps": init_next_steps(
                &summary,
                codex_login.is_authenticated(),
                grok_login.is_authenticated(),
                claude_login.is_authenticated(),
            ),
            "init": summary,
        });
        Ok(output)
    }
    .await;
    init_node.shutdown().await;
    result
}

fn added_account_note(sign_in: &gents::oauth_credential::SignIn) -> Option<String> {
    (sign_in.result == gents::oauth_credential::SignInResult::Added
        && sign_in.credential.account_ref.is_some())
    .then(|| {
        format!(
            "This sign-in was stored as another account, {}. The backend set up here runs on \
             the provider's original account; see `gents accounts`.",
            gents::oauth_credential::effective_account_label(&sign_in.credential)
        )
    })
}

enum InlineCodexLoginState {
    Unauthenticated,
    ExistingCredential,
    Completed(Box<crate::commands::codex_login::CodexLoginOutcome>),
}

impl InlineCodexLoginState {
    fn is_authenticated(&self) -> bool {
        matches!(self, Self::ExistingCredential | Self::Completed(_))
    }

    fn outcome(&self) -> Option<&crate::commands::codex_login::CodexLoginOutcome> {
        match self {
            Self::Completed(outcome) => Some(outcome.as_ref()),
            Self::Unauthenticated | Self::ExistingCredential => None,
        }
    }
}

enum InlineGrokLoginState {
    Unauthenticated,
    ExistingCredential,
    Completed(Box<crate::commands::grok_login::GrokLoginOutcome>),
}

impl InlineGrokLoginState {
    fn is_authenticated(&self) -> bool {
        matches!(self, Self::ExistingCredential | Self::Completed(_))
    }

    fn outcome(&self) -> Option<&crate::commands::grok_login::GrokLoginOutcome> {
        match self {
            Self::Completed(outcome) => Some(outcome.as_ref()),
            Self::Unauthenticated | Self::ExistingCredential => None,
        }
    }
}

async fn maybe_inline_grok_login(
    access: &ConfigAccess,
    node_did: &str,
    summary: &InitSummary,
) -> InlineGrokLoginState {
    if summary.provider_kind != gents::BackendProviderKind::XaiGrokOAuth {
        return InlineGrokLoginState::Unauthenticated;
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return InlineGrokLoginState::Unauthenticated;
    }
    let provider = gents::xai_grok_oauth::normalize_provider("xai-oauth");
    match gents::oauth_credential::resolve_oauth_credential(
        access,
        node_did,
        &provider,
        gents::oauth_credential::AccountPick::Reference(None),
    )
    .await
    {
        Ok(Some(_)) => return InlineGrokLoginState::ExistingCredential,
        Ok(None) => {}
        Err(error) => {
            eprintln!("Could not check for an existing Grok credential: {error:#}");
        }
    }
    if !crate::interactive_backend::confirm("Log in to Grok / xAI now to finish setup?", true).await
    {
        return InlineGrokLoginState::Unauthenticated;
    }
    match crate::commands::grok_login::run_grok_login(
        access,
        node_did,
        &crate::commands::grok_login::GrokLoginOptions {
            provider,
            label: None,
        },
    )
    .await
    {
        Ok(outcome) => {
            if let Some(note) = added_account_note(&outcome.sign_in) {
                eprintln!("{note}");
            }
            InlineGrokLoginState::Completed(Box::new(outcome))
        }
        Err(error) => {
            eprintln!("Grok login failed: {error:#}");
            InlineGrokLoginState::Unauthenticated
        }
    }
}

enum InlineClaudeLoginState {
    Unauthenticated,
    ExistingCredential,
    Completed(Box<crate::commands::claude_login::ClaudeLoginOutcome>),
}

impl InlineClaudeLoginState {
    fn is_authenticated(&self) -> bool {
        matches!(self, Self::ExistingCredential | Self::Completed(_))
    }

    fn outcome(&self) -> Option<&crate::commands::claude_login::ClaudeLoginOutcome> {
        match self {
            Self::Completed(outcome) => Some(outcome.as_ref()),
            Self::Unauthenticated | Self::ExistingCredential => None,
        }
    }
}

async fn maybe_inline_claude_login(
    access: &ConfigAccess,
    node_did: &str,
    summary: &InitSummary,
) -> InlineClaudeLoginState {
    if summary.provider_kind != gents::BackendProviderKind::ClaudeCliSubscription {
        return InlineClaudeLoginState::Unauthenticated;
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return InlineClaudeLoginState::Unauthenticated;
    }
    let provider =
        gents::claude_oauth::normalize_provider(gents::claude_oauth::CLAUDE_OAUTH_PROVIDER);
    match gents::oauth_credential::resolve_oauth_credential(
        access,
        node_did,
        &provider,
        gents::oauth_credential::AccountPick::Reference(None),
    )
    .await
    {
        Ok(Some(_)) => return InlineClaudeLoginState::ExistingCredential,
        Ok(None) => {}
        Err(error) => {
            eprintln!("Could not check for an existing Claude credential: {error:#}");
        }
    }
    if !crate::interactive_backend::confirm("Log in to Claude now to finish setup?", true).await {
        return InlineClaudeLoginState::Unauthenticated;
    }
    match crate::commands::claude_login::run_claude_login(
        access,
        node_did,
        &crate::commands::claude_login::ClaudeLoginOptions {
            provider,
            label: None,
            manual: false,
            open_browser: true,
            client_id: None,
            token_url: None,
        },
    )
    .await
    {
        Ok(outcome) => {
            if let Some(note) = added_account_note(&outcome.sign_in) {
                eprintln!("{note}");
            }
            InlineClaudeLoginState::Completed(Box::new(outcome))
        }
        Err(error) => {
            eprintln!("Claude login failed: {error:#}");
            InlineClaudeLoginState::Unauthenticated
        }
    }
}

async fn maybe_inline_codex_login(
    access: &ConfigAccess,
    node_did: &str,
    summary: &InitSummary,
) -> InlineCodexLoginState {
    if summary.provider_kind != gents::BackendProviderKind::ChatGptCodex {
        return InlineCodexLoginState::Unauthenticated;
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return InlineCodexLoginState::Unauthenticated;
    }
    let provider = gents::chatgpt_codex::normalize_provider("chatgpt-codex");
    match gents::oauth_credential::resolve_oauth_credential(
        access,
        node_did,
        &provider,
        gents::oauth_credential::AccountPick::Reference(None),
    )
    .await
    {
        Ok(Some(_)) => return InlineCodexLoginState::ExistingCredential,
        Ok(None) => {}
        Err(error) => {
            eprintln!("Could not check for an existing ChatGPT credential: {error:#}");
        }
    }
    if !crate::interactive_backend::confirm("Log in to ChatGPT now to finish setup?", true).await {
        return InlineCodexLoginState::Unauthenticated;
    }
    match crate::commands::codex_login::run_codex_login(
        access,
        node_did,
        &crate::commands::codex_login::CodexLoginOptions {
            provider,
            label: None,
            client_id: None,
            issuer: None,
            device_auth: false,
        },
    )
    .await
    {
        Ok(outcome) => {
            if let Some(note) = added_account_note(&outcome.sign_in) {
                eprintln!("{note}");
            }
            InlineCodexLoginState::Completed(Box::new(outcome))
        }
        Err(error) => {
            eprintln!(
                "ChatGPT login did not complete: {error:#}\n\
                 The backend is configured; finish later with `gents codex-login` \
                 (add --device-auth on a headless machine)."
            );
            InlineCodexLoginState::Unauthenticated
        }
    }
}

pub(crate) struct IdentityOnlyHomeOptions<'a> {
    pub(crate) home: &'a Path,
    pub(crate) node_name: &'a str,
    pub(crate) key_path: Option<&'a Path>,
    pub(crate) identity_backend: IdentityBackendArg,
    pub(crate) keychain_label: Option<&'a str>,
    pub(crate) secure_enclave_label: Option<&'a str>,
    pub(crate) tool_package: ToolPackageArg,
    pub(crate) tool_root: Option<&'a Path>,
    pub(crate) reset: bool,
    pub(crate) store_key_custody: gents::store_key::StoreKeyCustodyChoice,
}

struct HomeIdentityOptions<'a> {
    home: &'a Path,
    node_name: &'a str,
    key_path: Option<&'a Path>,
    identity_backend: IdentityBackendArg,
    keychain_label: Option<&'a str>,
    secure_enclave_label: Option<&'a str>,
}

struct HomeIdentity {
    identity: Arc<dyn NodeIdentity>,
    key_path: Option<String>,
    identity_backend: Option<String>,
    keychain_label: Option<String>,
    secure_enclave_label: Option<String>,
    node_identity_did: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct IdentityOnlyHomeSummary {
    pub(crate) home: String,
    pub(crate) node_name: String,
    pub(crate) node_did: String,
    pub(crate) key_path: Option<String>,
    pub(crate) identity_backend: Option<String>,
    pub(crate) keychain_label: Option<String>,
    pub(crate) secure_enclave_label: Option<String>,
    pub(crate) tool_ceiling: ToolCeilingArg,
    pub(crate) tool_package: ToolPackageArg,
    pub(crate) tool_root: Option<String>,
    pub(crate) runtime_state_reset: bool,
}

pub(crate) async fn write_identity_only_home_metadata(
    options: IdentityOnlyHomeOptions<'_>,
) -> Result<IdentityOnlyHomeSummary> {
    let data_dir = default_data_dir(options.home);
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;

    let initialized_identity = load_or_create_home_identity(HomeIdentityOptions {
        home: options.home,
        node_name: options.node_name,
        key_path: options.key_path,
        identity_backend: options.identity_backend,
        keychain_label: options.keychain_label,
        secure_enclave_label: options.secure_enclave_label,
    })?;
    initialized_identity
        .identity
        .sign(b"gents init identity")
        .await
        .context("creating or loading node identity key")?;

    let tool_ceiling = tool_ceiling_for_package(options.tool_package);
    let tool_root = resolve_tool_root_for_package(options.tool_package, options.tool_root)?
        .map(|path| path.to_string_lossy().to_string());
    gents::storage_backend::reject_legacy_store(&data_dir)?;
    let mut stored = StoredInitConfig {
        home: options.home.to_string_lossy().to_string(),
        node_name: options.node_name.to_string(),
        node_did: initialized_identity.identity.did().to_string(),
        key_path: initialized_identity.key_path.clone(),
        identity_backend: initialized_identity.identity_backend.clone(),
        keychain_label: initialized_identity.keychain_label.clone(),
        secure_enclave_label: initialized_identity.secure_enclave_label.clone(),
        tool_package: Some(options.tool_package),
        tool_ceiling,
        tool_root: tool_root.clone(),
        store_encryption: None,
    };
    initialize_home_store(
        options.home,
        &data_dir,
        options.store_key_custody,
        &mut stored,
    )
    .await?;
    write_init_config(options.home, &stored)?;
    let runtime_state_reset = if options.reset {
        clear_runtime_state(options.home)?
    } else {
        false
    };

    Ok(IdentityOnlyHomeSummary {
        home: options.home.to_string_lossy().to_string(),
        node_name: options.node_name.to_string(),
        node_did: initialized_identity.identity.did().to_string(),
        key_path: initialized_identity.key_path,
        identity_backend: initialized_identity.identity_backend,
        keychain_label: initialized_identity.keychain_label,
        secure_enclave_label: initialized_identity.secure_enclave_label,
        tool_ceiling,
        tool_package: options.tool_package,
        tool_root,
        runtime_state_reset,
    })
}

/// Identity and key custody must survive an error or interruption before
/// runtime configuration finishes. The caller holds the home's store lock.
async fn initialize_home_store(
    home: &Path,
    data_path: &Path,
    choice: gents::store_key::StoreKeyCustodyChoice,
    stored: &mut StoredInitConfig,
) -> Result<gents::store_key::StoreKey> {
    let key_file = gents::store_key::home_key_file(home);
    let record = match read_init_config(home)? {
        Some(_) => {
            let key =
                gents::store_key::open_home_store_key_with_custody(home, data_path, choice).await?;
            stored.store_encryption =
                read_init_config(home)?.and_then(|config| config.store_encryption);
            return Ok(key);
        }
        None => gents::store_key::StoreEncryption::prepare(choice, &key_file, data_path)?,
    };
    stored.store_encryption = Some(record.clone());
    write_init_config(home, stored)?;
    Ok(record.initialize(&key_file, data_path)?)
}

fn load_or_create_home_identity(options: HomeIdentityOptions<'_>) -> Result<HomeIdentity> {
    if let Some(stored) = read_init_config(options.home)? {
        let identity = crate::home_state::load_initialized_home_identity(options.home, &stored)?;
        let node_identity_did = Some(identity.did().to_string());
        return Ok(HomeIdentity {
            identity,
            key_path: stored.key_path,
            identity_backend: stored.identity_backend,
            keychain_label: stored.keychain_label,
            secure_enclave_label: stored.secure_enclave_label,
            node_identity_did,
        });
    }
    match options.identity_backend {
        IdentityBackendArg::File => {
            let key_path = options
                .key_path
                .map(Path::to_path_buf)
                .unwrap_or_else(|| default_key_path(options.home, options.node_name));
            let identity = Arc::new(
                KeyIdentity::load_or_create(&key_path, None)
                    .context("creating or loading node identity key")?,
            );
            let node_identity_did = identity.did().to_string();
            Ok(HomeIdentity {
                identity,
                key_path: Some(key_path.to_string_lossy().to_string()),
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                node_identity_did: Some(node_identity_did),
            })
        }
        IdentityBackendArg::MacosKeychain => {
            if options.key_path.is_some() {
                anyhow::bail!("--key-path cannot be used with --identity-backend macos-keychain");
            }
            let label = options
                .keychain_label
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "--keychain-label is required with --identity-backend macos-keychain"
                    )
                })?;
            let identity = Arc::new(
                load_or_create_macos_keychain_identity(label, None)
                    .with_context(|| format!("loading macOS keychain identity {label}"))?,
            );
            let did = identity.did().to_string();
            Ok(HomeIdentity {
                identity,
                key_path: None,
                identity_backend: Some("macos-keychain".to_string()),
                keychain_label: Some(label.to_string()),
                secure_enclave_label: None,
                node_identity_did: Some(did),
            })
        }
        IdentityBackendArg::MacosSecureEnclave => {
            if options.key_path.is_some() {
                anyhow::bail!(
                    "--key-path cannot be used with --identity-backend macos-secure-enclave"
                );
            }
            let label = options
                .secure_enclave_label
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "--secure-enclave-label is required with --identity-backend macos-secure-enclave"
                    )
                })?;
            let identity = Arc::new(
                load_or_create_macos_secure_enclave_identity(label, None)
                    .with_context(|| format!("loading macOS Secure Enclave identity {label}"))?,
            );
            let did = identity.did().to_string();
            Ok(HomeIdentity {
                identity,
                key_path: None,
                identity_backend: Some("macos-secure-enclave".to_string()),
                keychain_label: None,
                secure_enclave_label: Some(label.to_string()),
                node_identity_did: Some(did),
            })
        }
    }
}

async fn initialize_runtime_home(
    access: &ConfigAccess,
    args: &InitArgs,
    node_did: &str,
    tool_package: ToolPackageArg,
) -> Result<InitSummary> {
    let Some(node) = access.local_node() else {
        anyhow::bail!("init requires local DefraDB access");
    };
    let explicit_backend_id = args
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let explicit_backend_name = args
        .backend_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let model_name = resolve_init_model_name(args)?;
    let backend = resolve_init_backend_config(args)?;
    let backend_id_was_generated = explicit_backend_id.is_none();
    let backend_id = explicit_backend_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_backend_id_for_agent(node_did));
    let backend_name = explicit_backend_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if backend_id_was_generated {
                format!("{} backend", args.node_name)
            } else {
                backend_id.clone()
            }
        });
    let existing_node = load_node(node, node_did).await?;
    let default_agent_id = existing_node
        .as_ref()
        .and_then(|node_document| {
            normalize_optional_string(node_document.default_agent_id.as_deref())
        })
        .unwrap_or_else(|| default_agent_id_for_node(node_did));
    let existing_default_agent = load_agent(node, &default_agent_id).await?;
    if let Some(agent) = existing_default_agent.as_ref() {
        if agent.node_did != node_did {
            anyhow::bail!(
                "Agent {} belongs to {} not {}",
                default_agent_id,
                agent.node_did,
                node_did
            );
        }
    }
    let node_display_name = existing_node
        .as_ref()
        .and_then(|node_document| normalize_optional_string(node_document.display_name.as_deref()))
        .unwrap_or_else(|| args.node_name.clone());
    let node_enabled = existing_node
        .as_ref()
        .map(|node_document| node_document.enabled)
        .unwrap_or(true);
    let tools_id = default_tools_id_for_agent(&default_agent_id);
    let tool_ceiling = tool_ceiling_for_package(tool_package);
    let tool_root = resolve_tool_root_for_package(tool_package, args.tool_root.as_deref())?;
    let explicit_workspace_policy = gents::tool_surface::load_workspace_root_policy(node, None)
        .await?
        .configured;
    let pinned_tool_root = explicit_workspace_policy
        .then(|| tool_root.clone())
        .flatten();
    let backend_auth = backend_auth_for_init(&backend)?;
    let backend_doc = InferenceBackend {
        node_did: node_did.to_string(),
        backend_id: backend_id.clone(),
        name: backend_name.clone(),
        provider_kind: backend.provider_kind,
        openai_wire_api: backend.openai_wire_api,
        endpoint: backend.endpoint.clone(),
        auth: backend_auth,
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: args.max_concurrent,
        max_queue_depth: Some(args.max_queue_depth),
        enabled: model_name.is_some(),
        tags: Vec::new(),
    };

    let enable_defra_query = init_enable_defra_query(
        tool_package,
        args.enable_defra_query,
        args.disable_defra_query,
    );
    let mut tools = tools_for_package(
        node_did,
        &tools_id,
        tool_package,
        pinned_tool_root,
        args.enable_memory,
        enable_defra_query,
        args.defra_query_collections.clone(),
    );
    tools.self_config = Some(engineer_self_config());
    let engineer_mailbox = args
        .engineer
        .then(|| engineer_tools(&mut tools, node_did, !args.disable_defra_query));
    let context = AgentContext {
        context_id: default_context_id_for_agent(&default_agent_id),
        node_did: node_did.to_string(),
        display_name: Some(if args.engineer {
            "The Engineer".to_string()
        } else {
            "Default".to_string()
        }),
        description: None,
        system_prompt: Some(if args.engineer {
            ENGINEER_SYSTEM_PROMPT.to_string()
        } else {
            standard_system_prompt(tool_package).to_string()
        }),
        tools_id: Some(tools_id.clone()),
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    let inference_profile_id = default_inference_profile_id_for_agent(&default_agent_id);
    let inference_execution_id = default_inference_execution_id_for_profile(&inference_profile_id);
    let inference_execution = standard_inference_execution(node_did, &inference_execution_id);
    let mut inference_profile = standard_inference_profile(
        node_did,
        &inference_profile_id,
        &backend_id,
        model_name.unwrap_or_default(),
    );
    inference_profile.execution_id = Some(inference_execution_id);
    let agent = Agent {
        agent_id: default_agent_id.clone(),
        node_did: node_did.to_string(),
        display_name: Some(if args.engineer {
            "The Engineer".to_string()
        } else {
            "Default".to_string()
        }),
        description: if args.engineer {
            Some("Walks you through configuring Gents for the work you want to do.".to_string())
        } else {
            None
        },
        context_id: Some(context.context_id.clone()),
        inference_profile_id: inference_profile_id.clone(),
        enabled: true,
        tags: if args.engineer {
            vec![gents::self_config::ENGINEER_AGENT_TAG.to_string()]
        } else {
            Vec::new()
        },
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    if let Some(error) = tools
        .validation_violations()
        .into_iter()
        .chain(wide_open_tools_document(node_did).validation_violations())
        .next()
    {
        return Err(anyhow::anyhow!("seeded Tools document: {error}"));
    }
    backend_doc.validate()?;
    inference_execution.validate()?;
    inference_profile.validate()?;
    let wide_open_preset_id = wide_open_tools_id_for_node(node_did);
    let mut documents = vec![
        replacement(Collection::InferenceBackend, &backend_doc)?,
        replacement(Collection::Tools, &tools)?,
        replacement(Collection::AgentContext, &context)?,
        replacement(Collection::InferenceExecution, &inference_execution)?,
        replacement(Collection::InferenceProfile, &inference_profile)?,
        replacement(Collection::Agent, &agent)?,
        replacement(Collection::Tools, &wide_open_tools_document(node_did))?,
    ];
    if let Some(surface) = &engineer_mailbox {
        documents.push(replacement(Collection::DatastoreToolSurface, surface)?);
    }
    publish_home_config(
        access,
        node_did,
        &node_display_name,
        &default_agent_id,
        node_enabled,
        documents,
    )
    .await?;

    Ok(InitSummary {
        backend_id,
        backend_name,
        provider_kind: backend.provider_kind,
        endpoint: backend.endpoint,
        api_key: backend.api_key.map(|_| "<redacted>".to_string()),
        api_key_env_var: backend.api_key_env_var,
        model_name: model_name.unwrap_or_default().to_string(),
        max_concurrent: backend_doc.effective_max_concurrent(),
        max_queue_depth: args.max_queue_depth,
        default_agent_id,
        tools_id: tools_id.clone(),
        wide_open_preset_id,
        inference_profile_id,
        tool_package,
        tool_ceiling,
        tool_root: tool_root.map(|path| path.to_string_lossy().to_string()),
        enable_memory: args.enable_memory,
        enable_defra_query,
        defra_query_collections: args.defra_query_collections.clone(),
        created_node: existing_node.is_none(),
        created_default_agent: existing_default_agent.is_none(),
    })
}

async fn publish_home_config(
    access: &ConfigAccess,
    node_did: &str,
    display_name: &str,
    default_agent_id: &str,
    enabled: bool,
    documents: Vec<DesiredStateApplyDocument>,
) -> Result<()> {
    access
        .transact_write("init.initialize_runtime_home", |txn| {
            let mut documents = documents.clone();
            Box::pin(async move {
                let mut node_document =
                    read_desired_state_record_in_txn(txn, Collection::Node, node_did, node_did)
                        .await?
                        .map(|(_, value)| value)
                        .unwrap_or_else(|| {
                            json!({
                                "node_did": node_did,
                                "created_at": chrono::Utc::now().to_rfc3339(),
                                "created_by": node_did,
                            })
                        });
                node_document["display_name"] = json!(display_name);
                node_document["default_agent_id"] = json!(default_agent_id);
                node_document["enabled"] = json!(enabled);
                documents.push(DesiredStateApplyDocument {
                    collection: Collection::Node,
                    add: node_document.clone(),
                    update: node_document,
                });
                let plan = DesiredStateApplyPlan::new(documents)?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
}

fn replacement<T>(collection: Collection, document: &T) -> Result<DesiredStateApplyDocument>
where
    T: serde::Serialize,
{
    let value = serde_json::to_value(document)?;
    Ok(DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    })
}

fn default_tools_id_for_agent(agent_id: &str) -> String {
    format!("{agent_id}-tools")
}

fn default_context_id_for_agent(agent_id: &str) -> String {
    format!("{agent_id}-context")
}

fn tools_for_package(
    node_did: &str,
    tools_id: &str,
    tool_package: ToolPackageArg,
    tool_root: Option<PathBuf>,
    enable_memory: bool,
    enable_defra_query: bool,
    defra_query_collections: Vec<String>,
) -> Tools {
    let host = match tool_package {
        ToolPackageArg::Minimal | ToolPackageArg::Introspection => None,
        ToolPackageArg::Readonly | ToolPackageArg::Write | ToolPackageArg::Yolo => {
            Some(HostTools {
                root: tool_root.map(|path| path.to_string_lossy().to_string()),
                files: Some(FileTools {
                    mode: match tool_package {
                        ToolPackageArg::Readonly => FileToolMode::ReadOnly,
                        _ => FileToolMode::ReadWrite,
                    },
                    ..Default::default()
                }),
                bash: Some(BashTools {
                    mode: match tool_package {
                        ToolPackageArg::Readonly => BashMode::ReadOnly,
                        _ => BashMode::Unrestricted,
                    },
                    execution_mode: match tool_package {
                        ToolPackageArg::Write if cfg!(target_os = "macos") => {
                            Some(CommandExecutionMode::WorkspaceWrite)
                        }
                        ToolPackageArg::Write | ToolPackageArg::Yolo => {
                            Some(CommandExecutionMode::Unrestricted)
                        }
                        _ => None,
                    },
                    background_enabled: matches!(
                        tool_package,
                        ToolPackageArg::Write | ToolPackageArg::Yolo
                    ),
                    ..Default::default()
                }),
                cli: Vec::new(),
            })
        }
    };
    let privileged = !matches!(tool_package, ToolPackageArg::Minimal);
    Tools {
        tools_id: tools_id.to_string(),
        node_did: node_did.to_string(),
        display_name: Some(
            match tool_package {
                ToolPackageArg::Minimal => "Minimal Tools",
                ToolPackageArg::Introspection => "Introspection Tools",
                ToolPackageArg::Readonly => "Standard Read-Only Tools",
                ToolPackageArg::Write => "Standard Write Tools",
                ToolPackageArg::Yolo => "Unrestricted Write Tools (YOLO)",
            }
            .to_string(),
        ),
        host,
        remote: None,
        agents: None,
        built_ins: Some(BuiltInTools {
            enable_goal_tools: privileged.then_some(true),
            enable_goal_creation: Some(false),
            enable_memory: Some(enable_memory),
            enable_session_history_tool: None,
            enable_schema_tool: None,
            enable_p2p_tool: None,
            enable_p2p_mutations: None,
            p2p_collections: Vec::new(),
            enable_context_budget: Some(true),
            ..Default::default()
        }),
        datastore: Some(DatastoreTools {
            write_collections: None,
            enable_defra_query: Some(enable_defra_query),
            defra_query_collections: (!defra_query_collections.is_empty())
                .then_some(defra_query_collections),
            datastore_tool_surface_ids: None,
        }),
        integrations: None,
        self_config: None,
        tags: Vec::new(),
    }
}

fn wide_open_tools_document(node_did: &str) -> Tools {
    Tools {
        tools_id: wide_open_tools_id_for_node(node_did),
        node_did: node_did.to_string(),
        display_name: Some("Wide-open (permissive preset)".to_string()),
        built_ins: Some(BuiltInTools {
            enable_context_budget: Some(true),
            ..Default::default()
        }),
        datastore: Some(DatastoreTools {
            enable_defra_query: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn engineer_tools(
    tools: &mut Tools,
    node_did: &str,
    enable_defra_query: bool,
) -> DatastoreToolSurfaceDocument {
    let surface_id = "engineer-mailbox".to_string();
    tools.agents = Some(AgentTools {
        target_ids: Vec::new(),
        enabled: Some(true),
    });
    let built_ins = tools.built_ins.get_or_insert_with(Default::default);
    built_ins.enable_session_history_tool = Some(true);
    built_ins.enable_schema_tool = Some(true);
    built_ins.enable_p2p_tool = Some(true);
    built_ins.enable_p2p_mutations = Some(true);
    let datastore = tools.datastore.get_or_insert_with(Default::default);
    datastore.enable_defra_query = Some(enable_defra_query);
    datastore.datastore_tool_surface_ids = Some(vec![surface_id.clone()]);
    DatastoreToolSurfaceDocument {
        surface_id,
        node_did: node_did.to_string(),
        display_name: Some("Engineer escalations".to_string()),
        enabled: true,
        entries: Some(vec![SurfaceToolDecl::Create(
            gents::mailbox::canonical_mailbox_write_decl(),
        )]),
        created_at: None,
        tags: Vec::new(),
    }
}

fn wide_open_tools_id_for_node(node_did: &str) -> String {
    format!("{node_did}:wide-open")
}

fn backend_auth_for_init(backend: &ResolvedBackendConfig) -> Result<BackendAuth> {
    if backend.provider_kind.is_node_scoped_oauth() {
        return Ok(BackendAuth::NodeOAuth { account_ref: None });
    }
    match (
        backend.api_key.as_deref(),
        backend.api_key_env_var.as_deref(),
    ) {
        (Some(key), _) => Ok(BackendAuth::ApiKey {
            key: key.to_string(),
        }),
        (None, Some(variable)) => Ok(BackendAuth::Environment {
            variable: variable.to_string(),
        }),
        (None, None) => Ok(BackendAuth::Unauthenticated),
    }
}

fn resolve_initial_tool_package(args: &InitArgs) -> Result<ToolPackageArg> {
    resolve_init_tool_package(
        args.write_tools || (args.engineer && !args.yolo && args.tool_package.is_none()),
        args.yolo,
        args.tool_package,
    )
}

fn resolve_init_tool_package(
    write_tools: bool,
    yolo: bool,
    explicit: Option<ToolPackageArg>,
) -> Result<ToolPackageArg> {
    if yolo {
        return match explicit {
            Some(ToolPackageArg::Yolo) | None => Ok(ToolPackageArg::Yolo),
            Some(other) => anyhow::bail!(
                "--yolo is shorthand for --tool-package yolo; remove --yolo or choose --tool-package {}",
                format_tool_package(other)
            ),
        };
    }
    match (write_tools, explicit) {
        (true, Some(ToolPackageArg::Write)) | (true, None) => Ok(ToolPackageArg::Write),
        (true, Some(other)) => anyhow::bail!(
            "--write is shorthand for --tool-package write; remove --write or choose --tool-package {}",
            format_tool_package(other)
        ),
        (false, Some(package)) => Ok(package),
        (false, None) => Ok(ToolPackageArg::Readonly),
    }
}

fn validate_init_tool_flags(args: &InitArgs, tool_package: ToolPackageArg) -> Result<()> {
    if args.identity_only
        && (args.enable_memory
            || args.enable_defra_query
            || args.disable_defra_query
            || !args.defra_query_collections.is_empty())
    {
        anyhow::bail!(
            "--enable-memory, --enable-defra-query, --disable-defra-query, and --defra-query-collection cannot be used with --identity-only because no Tools document is written"
        );
    }
    if !args.defra_query_collections.is_empty()
        && (args.disable_defra_query
            || !(package_enables_defra_query(tool_package) || args.enable_defra_query))
    {
        anyhow::bail!(
                "--defra-query-collection requires defra_query to be enabled — pass --enable-defra-query or a tool package that enables it, and do not combine with --disable-defra-query"
            );
    }
    Ok(())
}

fn init_enable_defra_query(
    tool_package: ToolPackageArg,
    enable_defra_query: bool,
    disable_defra_query: bool,
) -> bool {
    (package_enables_defra_query(tool_package) || enable_defra_query) && !disable_defra_query
}

fn package_enables_defra_query(tool_package: ToolPackageArg) -> bool {
    matches!(tool_package, ToolPackageArg::Introspection)
}

fn tool_ceiling_for_package(tool_package: ToolPackageArg) -> ToolCeilingArg {
    match tool_package {
        ToolPackageArg::Minimal | ToolPackageArg::Introspection => ToolCeilingArg::MetaOnly,
        ToolPackageArg::Readonly => ToolCeilingArg::Readonly,
        ToolPackageArg::Write | ToolPackageArg::Yolo => ToolCeilingArg::Readwrite,
    }
}

fn resolve_tool_root_for_package(
    tool_package: ToolPackageArg,
    explicit: Option<&Path>,
) -> Result<Option<PathBuf>> {
    let needs_root = matches!(
        tool_package,
        ToolPackageArg::Readonly | ToolPackageArg::Write | ToolPackageArg::Yolo
    );
    if needs_root || explicit.is_some() {
        Ok(Some(resolve_default_tool_root(explicit)?))
    } else {
        Ok(None)
    }
}

fn standard_inference_profile(
    node_did: &str,
    profile_id: &str,
    backend_id: &str,
    model_name: &str,
) -> InferenceProfile {
    InferenceProfile {
        node_did: node_did.to_string(),
        profile_id: profile_id.to_string(),
        display_name: Some("Default".to_string()),
        description: None,
        backend_id: backend_id.to_string(),
        model_name: model_name.to_string(),
        reasoning_effort: None,
        service_tier: None,
        context_window: None,
        max_output_tokens: None,
        sampling_id: None,
        execution_id: None,
        tags: Vec::new(),
    }
}

fn default_inference_execution_id_for_profile(profile_id: &str) -> String {
    format!("{profile_id}-execution")
}

fn standard_inference_execution(node_did: &str, execution_id: &str) -> InferenceExecution {
    InferenceExecution {
        node_did: node_did.to_string(),
        execution_id: execution_id.to_string(),
        display_name: Some("Default".to_string()),
        ..Default::default()
    }
}

fn default_backend_id_for_agent(node_did: &str) -> String {
    format!("{node_did}:backend")
}

fn standard_system_prompt(tool_package: ToolPackageArg) -> &'static str {
    match tool_package {
        ToolPackageArg::Write | ToolPackageArg::Yolo => STANDARD_READWRITE_SYSTEM_PROMPT,
        ToolPackageArg::Minimal | ToolPackageArg::Introspection | ToolPackageArg::Readonly => {
            STANDARD_READONLY_SYSTEM_PROMPT
        }
    }
}

fn init_next_steps(
    summary: &InitSummary,
    codex_logged_in: bool,
    grok_logged_in: bool,
    claude_logged_in: bool,
) -> Vec<String> {
    let mut steps = Vec::new();
    if summary.provider_kind == gents::BackendProviderKind::ChatGptCodex && !codex_logged_in {
        steps.push("gents codex-login".to_string());
    }
    if summary.provider_kind == gents::BackendProviderKind::XaiGrokOAuth && !grok_logged_in {
        steps.push("gents grok-login".to_string());
    }
    if summary.provider_kind == gents::BackendProviderKind::ClaudeCliSubscription
        && !claude_logged_in
    {
        steps.push("gents claude-login".to_string());
    }
    if summary.model_name.is_empty() {
        steps.push(
            "Select a provider and model in Gents settings before starting work.".to_string(),
        );
    } else if is_probably_ollama_endpoint(&summary.endpoint) {
        steps.push(format!("ollama pull {}", summary.model_name));
    } else if is_probably_llama_server_endpoint(&summary.endpoint) {
        steps.push(format!("llama-server -hf {}", summary.model_name));
    }
    steps.push("gents server".to_string());
    steps.push("gents codex".to_string());
    steps.push(format!(
        "gents config backend set --graphql http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql --backend-id {} --name {} --endpoint <URL>",
        summary.backend_id, summary.backend_name
    ));
    steps
}

fn is_probably_ollama_endpoint(endpoint: &str) -> bool {
    endpoint.contains("localhost:11434") || endpoint.contains("127.0.0.1:11434")
}

fn is_probably_llama_server_endpoint(endpoint: &str) -> bool {
    endpoint.contains("localhost:8080") || endpoint.contains("127.0.0.1:8080")
}

fn resolve_init_backend_config(args: &InitArgs) -> Result<ResolvedBackendConfig> {
    crate::resolve_backend_config_with_preset(
        args.backend_preset,
        args.resolved_inference_endpoint(),
        args.provider_kind.as_deref(),
        args.openai_wire_api,
        args.api_key.as_deref(),
        args.api_key_env_var.as_deref(),
        BackendResolutionMode::Init,
    )
}

fn resolve_init_model_name(args: &InitArgs) -> Result<Option<&str>> {
    if let Some(explicit) = args.model_name.as_deref() {
        let model_name = explicit.trim();
        if model_name.is_empty() {
            anyhow::bail!("--model-name must not be empty");
        }
        return Ok(Some(model_name));
    }

    if let Some(default) = args.preset_default_model_name() {
        return Ok(Some(default));
    }

    if let Some(preset) = args.backend_preset {
        anyhow::bail!(
            "--model-name is required for --backend-preset {} because that preset has no safe default model",
            preset.as_str()
        );
    }

    Ok((!args.engineer).then_some(crate::DEFAULT_INIT_MODEL_NAME))
}

fn resolve_default_tool_root(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }

    std::env::current_dir()
        .ok()
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or_else(|| anyhow::anyhow!("unable to determine a default tool root for local tools"))
}

#[cfg(test)]
#[path = "init/inference_seed_tests.rs"]
mod inference_seed_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn overwrite(home: &Path, user_home: &Path) -> Result<gents::home::StoreLock> {
        lock_init_store_for_user(home, &home.join("data"), true, Some(user_home))
    }

    #[cfg(unix)]
    #[test]
    fn an_overwrite_never_wipes_the_user_home_through_an_alias() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("user");
        fs::create_dir_all(user_home.join("Documents")).unwrap();
        fs::write(user_home.join("Documents/precious"), "keep").unwrap();
        let link = temp.path().join("gents-home");
        std::os::unix::fs::symlink(&user_home, &link).unwrap();

        for alias in [
            link.clone(),
            user_home.join("."),
            user_home.join("Documents").join(".."),
            temp.path().to_path_buf(),
        ] {
            let error = overwrite(&alias, &user_home)
                .expect_err("the user home and its ancestors are never overwritten")
                .to_string();
            assert!(error.contains("user home"), "{alias:?}: {error}");
        }
        assert_eq!(
            fs::read_to_string(user_home.join("Documents/precious")).unwrap(),
            "keep"
        );
        assert!(
            !user_home.join("data").exists(),
            "nothing is created before the home is validated"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_overwrite_refuses_a_data_directory_linked_out_of_the_home() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("user");
        fs::create_dir_all(&user_home).unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let external = temp.path().join("real-data");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("MANIFEST"), "store").unwrap();
        std::os::unix::fs::symlink(&external, home.join("data")).unwrap();

        let error = overwrite(&home, &user_home).expect_err("a linked data directory is refused");
        assert!(error.to_string().contains("symlink"), "{error}");
        assert!(external.join("MANIFEST").is_file());
        assert!(fs::symlink_metadata(home.join("data"))
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[cfg(unix)]
    #[test]
    fn an_overwrite_through_a_linked_home_keeps_one_lock() {
        let temp = tempfile::tempdir().unwrap();
        let user_home = temp.path().join("user");
        fs::create_dir_all(&user_home).unwrap();
        let real = temp.path().join("real-home");
        fs::create_dir_all(real.join("data")).unwrap();
        fs::write(real.join("init.json"), "{}").unwrap();
        let link = temp.path().join("link-home");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let held = overwrite(&link, &user_home).expect("a dedicated linked home is overwritten");
        assert!(!real.join("init.json").exists());
        assert!(real.join("data").is_dir());
        for alias in [link.join("data"), real.join("data")] {
            assert!(
                gents::home::lock_store(&real, &alias).is_err(),
                "{alias:?} takes the lock init holds"
            );
        }
        drop(held);
    }

    #[test]
    fn an_overwrite_is_refused_when_the_user_home_is_unknown_or_unresolvable() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(home.join("data")).unwrap();
        fs::write(home.join("init.json"), "{}").unwrap();
        for unknown in [None, Some(Path::new(""))] {
            let error = lock_init_store_for_user(&home, &home.join("data"), true, unknown)
                .expect_err("an overwrite needs a known user home")
                .to_string();
            assert!(error.contains("cannot be determined"), "{error}");
        }
        let missing = temp.path().join("missing-user-home");
        let error = lock_init_store_for_user(&home, &home.join("data"), true, Some(&missing))
            .expect_err("an unresolvable user home is not compared lexically");
        assert!(
            format!("{error:#}").contains("cannot be resolved"),
            "{error:#}"
        );
        assert!(home.join("init.json").is_file(), "nothing was wiped");
        lock_init_store_for_user(&home, &home.join("data"), false, None)
            .expect("init without an overwrite does not need the user home");
    }

    #[test]
    fn init_holds_the_store_lock_until_it_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let data = home.join("data");
        let held = lock_init_store(&home, &data, false).expect("init locks a fresh store");
        assert!(data.is_dir());
        assert!(
            gents::home::lock_store(&home, &data).is_err(),
            "a runtime cannot open the store while init runs"
        );
        drop(held);
        gents::home::lock_store(&home, &data).expect("init releases the store");
    }

    #[test]
    fn an_overwrite_wipes_under_the_lock_and_keeps_excluding_others() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let data = home.join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("MANIFEST"), "store").unwrap();
        fs::write(home.join("init.json"), "{}").unwrap();

        let user_home = temp.path().join("user");
        fs::create_dir_all(&user_home).unwrap();
        let runtime = gents::home::lock_store(&home, &data).unwrap();
        assert!(
            overwrite(&home, &user_home).is_err(),
            "a store a runtime has open is not wiped"
        );
        assert!(data.join("MANIFEST").is_file());
        drop(runtime);

        let held = overwrite(&home, &user_home).expect("an idle home is overwritten");
        assert!(!data.join("MANIFEST").exists());
        assert!(!home.join("init.json").exists());
        assert!(data.is_dir());
        assert!(
            held.path().is_file(),
            "the held lock file survives the wipe"
        );
        assert!(
            gents::home::lock_store(&home, &data).is_err(),
            "the lock init holds is the one a runtime would take"
        );
    }

    #[tokio::test]
    async fn a_freshly_initialized_profile_resolves_to_the_default_turn_limit() {
        let temp = tempfile::tempdir().unwrap();
        let identity =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap());
        let owner = identity.did().to_string();
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .with_node_identity_did(owner.clone())
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());

        let agent_id = default_agent_id_for_node(&owner);
        let profile_id = default_inference_profile_id_for_agent(&agent_id);
        let execution_id = default_inference_execution_id_for_profile(&profile_id);
        let backend_id = default_backend_id_for_agent(&owner);
        let backend = json!({"node_did": owner, "backend_id": backend_id, "name": "Local",
            "provider_kind": "OpenAiCompatible", "endpoint": "http://localhost:8000/v1",
            "auth": {"kind": "unauthenticated"}, "enabled": true});
        let agent = json!({"node_did": owner, "agent_id": agent_id,
            "inference_profile_id": profile_id, "enabled": true});
        let mut profile = standard_inference_profile(&owner, &profile_id, &backend_id, "model");
        profile.execution_id = Some(execution_id.clone());
        let documents = vec![
            DesiredStateApplyDocument {
                collection: Collection::InferenceBackend,
                add: backend.clone(),
                update: backend,
            },
            replacement(
                Collection::InferenceExecution,
                &standard_inference_execution(&owner, &execution_id),
            )
            .unwrap(),
            replacement(Collection::InferenceProfile, &profile).unwrap(),
            DesiredStateApplyDocument {
                collection: Collection::Agent,
                add: agent.clone(),
                update: agent,
            },
        ];
        publish_home_config(&access, &owner, "Agent", &agent_id, true, documents)
            .await
            .unwrap();
        gents::backend_registry::set_backend_probe_status_with_last_probe(
            &node,
            &owner,
            &backend_id,
            gents::HEALTHY_PROBE_STATUS,
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        let agent = gents::Gents::from_default_agent_documents(
            node.clone(),
            identity,
            gents::DocumentRuntimeOptions::default(),
        )
        .await
        .unwrap();
        let resolved = agent
            .agents()
            .iter()
            .find(|resolved| resolved.agent_id == agent_id)
            .expect("init default agent resolves");
        assert_eq!(resolved.max_turns, gents::config::DEFAULT_MAX_TURNS);
        assert_eq!(resolved.max_turns, 1_000);
        assert_eq!(
            resolved.max_turns_provenance,
            gents::config::MaxTurnsProvenance::Default
        );
    }

    #[tokio::test]
    async fn the_engineer_sets_max_turns_on_a_freshly_initialized_home() {
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn gents::NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap());
        let owner = identity.did().to_string();
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .with_node_identity_did(owner.clone())
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());

        let args = init_args();
        let tool_package = resolve_initial_tool_package(&args).unwrap();
        let summary = initialize_runtime_home(&access, &args, &owner, tool_package)
            .await
            .expect("a fresh home initializes");
        gents::backend_registry::set_backend_probe_status_with_last_probe(
            &node,
            &owner,
            &summary.backend_id,
            gents::HEALTHY_PROBE_STATUS,
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        let categories = engineer_self_config()
            .self_config_categories
            .expect("the configurator grant names its categories");
        assert!(categories.iter().any(|category| category == "profile"));
        let tools = gents::self_config::build_self_config_tools(
            std::sync::Arc::default(),
            node.clone(),
            owner.clone(),
            Some(identity.clone()),
            &gents::tool_surface::SelfConfigToolConfig {
                enabled: true,
                agent_id: summary.default_agent_id.clone(),
                categories: categories.into_iter().collect(),
                ..Default::default()
            },
            std::sync::Arc::new(gents::plugin::executor::PluginExecutor::default()),
        );
        let config = tools
            .iter()
            .find(|tool| tool.name() == gents::self_config::CONFIG_TOOL_NAME)
            .expect("the configurator registers its config tool");
        config
            .call(
                json!({"argv":["profile","edit","execution","--set","max_turns=250"]}).to_string(),
            )
            .await
            .expect("a freshly initialized home binds an execution the configurator can edit");

        let agent = gents::Gents::from_default_agent_documents(
            node.clone(),
            identity,
            gents::DocumentRuntimeOptions::default(),
        )
        .await
        .unwrap();
        let resolved = agent
            .agents()
            .iter()
            .find(|resolved| resolved.agent_id == summary.default_agent_id)
            .expect("init default agent resolves")
            .clone();
        assert_eq!(resolved.max_turns, 250);
        assert_eq!(
            resolved.max_turns_provenance,
            gents::config::MaxTurnsProvenance::ExecutionProfile
        );
    }

    async fn explain_initialized_default_agent(
        engineer: bool,
    ) -> gents::tool_surface::ToolSurfaceExplanation {
        let temp = tempfile::tempdir().unwrap();
        let identity: Arc<dyn gents::NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(temp.path().join("agent.key"), None).unwrap());
        let owner = identity.did().to_string();
        let node = Arc::new(
            gents::defra_node::EmbeddedNode::builder()
                .with_node_identity_did(owner.clone())
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());

        let mut args = init_args();
        args.engineer = engineer;
        let tool_package = resolve_initial_tool_package(&args).unwrap();
        let summary = initialize_runtime_home(&access, &args, &owner, tool_package)
            .await
            .expect("a fresh home initializes");
        gents::backend_registry::set_backend_probe_status_with_last_probe(
            &node,
            &owner,
            &summary.backend_id,
            gents::HEALTHY_PROBE_STATUS,
            chrono::Utc::now(),
        )
        .await
        .unwrap();

        let agent = gents::Gents::from_default_agent_documents(
            node.clone(),
            identity,
            gents::DocumentRuntimeOptions::default(),
        )
        .await
        .unwrap();
        agent
            .agents()
            .iter()
            .find(|resolved| resolved.agent_id == summary.default_agent_id)
            .expect("init default agent resolves")
            .tools
            .explain_with_runtime(false, &owner, &Default::default())
    }

    #[tokio::test]
    async fn the_engineer_gets_the_p2p_tool_and_other_presets_do_not() {
        let p2p = gents::p2p_tool::P2P_TOOL_NAME;

        let engineer = explain_initialized_default_agent(true).await;
        assert!(
            engineer.tool_names.iter().any(|name| name == p2p),
            "{:?}",
            engineer.tool_names
        );
        let effective = &engineer.policy.effective[p2p];
        assert!(
            effective.iter().any(|entry| entry == "mutate:true"),
            "{effective:?}"
        );

        let default = explain_initialized_default_agent(false).await;
        assert!(
            !default.tool_names.iter().any(|name| name == p2p),
            "{:?}",
            default.tool_names
        );
    }

    #[tokio::test]
    async fn failed_init_publishes_no_default_agent() {
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .build()
                .await
                .unwrap(),
        );
        gents::ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::local(node.clone());
        let owner = "did:key:z-init-atomic";
        let backend = json!({"node_did":owner,"backend_id":"backend","name":"Local",
            "provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1",
            "auth":{"kind":"unauthenticated"}});
        let profile = json!({"node_did":owner,"profile_id":"profile","backend_id":"backend",
            "model_name":"model"});
        let agent = |enabled: bool| {
            json!({"node_did":owner,"agent_id":"default",
                "inference_profile_id":"profile","enabled":enabled})
        };
        let entry = |collection, value: serde_json::Value| DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        };
        let node_document = || async { load_node(&node, owner).await.unwrap() };

        let missing = vec![
            entry(Collection::InferenceBackend, backend.clone()),
            entry(Collection::InferenceProfile, profile.clone()),
        ];
        assert!(
            publish_home_config(&access, owner, "Agent", "default", true, missing)
                .await
                .is_err()
        );
        assert_eq!(node_document().await, None);
        let disabled = vec![
            entry(Collection::InferenceBackend, backend.clone()),
            entry(Collection::InferenceProfile, profile.clone()),
            entry(Collection::Agent, agent(false)),
        ];
        let error = publish_home_config(&access, owner, "Agent", "default", true, disabled)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("must be enabled"),
            "{error:#}"
        );
        assert_eq!(node_document().await, None);

        let complete = vec![
            entry(Collection::InferenceBackend, backend),
            entry(Collection::InferenceProfile, profile),
            entry(Collection::Agent, agent(true)),
        ];
        publish_home_config(&access, owner, "Agent", "default", true, complete)
            .await
            .unwrap();
        assert_eq!(
            node_document().await.unwrap().default_agent_id.as_deref(),
            Some("default")
        );
    }

    async fn initialize_disposable_home(home: &Path, empty_model: bool) -> Result<Value> {
        let mut args = init_args();
        args.home = Some(home.to_path_buf());
        args.inference_endpoint = Some("http://127.0.0.1:9/v1".into());
        args.provider_kind = Some("OpenAiCompatible".into());
        args.api_key = Some("unused-init-test-key".into());
        if empty_model {
            args.model_name = Some(String::new());
        }
        init_home(args).await
    }

    #[tokio::test]
    async fn an_initialized_home_reopens_without_ending_the_callers_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("retained"), "keep").unwrap();
        let first = initialize_disposable_home(&home, false).await.unwrap();
        let second = initialize_disposable_home(&home, false).await.unwrap();
        assert_eq!(first["status"], "initialized");
        assert_eq!(second["status"], "initialized");
        assert_eq!(second["node_did"], first["node_did"]);
        assert_eq!(second["default_agent_id"], first["default_agent_id"]);
        assert_eq!(fs::read_to_string(home.join("retained")).unwrap(), "keep");
    }

    #[tokio::test]
    async fn a_failed_initialization_releases_the_store_for_a_valid_retry() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("retained"), "keep").unwrap();
        let error = initialize_disposable_home(&home, true).await.unwrap_err();
        assert!(error.to_string().contains("--model-name must not be empty"));
        let retried = initialize_disposable_home(&home, false).await.unwrap();
        assert_eq!(retried["status"], "initialized");
        assert_eq!(fs::read_to_string(home.join("retained")).unwrap(), "keep");
    }

    fn init_args() -> InitArgs {
        InitArgs {
            home: None,
            data_dir: None,
            dangerously_overwrite: false,
            reset: false,
            identity_only: false,
            node_name: "test-agent".to_string(),
            key_path: None,
            identity_backend: IdentityBackendArg::File,
            keychain_label: None,
            secure_enclave_label: None,
            store_key_custody: Some(crate::cli::args::StoreKeyCustodyArg::File),
            inference_endpoint: None,
            backend_id: None,
            backend_name: None,
            backend_preset: None,
            provider_kind: None,
            openai_wire_api: None,
            api_key: None,
            api_key_env_var: None,
            model_name: Some("test-model".to_string()),
            max_concurrent: Some(2),
            max_queue_depth: 16,
            write_tools: false,
            yolo: false,
            tool_package: None,
            engineer: false,
            tool_root: None,
            enable_memory: false,
            disable_defra_query: false,
            enable_defra_query: false,
            defra_query_collections: Vec::new(),
        }
    }
}
