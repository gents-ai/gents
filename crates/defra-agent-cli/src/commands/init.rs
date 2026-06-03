use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_agent::config::{
    DEFAULT_CONTEXT_WINDOW, DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS,
    DEFAULT_MAX_TURNS, DEFAULT_STREAM_BATCH_MS,
};
use defra_agent::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    default_tool_selection_id_for_behavior, ensure_config_bootstrap_schemas, load_agent_behavior,
    load_agent_principal, load_or_create_macos_keychain_identity,
    load_or_create_macos_secure_enclave_identity, upsert_agent_principal, upsert_inference_profile,
    AgentBehaviorDocument, AgentIdentity, InferenceProfile, KeyIdentity, ToolSelectionDocument,
};
use serde::Serialize;
use serde_json::json;

use crate::cli::*;
use crate::config_writes::{
    write_agent_behavior_document, write_inference_backend_document, write_tool_selection_document,
    ConfigAccess, InferenceBackendUpsertDocument,
};
use crate::shared::*;
use crate::{
    clear_runtime_state, dangerously_overwrite_home, default_data_dir, default_key_path,
    format_tool_ceiling, normalize_optional_string, print_json, resolve_home_dir,
    write_init_config, BackendResolutionMode, DEFAULT_HTTP_PORT,
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

For long-running commands such as builds, test suites, installs, servers, and log tails, prefer background_tool with tool_name "bash_unrestricted" instead of shell backgrounding with "&". Use list_background_tools, read_tool_output, wait_tool, or cancel_tool to inspect, finish, or stop backgrounded work."#;

pub(crate) async fn init(args: InitArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    if args.dangerously_overwrite {
        dangerously_overwrite_home(&home_dir)?;
    }
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| default_data_dir(&home_dir));
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;

    if args.identity_only {
        if args.identity_backend != IdentityBackendArg::File && args.key_path.is_some() {
            anyhow::bail!("--key-path cannot be used with non-file identity backends");
        }
        let key_path = (args.identity_backend == IdentityBackendArg::File).then(|| {
            args.key_path
                .clone()
                .unwrap_or_else(|| default_key_path(&home_dir, &args.agent_name))
        });
        let summary = write_identity_only_home_metadata(IdentityOnlyHomeOptions {
            home: &home_dir,
            agent_name: &args.agent_name,
            key_path: key_path.as_deref(),
            identity_backend: args.identity_backend,
            keychain_label: args.keychain_label.as_deref(),
            secure_enclave_label: args.secure_enclave_label.as_deref(),
            write_tools: args.write_tools,
            tool_root: args.tool_root.as_deref(),
            reset: args.reset,
        })
        .await?;
        let output = json!({
            "status": "initialized",
            "identity_only": true,
            "home": summary.home,
            "agent_name": summary.agent_name,
            "agent_did": summary.agent_did,
            "key_path": summary.key_path,
            "tool_ceiling": format_tool_ceiling(summary.tool_ceiling),
            "tool_root": summary.tool_root,
            "runtime_state_reset": summary.runtime_state_reset,
            "identity": {
                "agent_did": summary.agent_did,
                "key_path": summary.key_path,
                "identity_backend": summary.identity_backend,
                "keychain_label": summary.keychain_label,
                "secure_enclave_label": summary.secure_enclave_label,
                "permission_boundary": "This DID and key identify the permission boundary for every action the agent runtime performs."
            },
            "next_steps": [
                "defra-agent config apply --root <manifest-root> --home <home> --bind-agent-did home",
                "defra-agent server"
            ],
            "init": null
        });
        print_json(&output)?;
        return Ok(());
    }

    let initialized_identity = load_or_create_home_identity(HomeIdentityOptions {
        home: &home_dir,
        agent_name: &args.agent_name,
        key_path: args.key_path.as_deref(),
        identity_backend: args.identity_backend,
        keychain_label: args.keychain_label.as_deref(),
        secure_enclave_label: args.secure_enclave_label.as_deref(),
    })?;
    initialized_identity
        .identity
        .sign(b"defra-agent init identity")
        .await
        .context("creating or loading agent identity key")?;

    let mut node_builder = crate::persistent_node_builder(&data_dir);
    if let Some(node_identity_did) = initialized_identity.node_identity_did.as_ref() {
        node_builder = node_builder.with_node_identity_did(node_identity_did.clone());
    }
    let node = node_builder
        .build()
        .await
        .context("building embedded defra node for init")?;
    ensure_config_bootstrap_schemas(&node).await?;

    let access = ConfigAccess::Local(node);
    let summary =
        initialize_runtime_home(&access, &args, initialized_identity.identity.did()).await?;
    let stored = StoredInitConfig {
        home: home_dir.to_string_lossy().to_string(),
        agent_name: args.agent_name.clone(),
        agent_did: initialized_identity.identity.did().to_string(),
        key_path: initialized_identity.key_path.clone(),
        identity_backend: initialized_identity.identity_backend.clone(),
        keychain_label: initialized_identity.keychain_label.clone(),
        secure_enclave_label: initialized_identity.secure_enclave_label.clone(),
        tool_ceiling: summary.tool_ceiling,
        tool_root: summary.tool_root.clone(),
    };
    write_init_config(&home_dir, &stored)?;
    let runtime_state_reset = if args.reset {
        clear_runtime_state(&home_dir)?
    } else {
        false
    };

    let output = json!({
        "status": "initialized",
        "home": home_dir,
        "agent_name": args.agent_name,
        "agent_did": initialized_identity.identity.did(),
        "key_path": initialized_identity.key_path,
        "identity_backend": initialized_identity.identity_backend,
        "keychain_label": initialized_identity.keychain_label,
        "secure_enclave_label": initialized_identity.secure_enclave_label,
        "default_behavior_id": summary.default_behavior_id,
        "tool_selection_id": summary.tool_selection_id,
        "inference_profile_id": summary.inference_profile_id,
        "tool_ceiling": format_tool_ceiling(summary.tool_ceiling),
        "tool_root": summary.tool_root,
        "runtime_state_reset": runtime_state_reset,
        "identity": {
            "agent_did": initialized_identity.identity.did(),
            "key_path": stored.key_path,
            "identity_backend": stored.identity_backend,
            "keychain_label": stored.keychain_label,
            "secure_enclave_label": stored.secure_enclave_label,
            "permission_boundary": "This DID and key identify the permission boundary for every action the agent runtime performs."
        },
        "next_steps": init_next_steps(&summary),
        "init": summary,
    });
    print_json(&output)?;

    Ok(())
}

pub(crate) struct IdentityOnlyHomeOptions<'a> {
    pub(crate) home: &'a Path,
    pub(crate) agent_name: &'a str,
    pub(crate) key_path: Option<&'a Path>,
    pub(crate) identity_backend: IdentityBackendArg,
    pub(crate) keychain_label: Option<&'a str>,
    pub(crate) secure_enclave_label: Option<&'a str>,
    pub(crate) write_tools: bool,
    pub(crate) tool_root: Option<&'a Path>,
    pub(crate) reset: bool,
}

struct HomeIdentityOptions<'a> {
    home: &'a Path,
    agent_name: &'a str,
    key_path: Option<&'a Path>,
    identity_backend: IdentityBackendArg,
    keychain_label: Option<&'a str>,
    secure_enclave_label: Option<&'a str>,
}

struct HomeIdentity {
    identity: Arc<dyn AgentIdentity>,
    key_path: Option<String>,
    identity_backend: Option<String>,
    keychain_label: Option<String>,
    secure_enclave_label: Option<String>,
    node_identity_did: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct IdentityOnlyHomeSummary {
    pub(crate) home: String,
    pub(crate) agent_name: String,
    pub(crate) agent_did: String,
    pub(crate) key_path: Option<String>,
    pub(crate) identity_backend: Option<String>,
    pub(crate) keychain_label: Option<String>,
    pub(crate) secure_enclave_label: Option<String>,
    pub(crate) tool_ceiling: ToolCeilingArg,
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
        agent_name: options.agent_name,
        key_path: options.key_path,
        identity_backend: options.identity_backend,
        keychain_label: options.keychain_label,
        secure_enclave_label: options.secure_enclave_label,
    })?;
    initialized_identity
        .identity
        .sign(b"defra-agent init identity")
        .await
        .context("creating or loading agent identity key")?;

    let tool_ceiling = if options.write_tools {
        ToolCeilingArg::Readwrite
    } else {
        ToolCeilingArg::Readonly
    };
    let tool_root = Some(
        resolve_default_tool_root(options.tool_root)?
            .to_string_lossy()
            .to_string(),
    );
    let stored = StoredInitConfig {
        home: options.home.to_string_lossy().to_string(),
        agent_name: options.agent_name.to_string(),
        agent_did: initialized_identity.identity.did().to_string(),
        key_path: initialized_identity.key_path.clone(),
        identity_backend: initialized_identity.identity_backend.clone(),
        keychain_label: initialized_identity.keychain_label.clone(),
        secure_enclave_label: initialized_identity.secure_enclave_label.clone(),
        tool_ceiling,
        tool_root: tool_root.clone(),
    };
    write_init_config(options.home, &stored)?;
    let runtime_state_reset = if options.reset {
        clear_runtime_state(options.home)?
    } else {
        false
    };

    Ok(IdentityOnlyHomeSummary {
        home: options.home.to_string_lossy().to_string(),
        agent_name: options.agent_name.to_string(),
        agent_did: initialized_identity.identity.did().to_string(),
        key_path: initialized_identity.key_path,
        identity_backend: initialized_identity.identity_backend,
        keychain_label: initialized_identity.keychain_label,
        secure_enclave_label: initialized_identity.secure_enclave_label,
        tool_ceiling,
        tool_root,
        runtime_state_reset,
    })
}

fn load_or_create_home_identity(options: HomeIdentityOptions<'_>) -> Result<HomeIdentity> {
    match options.identity_backend {
        IdentityBackendArg::File => {
            let key_path = options
                .key_path
                .map(Path::to_path_buf)
                .unwrap_or_else(|| default_key_path(options.home, options.agent_name));
            if let Some(parent) = key_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("creating key directory {}", parent.display()))?;
            }
            let identity = Arc::new(
                KeyIdentity::load_or_create(&key_path, None)
                    .context("creating or loading agent identity key")?,
            );
            Ok(HomeIdentity {
                identity,
                key_path: Some(key_path.to_string_lossy().to_string()),
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                node_identity_did: None,
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
    agent_did: &str,
) -> Result<InitSummary> {
    let ConfigAccess::Local(node) = access else {
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
    let model_name = args.model_name.trim();
    if model_name.is_empty() {
        anyhow::bail!("--model-name must not be empty");
    }
    let backend = resolve_init_backend_config(args)?;
    let backend_id_was_generated = explicit_backend_id.is_none();
    let backend_id = explicit_backend_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default_backend_id_for_agent(agent_did));
    let backend_name = explicit_backend_name
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            if backend_id_was_generated {
                format!("{} backend", args.agent_name)
            } else {
                backend_id.clone()
            }
        });
    let existing_principal = load_agent_principal(node, agent_did).await?;
    let default_behavior_id = existing_principal
        .as_ref()
        .and_then(|principal| normalize_optional_string(principal.default_behavior_id.as_deref()))
        .unwrap_or_else(|| default_behavior_id_for_agent(agent_did));
    let existing_default_behavior = load_agent_behavior(node, &default_behavior_id).await?;
    if let Some(behavior) = existing_default_behavior.as_ref() {
        if behavior.agent_did != agent_did {
            anyhow::bail!(
                "AgentBehavior {} belongs to {} not {}",
                default_behavior_id,
                behavior.agent_did,
                agent_did
            );
        }
    }
    let principal_display_name = existing_principal
        .as_ref()
        .and_then(|principal| normalize_optional_string(principal.display_name.as_deref()))
        .unwrap_or_else(|| args.agent_name.clone());
    let principal_enabled = existing_principal
        .as_ref()
        .map(|principal| principal.enabled)
        .unwrap_or(true);
    upsert_agent_principal(
        node,
        agent_did,
        Some(&principal_display_name),
        Some(&default_behavior_id),
        principal_enabled,
    )
    .await?;
    let tool_selection_id = default_tool_selection_id_for_behavior(&default_behavior_id);
    let tool_ceiling = if args.write_tools {
        ToolCeilingArg::Readwrite
    } else {
        ToolCeilingArg::Readonly
    };
    let tool_root = Some(resolve_default_tool_root(args.tool_root.as_deref())?);
    let backend_doc = InferenceBackendUpsertDocument {
        backend_id: backend_id.clone(),
        name: backend_name.clone(),
        provider_kind: backend.provider_kind,
        endpoint: backend.endpoint.clone(),
        api_key: backend.api_key.clone(),
        api_key_env_var: backend.api_key_env_var.clone(),
        max_concurrent: args.max_concurrent,
        max_queue_depth: args.max_queue_depth,
        enabled: true,
        models_on_add: vec![model_name.to_string()],
        models_on_update: Some(vec![model_name.to_string()]),
        probe_status: "healthy".to_string(),
    };
    write_inference_backend_document(access, &backend_doc).await?;

    let tool_selection = standard_tool_selection(agent_did, &tool_selection_id, tool_ceiling);
    write_tool_selection_document(access, &tool_selection).await?;

    let inference_profile_id = default_inference_profile_id_for_behavior(&default_behavior_id);
    let inference_profile = standard_inference_profile(&inference_profile_id);
    upsert_inference_profile(node, &inference_profile).await?;

    let behavior = AgentBehaviorDocument {
        behavior_id: default_behavior_id.clone(),
        agent_did: agent_did.to_string(),
        display_name: Some("Default".to_string()),
        system_prompt: Some(standard_system_prompt(tool_ceiling).to_string()),
        backend_id: Some(backend_id.clone()),
        model_name: Some(model_name.to_string()),
        tool_selection_id: Some(tool_selection_id.clone()),
        inference_profile_id: Some(inference_profile_id.clone()),
        compaction_strategy: None,
        compaction_threshold: None,
        enabled: true,
        skill_refs: Vec::new(),
        skill_excludes: Vec::new(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    write_agent_behavior_document(access, &behavior).await?;

    Ok(InitSummary {
        backend_id,
        backend_name,
        provider_kind: backend.provider_kind,
        endpoint: backend.endpoint,
        api_key: backend.api_key.map(|_| "<redacted>".to_string()),
        api_key_env_var: backend.api_key_env_var,
        model_name: model_name.to_string(),
        max_concurrent: args.max_concurrent,
        max_queue_depth: args.max_queue_depth,
        default_behavior_id,
        tool_selection_id,
        inference_profile_id,
        tool_ceiling,
        tool_root: tool_root.map(|path| path.to_string_lossy().to_string()),
        created_principal: existing_principal.is_none(),
        created_default_behavior: existing_default_behavior.is_none(),
    })
}

fn standard_tool_selection(
    agent_did: &str,
    tool_selection_id: &str,
    tool_ceiling: ToolCeilingArg,
) -> ToolSelectionDocument {
    let (display_name, file_tools_mode, bash_mode) = match tool_ceiling {
        ToolCeilingArg::Readwrite => ("Standard Write Tools", "ReadWrite", "Unrestricted"),
        ToolCeilingArg::MetaOnly | ToolCeilingArg::Readonly => {
            ("Standard Read-Only Tools", "ReadOnly", "ReadOnly")
        }
    };
    ToolSelectionDocument {
        selection_id: tool_selection_id.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some(display_name.to_string()),
        enable_file_tools: Some(true),
        file_tools_mode: Some(file_tools_mode.to_string()),
        file_tool_root: None,
        enable_bash: Some(true),
        bash_mode: Some(bash_mode.to_string()),
        command_execution_policy: default_command_execution_policy_for_init(tool_ceiling),
        command_allowed_argv_prefixes: Some(Vec::new()),
        command_forbidden_argv_prefixes: Some(Vec::new()),
        command_network_mode: None,
        cli_tool_names: Some(Vec::new()),
        enable_meta_tools: Some(true),
        allowed_mcp_service_ids: Some(Vec::new()),
        delegate_to: Some(Vec::new()),
        backgroundable_tool_names: Some(default_backgroundable_tool_names(tool_ceiling)),
        subagent_targets: Some(Vec::new()),
        subagent_spawn_enabled: Some(false),
        subagent_steering_enabled: Some(false),
        subagent_background_enabled: Some(false),
        cross_deployment_spawn_timeout_seconds: None,
        enable_defra_query: None,
        defra_query_collections: None,
    }
}

fn default_command_execution_policy_for_init(tool_ceiling: ToolCeilingArg) -> Option<String> {
    match tool_ceiling {
        ToolCeilingArg::Readwrite if cfg!(target_os = "macos") => {
            Some("workspace_write".to_string())
        }
        ToolCeilingArg::Readwrite => Some("unrestricted".to_string()),
        ToolCeilingArg::MetaOnly | ToolCeilingArg::Readonly => None,
    }
}

fn default_backgroundable_tool_names(tool_ceiling: ToolCeilingArg) -> Vec<String> {
    match tool_ceiling {
        ToolCeilingArg::Readwrite => vec!["bash_unrestricted".to_string()],
        ToolCeilingArg::Readonly | ToolCeilingArg::MetaOnly => Vec::new(),
    }
}

fn standard_inference_profile(profile_id: &str) -> InferenceProfile {
    InferenceProfile {
        profile_id: profile_id.to_string(),
        display_name: Some("Default".to_string()),
        context_window: Some(DEFAULT_CONTEXT_WINDOW as i64),
        max_output_tokens: Some(DEFAULT_MAX_OUTPUT_TOKENS as i64),
        max_turns: Some(DEFAULT_MAX_TURNS as i64),
        temperature: Some(0.0),
        stream_batch_ms: Some(DEFAULT_STREAM_BATCH_MS as i64),
        deadline_duration_secs: Some(DEFAULT_DEADLINE_DURATION_SECS as i64),
    }
}

fn default_backend_id_for_agent(agent_did: &str) -> String {
    format!("{agent_did}:backend")
}

fn standard_system_prompt(tool_ceiling: ToolCeilingArg) -> &'static str {
    match tool_ceiling {
        ToolCeilingArg::Readwrite => STANDARD_READWRITE_SYSTEM_PROMPT,
        ToolCeilingArg::MetaOnly | ToolCeilingArg::Readonly => STANDARD_READONLY_SYSTEM_PROMPT,
    }
}

fn init_next_steps(summary: &InitSummary) -> Vec<String> {
    let mut steps = Vec::new();
    if is_probably_ollama_endpoint(&summary.endpoint) {
        steps.push(format!("ollama pull {}", summary.model_name));
    }
    steps.push("defra-agent server".to_string());
    steps.push("defra-agent chat".to_string());
    steps.push(format!(
        "defra-agent config backend set --graphql http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql --backend-id {} --name {} --endpoint <URL> --max-concurrent {}",
        summary.backend_id, summary.backend_name, summary.max_concurrent
    ));
    steps
}

fn is_probably_ollama_endpoint(endpoint: &str) -> bool {
    endpoint.contains("localhost:11434") || endpoint.contains("127.0.0.1:11434")
}

fn resolve_init_backend_config(args: &InitArgs) -> Result<ResolvedBackendConfig> {
    crate::resolve_backend_config_with_preset(
        args.backend_preset,
        args.resolved_inference_endpoint(),
        args.provider_kind.as_deref(),
        args.api_key.as_deref(),
        args.api_key_env_var.as_deref(),
        BackendResolutionMode::Init,
    )
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
