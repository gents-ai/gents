// Soft-cap justified: thin entry point plus ~170 lines of constants and ~175 lines of co-located bootstrap tests. Further splitting would fragment binary-crate setup.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::ensure_runtime_schemas;
use serde::de::DeserializeOwned;
use serde::Serialize;

mod cli;
mod commands;
mod config_bundle;
mod config_import;
mod config_writes;
mod desired_state;
mod graphql_access;
mod home_state;
mod http;
mod request_helpers;
mod resolve_helpers;
mod shared;
mod telemetry;

use cli::*;
use shared::*;

use config_bundle::*;
use config_import::*;
use config_writes::ConfigAccess;
use graphql_access::*;
use home_state::*;
use request_helpers::*;
use resolve_helpers::*;

const DEFAULT_AGENT_NAME: &str = "default";
const DEFAULT_INIT_ENDPOINT: &str = "http://localhost:11434/v1";
const DEFAULT_INIT_MODEL_NAME: &str = "gemma4-26b-a4b";
const DEFAULT_HTTP_PORT: u16 = 9191;
const DEFAULT_P2P_MAX_CONCURRENT_DAG_FETCHES: usize = 4;
const DEFAULT_P2P_MAX_CONCURRENT_PUSH_TASKS: usize = 8;
const DEFAULT_P2P_RATE_LIMIT_BURST: u32 = 500;
const DEFAULT_P2P_RATE_LIMIT_RATE: f64 = 50.0;
const DEFAULT_LOG_FILTER: &str = concat!(
    "warn,",
    "defra_agent::agent::runtime=info,",
    "defra_agent::agent::daemon=info,",
    "defra_agent::agent::reconcile=info,",
    "defra_agent::hook=info,",
    "defra_agent::session::sessions=info,",
    "defra_agent::streaming=info,",
    "defra_agent::scheduler::loop_impl=info"
);
const INIT_CONFIG_FILE_NAME: &str = "init.json";
const RUNTIME_STATE_FILE_NAME: &str = "runtime.json";
const CLI_AFTER_HELP: &str = "\
Quick start:
  defra-agent init
  defra-agent server
  defra-agent chat

Inspect the local runtime:
  defra-agent status
  defra-agent show runtime
  defra-agent show response REQUEST_ID
  defra-agent reset

Update runtime documents:
  defra-agent config backend set ...
  defra-agent config behavior set ...
  defra-agent config tools set ...";
const INIT_AFTER_HELP: &str = "\
Bootstrap a local home directory with one default backend, one default behavior, and a safe read-only tool selection.

Examples:
  defra-agent init
  defra-agent init --inference-url http://HOST:PORT/v1 --model-name MODEL
  defra-agent init --backend-preset openrouter --model-name MODEL
  defra-agent init --backend-preset openai --model-name MODEL
  defra-agent init --inference-url $INFERENCE_ENDPOINT --model-name MODEL --write-tools

Next:
  ollama pull gemma4-26b-a4b
  defra-agent server
  defra-agent chat";
const RESET_AFTER_HELP: &str = "\
Examples:
  defra-agent reset
  defra-agent reset --home /path/to/home";
const SERVER_AFTER_HELP: &str = "\
`server` reads the initialized home directory, starts the embedded DefraDB runtime, serves GraphQL locally, and starts IROH P2P for desktop pairing.

Common flow:
  defra-agent init
  defra-agent server
  defra-agent-desktop init
  defra-agent-desktop
  defra-agent chat";
const CHAT_AFTER_HELP: &str = "\
Examples:
  defra-agent chat
  defra-agent chat \"summarize this repo\"
  defra-agent chat --session-id SESSION_ID \"continue the previous conversation\"

Diagnostics:
  defra-agent status
  defra-agent show response REQUEST_ID";
const P2P_AFTER_HELP: &str = "\
Examples:
  defra-agent p2p status
  defra-agent p2p peers --home /path/to/home
  defra-agent p2p connect --graphql http://127.0.0.1:9191/api/v0/graphql --peer <peer-id-or-address>
  defra-agent p2p collections add --profile chat-requests
  defra-agent p2p collections sync-versions --version-id <collection-version-id>
  defra-agent p2p replicators add --peer <peer-id-or-address> --profile runtime
  defra-agent p2p documents sync --collection AgentRequest --doc-id <doc-id>
  defra-agent p2p diagnose";
const STATUS_AFTER_HELP: &str = "\
Status reads the local runtime by default.

Examples:
  defra-agent status
  defra-agent status --home /path/to/home
  defra-agent status --graphql http://127.0.0.1:9191/api/v0/graphql";
const SHOW_AFTER_HELP: &str = "\
Examples:
  defra-agent show runtime
  defra-agent show request REQUEST_ID
  defra-agent show response REQUEST_ID";
const CONFIG_AFTER_HELP: &str = "\
Examples:
  defra-agent config validate --root infra/agents/default
  defra-agent config diff --root infra/agents/default --home /path/to/home
  defra-agent config apply --root infra/agents/default --home /path/to/home
  defra-agent config backend set --graphql URL --backend-id default-backend --name default-backend --backend-preset openrouter --max-concurrent 2
  defra-agent config backend discover-models --backend-preset openrouter
  defra-agent config behavior set --graphql URL --agent-did did:defra-agent:default --backend-id default-backend --model-name MODEL
  defra-agent config tools set --graphql URL --agent-did did:defra-agent:default --selection-id did:defra-agent:default:default:tools --enable-file-tools";
const REQUEST_AFTER_HELP: &str = "\
`request` is the low-level document path. Most users should prefer `defra-agent chat`.

Examples:
  defra-agent request submit --content \"summarize this repo\"
  defra-agent request show REQUEST_ID";
const RESPONSE_AFTER_HELP: &str = "\
Examples:
  defra-agent response wait REQUEST_ID
  defra-agent response show REQUEST_ID";
const DIAGNOSE_AFTER_HELP: &str = "\
Examples:
  defra-agent diagnose
  defra-agent diagnose --home /path/to/home
  defra-agent diagnose --graphql http://127.0.0.1:9191/api/v0/graphql";
const CONFIG_EXPORT_AFTER_HELP: &str = "\
Exports the desired configuration documents for one agent principal.

Examples:
  defra-agent config export > agent-config.json
  defra-agent config export --agent-did did:defra-agent:default > agent-config.json";
const CONFIG_IMPORT_AFTER_HELP: &str = "\
Imports desired configuration documents.

Default behavior is insert-only and will fail if a document already exists.
Use --override to switch to upsert mode.

Examples:
  defra-agent config import agent-config.json
  cat agent-config.json | defra-agent config import
  defra-agent config import agent-config.json --override";
pub(crate) const CONFIG_EXPORT_FORMAT_V1: &str = "defra-agent-config/v1";
pub(crate) const CONFIG_EXPORT_FORMAT: &str = "defra-agent-config/v2";

pub(crate) const SCHEMA_COLLECTION_CHECKS: &[(&str, &str)] = &[
    ("AgentPrincipal", "agent_did"),
    ("AgentBehavior", "behavior_id"),
    ("AgentRuntime", "agent_did"),
    ("ToolSelection", "selection_id"),
    ("InferenceProfile", "profile_id"),
    ("InferenceBackend", "backend_id"),
    ("AgentConversation", "session_id"),
    ("AgentRequest", "request_id"),
    ("AgentResponse", "request_id"),
    ("AgentToolResult", "agent_did"),
    ("AgentSession", "session_id"),
    ("AgentMessage", "message_key"),
    ("AgentToolCall", "tool_call_key"),
    ("CompactionEntry", "compaction_key"),
    ("ScheduledTask", "task_id"),
    ("ToolServiceRegistry", "service_id"),
];
const CONFIG_SCHEMA_COLLECTIONS: &[&str] = &[
    "AgentPrincipal",
    "AgentBehavior",
    "ToolSelection",
    "InferenceBackend",
];
pub(crate) const EXPORT_AGENT_PRINCIPAL_FIELDS: &str =
    "agent_did display_name default_behavior_id enabled created_at created_by";
pub(crate) const EXPORT_AGENT_BEHAVIOR_FIELDS: &str = "behavior_id agent_did display_name system_prompt backend_id model_name tool_selection_id inference_profile_id compaction_strategy compaction_threshold enabled created_at";
pub(crate) const EXPORT_TOOL_SELECTION_FIELDS: &str = "selection_id agent_did display_name enable_file_tools file_tools_mode file_tool_root enable_bash bash_mode cli_tool_names enable_meta_tools delegate_to";
pub(crate) const EXPORT_INFERENCE_BACKEND_FIELDS: &str =
    "backend_id name provider_kind endpoint api_key api_key_env_var max_concurrent max_queue_depth enabled models last_probe probe_status";
pub(crate) const EXPORT_INFERENCE_PROFILE_FIELDS: &str =
    "profile_id display_name context_window max_output_tokens max_turns temperature stream_batch_ms deadline_duration_secs";
pub(crate) const EXPORT_TOOL_SERVICE_REGISTRY_FIELDS: &str =
    "service_id display_name description hostname tailscale_ip lan_ip mcp_port mcp_path";
pub(crate) const EXPORT_SCHEDULED_TASK_FIELDS: &str =
    "task_id agent_did behavior_id name prompt interval_secs enabled";

#[tokio::main]
async fn main() -> Result<()> {
    let telemetry = telemetry::init(DEFAULT_LOG_FILTER)?;
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Init(args) => commands::init::init(args).await,
        Command::Reset(args) => commands::reset::reset(args).await,
        Command::Server(args) => commands::serve::serve(args).await,
        Command::Chat(args) => commands::chat::chat(args).await,
        Command::P2p { command } => commands::p2p::dispatch(command).await,
        Command::Show { command } => commands::show::dispatch(command).await,
        Command::Status(args) => commands::status::status(args).await,
        Command::Diagnose(args) => commands::diagnose::diagnose(args).await,
        Command::Config { command } => commands::config::dispatch(command).await,
        Command::Request { command } => commands::request::dispatch(command).await,
        Command::Response { command } => commands::response::dispatch(command).await,
    };
    telemetry.shutdown();
    result
}

pub(crate) fn expand_nonempty_values(values: &[String], flag_name: &str) -> Result<Vec<String>> {
    let values = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();

    if values.is_empty() {
        anyhow::bail!("provide at least one {flag_name}");
    }

    Ok(values.into_iter().collect())
}

pub(crate) async fn http_get_json<T: DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("sending GET request to {url}"))?;
    let status = response.status();
    let body = response
        .bytes()
        .await
        .with_context(|| format!("reading GET response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "GET {url} failed with {status}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    serde_json::from_slice(&body).with_context(|| format!("decoding JSON response from {url}"))
}

pub(crate) async fn http_post_json<B: Serialize>(
    client: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<()> {
    let response = client
        .post(url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("sending POST request to {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading POST response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "POST {url} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}

pub(crate) async fn http_delete_json<B: Serialize>(
    client: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<()> {
    let response = client
        .delete(url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("sending DELETE request to {url}"))?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("reading DELETE response body from {url}"))?;
    if !status.is_success() {
        anyhow::bail!(
            "DELETE {url} failed with {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(())
}

pub(crate) async fn resolve_config_access(
    home: Option<&Path>,
    explicit_graphql: Option<&str>,
    ensure_local_schemas: bool,
) -> Result<(ConfigAccess, PathBuf)> {
    let home_dir = resolve_home_dir(home);
    if let Some(graphql) = explicit_graphql
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok((ConfigAccess::Graphql(graphql.to_string()), home_dir));
    }
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        if graphql_endpoint_available(&runtime_state.graphql).await {
            return Ok((ConfigAccess::Graphql(runtime_state.graphql), home_dir));
        }
    }

    let data_dir = default_data_dir(&home_dir);
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let node = EmbeddedNode::builder()
        .data_path(&data_dir)
        .build()
        .await
        .with_context(|| format!("building embedded defra node from {}", data_dir.display()))?;
    if ensure_local_schemas {
        ensure_runtime_schemas(&node).await?;
    }
    Ok((ConfigAccess::Local(node), home_dir))
}

pub(crate) fn require_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--{field} must not be empty");
    }
    Ok(trimmed)
}

pub(crate) fn normalize_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .map(ToOwned::to_owned)
}

pub(crate) fn dangerously_overwrite_home(home_dir: &Path) -> Result<()> {
    if !home_dir.exists() {
        return Ok(());
    }

    if home_dir.as_os_str().is_empty() || home_dir == Path::new("/") {
        anyhow::bail!("refusing to dangerously overwrite {}", home_dir.display());
    }
    if let Some(user_home) = std::env::var_os("HOME").map(PathBuf::from) {
        if home_dir == user_home {
            anyhow::bail!(
                "refusing to dangerously overwrite the user home directory {}; pass a dedicated defra-agent home instead",
                home_dir.display()
            );
        }
    }

    fs::remove_dir_all(home_dir)
        .with_context(|| format!("dangerously overwriting {}", home_dir.display()))?;
    Ok(())
}

pub(crate) fn server_start_failure_hint(home_dir: &Path) -> String {
    format!(
        "Next:\n  1. For the default local backend, run `ollama pull {DEFAULT_INIT_MODEL_NAME}` and make sure Ollama is listening on {DEFAULT_INIT_ENDPOINT}\n  2. Point the backend elsewhere with `defra-agent config backend set --graphql http://127.0.0.1:{DEFAULT_HTTP_PORT}/api/v0/graphql --backend-id <ID> --name <NAME> --endpoint <URL> --max-concurrent 2`\n  3. Inspect the initialized home at {}\n  4. If persisted runtime state is stale, run `defra-agent reset --home {}`",
        init_config_path(home_dir).display(),
        home_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn sanitize_inference_backend_drops_deprecated_capability_fields() {
        let input = serde_json::json!({
            "backend_id": "local",
            "name": "Local",
            "provider_kind": "OpenAiCompatible",
            "endpoint": "http://127.0.0.1:11434/v1",
            "api_key": null,
            "api_key_env_var": null,
            "max_concurrent": 1,
            "max_queue_depth": 100,
            "enabled": true,
            "supports_tool_calls": true,
            "supports_streaming": true,
            "supports_structured_outputs": false,
            "supports_json_schema": false,
            "context_window": 32768,
            "max_output_tokens": 4096,
            "last_probe": "2026-04-15T00:00:00Z",
            "models": ["test-model"],
            "probe_status": "healthy"
        });

        let out = sanitize_import_document("InferenceBackend", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        for field in [
            "supports_tool_calls",
            "supports_streaming",
            "supports_structured_outputs",
            "supports_json_schema",
            "context_window",
            "max_output_tokens",
            "last_probe",
        ] {
            assert!(!obj.contains_key(field), "{field} should be stripped");
        }
        assert_eq!(obj.get("backend_id").and_then(Value::as_str), Some("local"));
    }

    #[test]
    fn read_config_import_bundle_migrates_v1_backend_capability_fields() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("config.json");
        fs::write(
            &path,
            serde_json::to_string(&serde_json::json!({
                "format": CONFIG_EXPORT_FORMAT_V1,
                "agent_did": "did:defra-agent:test",
                "exported_at": "2026-04-15T00:00:00Z",
                "access_mode": "local",
                "agent_principal": null,
                "agent_behaviors": [],
                "tool_selections": [],
                "inference_backends": [{
                    "backend_id": "local",
                    "name": "Local",
                    "provider_kind": "OpenAiCompatible",
                    "endpoint": "http://127.0.0.1:11434/v1",
                    "api_key": null,
                    "api_key_env_var": null,
                    "max_concurrent": 1,
                    "max_queue_depth": 100,
                    "enabled": true,
                    "supports_tool_calls": true,
                    "supports_streaming": true,
                    "supports_structured_outputs": false,
                    "supports_json_schema": false,
                    "models": ["test-model"],
                    "probe_status": "healthy"
                }],
                "inference_profiles": [],
                "tool_service_registries": [],
                "scheduled_tasks": []
            }))
            .unwrap(),
        )
        .unwrap();

        let bundle = read_config_import_bundle(Some(&path)).unwrap();
        validate_config_import_bundle(&bundle).unwrap();
        assert_eq!(bundle.format, CONFIG_EXPORT_FORMAT);
        let backend = bundle.inference_backends[0].as_object().unwrap();
        assert!(!backend.contains_key("supports_tool_calls"));
        assert!(!backend.contains_key("supports_streaming"));
        assert!(!backend.contains_key("supports_structured_outputs"));
        assert!(!backend.contains_key("supports_json_schema"));
    }

    #[test]
    fn sanitize_tool_service_registry_defaults_status_online_when_absent() {
        let input = serde_json::json!({
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "tailscale_ip": "100.69.4.79",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("online"));
    }

    #[test]
    fn sanitize_tool_service_registry_fills_status_when_null() {
        let input = serde_json::json!({
            "service_id": "observability-mcp",
            "status": null,
            "hostname": "studio-1",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("online"));
    }

    #[test]
    fn sanitize_tool_service_registry_preserves_explicit_status() {
        let input = serde_json::json!({
            "service_id": "observability-mcp",
            "status": "offline",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("status").and_then(|v| v.as_str()), Some("offline"));
    }

    #[test]
    fn sanitize_tool_service_registry_normalizes_address_fields_for_storage() {
        let input = serde_json::json!({
            "service_id": "observability-mcp",
            "hostname": null,
            "tailscale_ip": " 100.69.4.79 ",
            "lan_ip": null,
            "mcp_port": 9201,
            "mcp_path": "mcp"
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("hostname").and_then(|v| v.as_str()), Some(""));
        assert_eq!(obj.get("lan_ip").and_then(|v| v.as_str()), Some(""));
        assert_eq!(
            obj.get("tailscale_ip").and_then(|v| v.as_str()),
            Some("100.69.4.79")
        );
        assert_eq!(obj.get("mcp_path").and_then(|v| v.as_str()), Some("/mcp"));
    }

    #[test]
    fn sanitize_tool_service_registry_defaults_mcp_path() {
        let input = serde_json::json!({
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "mcp_port": 9201
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert_eq!(obj.get("mcp_path").and_then(|v| v.as_str()), Some("/mcp"));
    }

    #[test]
    fn sanitize_tool_service_registry_still_strips_runtime_owned_fields() {
        let input = serde_json::json!({
            "service_id": "observability-mcp",
            "mcp_port": 9201,
            "tools": [{"name": "x", "description": "y"}],
            "version": "1.2.3",
            "updated_at": "2026-04-14T00:00:00Z"
        });
        let out = sanitize_import_document("ToolServiceRegistry", &input, false).unwrap();
        let obj = out.as_object().unwrap();
        assert!(obj.get("tools").is_none(), "tools should be stripped");
        assert!(obj.get("version").is_none(), "version should be stripped");
        assert!(
            obj.get("updated_at").is_none(),
            "updated_at should be stripped on create"
        );
    }
}
