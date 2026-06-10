use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::{
    ensure_runtime_schemas, load_macos_keychain_identity, load_macos_secure_enclave_identity,
    AgentIdentity, DefraAgent, DocumentRuntimeOptions, KeyIdentity, McpPool,
    ProcessLifecycleObserver, ProcessLifecycleState, ToolCeiling,
};
use serde_json::{json, Value};
use tokio::sync::watch;

use crate::cli::*;
use crate::commands::codex_shim::{bind_codex_shim, CodexShimBindArgs};
use crate::http::runtime_contract_router;
use crate::shared::*;
use crate::{
    default_data_dir, default_key_path, display_host, format_tool_ceiling, parse_cli_tool_arg,
    print_json, read_init_config, resolve_home_dir, server_start_failure_hint, write_runtime_state,
    DEFAULT_AGENT_NAME,
};

pub(crate) struct CliReadyObserver {
    pub(crate) tx: watch::Sender<ProcessLifecycleState>,
}

impl ProcessLifecycleObserver for CliReadyObserver {
    fn on_process_state_change(&self, state: ProcessLifecycleState) {
        let _ = self.tx.send(state);
    }
}

pub(crate) async fn serve(args: ServeArgs) -> Result<()> {
    let home_dir = resolve_home_dir(args.home.as_deref());
    let data_dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| default_data_dir(&home_dir));
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let http_addr = SocketAddr::new(args.http_addr, args.http_port);
    let graphql_url = format!(
        "http://{}:{}/api/v0/graphql",
        display_host(args.http_addr),
        args.http_port
    );
    let init_config = read_init_config(&home_dir)?;
    if let (Some(explicit), Some(config)) = (args.agent_name.as_deref(), init_config.as_ref()) {
        if explicit != config.agent_name {
            anyhow::bail!(
                "--agent-name {} does not match initialized home agent {}",
                explicit,
                config.agent_name
            );
        }
    }

    let local_hostname = hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    let agent_name = args
        .agent_name
        .clone()
        .or_else(|| init_config.as_ref().map(|config| config.agent_name.clone()))
        .unwrap_or_else(|| DEFAULT_AGENT_NAME.to_string());
    let server_identity =
        resolve_server_identity(&args, init_config.as_ref(), &home_dir, &agent_name)?;
    let identity = server_identity.identity;
    let effective_tool_ceiling = args
        .tool_ceiling
        .or_else(|| init_config.as_ref().map(|config| config.tool_ceiling))
        .unwrap_or(ToolCeilingArg::MetaOnly);
    let configured_tool_root = args.tool_root.clone().or_else(|| {
        init_config
            .as_ref()
            .and_then(|config| config.tool_root.as_ref().map(PathBuf::from))
    });
    let effective_tool_root = match effective_tool_ceiling {
        ToolCeilingArg::MetaOnly => configured_tool_root,
        ToolCeilingArg::Readonly => Some(match configured_tool_root {
            Some(root) => root,
            None => resolve_default_tool_root(None)?,
        }),
        ToolCeilingArg::Readwrite => Some(configured_tool_root.ok_or_else(|| {
            anyhow::anyhow!("--tool-root is required when --tool-ceiling readwrite")
        })?),
    };
    let mut tool_ceiling = match effective_tool_ceiling {
        ToolCeilingArg::MetaOnly => ToolCeiling::meta_only(),
        ToolCeilingArg::Readonly => ToolCeiling::readonly_at(
            effective_tool_root
                .as_ref()
                .expect("readonly root resolved"),
        ),
        ToolCeilingArg::Readwrite => ToolCeiling::readwrite(
            effective_tool_root
                .as_ref()
                .expect("readwrite root resolved"),
        ),
    };
    for cli_tool_arg in &args.cli_tools {
        tool_ceiling = tool_ceiling.with_cli_tool(parse_cli_tool_arg(cli_tool_arg)?);
    }

    let p2p_config = resolve_server_p2p_config(&home_dir, &args)?;
    // The MCP `defra_query` endpoint is opt-in (unauthenticated read surface).
    let mcp_query_scope = if !args.enable_mcp {
        None
    } else if args.mcp_query_collections.is_empty() {
        Some(defra_agent::defra_query::CollectionScope::all())
    } else {
        Some(defra_agent::defra_query::CollectionScope::restricted(
            args.mcp_query_collections.clone(),
        ))
    };
    let mut node_builder = crate::persistent_node_builder(&data_dir).with_http(
        defra_node::HttpConfig::with_addr(http_addr).with_extra_routes(runtime_contract_router(
            graphql_url.clone(),
            agent_name.clone(),
            identity.did().to_string(),
            mcp_query_scope,
        )),
    );
    if let Some(node_identity_did) = server_identity.node_identity_did.as_ref() {
        node_builder = node_builder.with_node_identity_did(node_identity_did.clone());
    }
    if let Some(config) = p2p_config {
        node_builder = node_builder.with_p2p(config);
    }
    let node = Arc::new(
        node_builder
            .build()
            .await
            .context("building embedded defra node")?,
    );
    ensure_runtime_schemas(node.as_ref()).await?;
    defra_agent::migration::ensure_tool_call_migrations(node.clone()).await?;
    defra_agent::migration::ensure_subagent_extensions_migrations(node.clone()).await?;
    // ensure_agent_behavior_migrations is now called inside
    // DefraAgent::from_default_behavior_documents (below), so the explicit
    // call that was here (added in commit b13d7cd8) is no longer needed.
    let (ready_tx, mut ready_rx) = watch::channel(ProcessLifecycleState::Uninitialized);

    let agent = DefraAgent::from_default_behavior_documents(
        node.clone(),
        identity.clone(),
        DocumentRuntimeOptions {
            mcp_pool: McpPool::new(),
            local_hostname: Some(local_hostname),
            tool_ceiling,
            process_state_observer: Some(Arc::new(CliReadyObserver { tx: ready_tx })),
            ..Default::default()
        },
    )
    .await
    .with_context(|| {
        format!(
            "starting defra-agent server from {}\n{}",
            home_dir.display(),
            server_start_failure_hint(&home_dir)
        )
    })?;
    let runnable_behaviors = agent
        .behaviors()
        .iter()
        .map(|behavior| {
            json!({
                "behavior_id": behavior.behavior_id,
                "backend_id": behavior.backend_id,
                "model_name": behavior.model_name,
            })
        })
        .collect::<Vec<_>>();
    let default_behavior_id = agent.default_behavior_id().to_string();
    let unavailable_behaviors = agent.unavailable_behaviors().clone();
    let behavior_readiness = if unavailable_behaviors.is_empty() {
        "ready"
    } else {
        "degraded"
    };
    let background_execution_registry = agent.background_execution_registry();

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = shutdown_tx.send(true);
        }
    });

    let mut run_handle = tokio::spawn(agent.run(shutdown_rx));
    loop {
        if *ready_rx.borrow() == ProcessLifecycleState::Ready {
            break;
        }

        tokio::select! {
            changed = ready_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            joined = &mut run_handle => {
                let result = joined.context("joining defra-agent runtime task")?;
                return result;
            }
        }
    }

    let p2p_status = load_local_server_p2p_status(node.as_ref(), P2pTransportArg::Iroh).await?;
    write_runtime_state(
        &home_dir,
        &StoredRuntimeState {
            home: home_dir.to_string_lossy().to_string(),
            graphql: graphql_url.clone(),
            agent_name: agent_name.clone(),
            agent_did: identity.did().to_string(),
            default_behavior_id: default_behavior_id.clone(),
            p2p_transport: p2p_status
                .get("p2p_transport")
                .and_then(Value::as_str)
                .unwrap_or(P2pTransportArg::None.as_str())
                .to_string(),
            p2p_peer_id: p2p_status
                .get("p2p_peer_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            p2p_listen_addresses: p2p_status
                .get("p2p_listen_addresses")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        },
    )?;

    let mut codex_shim_output = None;
    // The shim runs by default, so its preconditions (free port, behavior with
    // an inference profile) must not take the whole runtime down: degrade to
    // a disabled endpoint and keep serving.
    let mut codex_shim_handle = if args.no_codex_shim {
        None
    } else if let Some(bound) = match bind_codex_shim(CodexShimBindArgs {
        home: home_dir.clone(),
        fs_root: effective_tool_root.clone(),
        node: node.clone(),
        background_execution_registry: background_execution_registry.clone(),
        graphql: graphql_url.clone(),
        agent_did: identity.did().to_string(),
        behavior_id: args.codex_shim_behavior_id.clone(),
        bind_addr: args.codex_shim_bind_addr,
        port: args.codex_shim_port,
        timeout_secs: args.codex_shim_timeout_secs,
        poll_ms: args.codex_shim_poll_ms,
    })
    .await
    {
        Ok(bound) => Some(bound),
        Err(error) => {
            eprintln!("Codex endpoint disabled: {error:#}");
            eprintln!(
                "The server keeps running without it. Fix the cause and restart, pick another port with --codex-shim-port, or silence this with --no-codex-shim."
            );
            codex_shim_output = Some(json!({
                "disabled": true,
                "reason": format!("{error:#}"),
            }));
            None
        }
    } {
        let codex_shim_url = format!(
            "ws://{}:{}/",
            display_shim_host(bound.addr().ip()),
            bound.addr().port()
        );
        eprintln!(
            "Codex shim is running on {codex_shim_url} with state dir {}",
            bound.codex_home().display(),
        );
        eprintln!("Codex shim event log: {}", bound.trace_path().display());
        eprintln!("Chat from another terminal with: defra-agent codex");
        if codex_shim_url != crate::DEFAULT_CODEX_REMOTE {
            eprintln!("  (this shim is not on the default address; pass --remote {codex_shim_url})");
        }
        if args.codex_shim_bind_addr.is_loopback() {
            eprintln!(
                "For another device, restart with --codex-shim-bind-addr <trusted-private-or-tailscale-ip> and use that host in --remote."
            );
        } else {
            eprintln!(
                "Codex shim has no transport authentication; only expose this address on a trusted private network."
            );
        }
        codex_shim_output = Some(json!({
            "websocket": codex_shim_url,
            "launch_command": if codex_shim_url == crate::DEFAULT_CODEX_REMOTE {
                "defra-agent codex".to_string()
            } else {
                format!("defra-agent codex --remote {codex_shim_url}")
            },
            "shim_home": bound.codex_home().to_path_buf(),
            "codex_home": bound.codex_home().to_path_buf(),
            "event_log": bound.trace_path().to_path_buf(),
        }));
        Some(bound.spawn())
    } else {
        None
    };

    let output = json!({
        "status": "serving",
        "behavior_readiness": behavior_readiness,
        "home": home_dir,
        "agent_name": agent_name,
        "agent_did": identity.did(),
        "default_behavior_id": default_behavior_id,
        "tool_ceiling": format_tool_ceiling(effective_tool_ceiling),
        "tool_root": effective_tool_root,
        "runnable_behaviors": runnable_behaviors,
        "unavailable_behaviors": unavailable_behaviors,
        "graphql": graphql_url,
        "p2p_transport": p2p_status.get("p2p_transport").cloned().unwrap_or(Value::String(default_p2p_transport())),
        "p2p_peer_id": p2p_status.get("p2p_peer_id").cloned().unwrap_or(Value::Null),
        "p2p_listen_addresses": p2p_status.get("p2p_listen_addresses").cloned().unwrap_or_else(|| json!([])),
        "codex_shim": codex_shim_output,
    });
    print_json(&output)?;
    eprintln!(
        "defra-agent server is running with IROH P2P. Press Ctrl-C to stop. For the desktop demo, run `defra-agent-desktop init`, launch `defra-agent-desktop`, wait for `replication: subscriptions armed`, then chat."
    );

    if let Some(handle) = codex_shim_handle.as_mut() {
        tokio::select! {
            result = &mut run_handle => {
                result.context("joining defra-agent runtime task")?
            }
            result = handle => {
                result.context("joining Codex shim task")?
                    .context("Codex shim task failed")?;
                Ok(())
            }
        }
    } else {
        run_handle
            .await
            .context("joining defra-agent runtime task")?
    }
}

struct ServerIdentity {
    identity: Arc<dyn AgentIdentity>,
    node_identity_did: Option<String>,
}

fn resolve_server_identity(
    args: &ServeArgs,
    init_config: Option<&StoredInitConfig>,
    home_dir: &Path,
    agent_name: &str,
) -> Result<ServerIdentity> {
    if let Some(config) = init_config {
        let agent_did = config.agent_did.trim();
        if is_real_agent_did(agent_did)
            && args.key_path.is_none()
            && config
                .key_path
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
        {
            return resolve_no_key_server_identity(config, home_dir);
        }
    }

    let key_path = resolve_server_key_path(args, init_config, home_dir, agent_name)?;
    ensure_key_path_exists_for_initialized_did(init_config, &key_path)?;
    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating key directory {}", parent.display()))?;
    }
    let identity = Arc::new(
        KeyIdentity::load_or_create(&key_path, None)
            .context("creating or loading agent identity key")?,
    );
    ensure_identity_matches_init_config(init_config, identity.did())?;
    Ok(ServerIdentity {
        identity,
        node_identity_did: None,
    })
}

fn resolve_no_key_server_identity(
    config: &StoredInitConfig,
    home_dir: &Path,
) -> Result<ServerIdentity> {
    let backend = config
        .identity_backend
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "initialized home {} has agent DID {} but no key_path or identity_backend",
                home_dir.display(),
                config.agent_did
            )
        })?;
    match backend {
        "macos-keychain" => {
            let label = config
                .keychain_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "initialized home {} uses macos-keychain but has no keychain_label",
                        home_dir.display()
                    )
                })?;
            let identity = Arc::new(
                load_macos_keychain_identity(label, None)
                    .with_context(|| format!("loading macOS keychain identity {label}"))?,
            );
            ensure_identity_matches_init_config(Some(config), identity.did())?;
            Ok(ServerIdentity {
                node_identity_did: Some(identity.did().to_string()),
                identity,
            })
        }
        "macos-secure-enclave" => {
            let label = config
                .secure_enclave_label
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "initialized home {} uses macos-secure-enclave but has no secure_enclave_label",
                        home_dir.display()
                    )
                })?;
            let identity = Arc::new(
                load_macos_secure_enclave_identity(label, None)
                    .with_context(|| format!("loading macOS Secure Enclave identity {label}"))?,
            );
            ensure_identity_matches_init_config(Some(config), identity.did())?;
            Ok(ServerIdentity {
                node_identity_did: Some(identity.did().to_string()),
                identity,
            })
        }
        other => anyhow::bail!(
            "initialized home {} uses unsupported identity_backend {other:?} without key_path",
            home_dir.display()
        ),
    }
}

fn default_p2p_transport() -> String {
    P2pTransportArg::Iroh.as_str().to_string()
}

fn resolve_server_key_path(
    args: &ServeArgs,
    init_config: Option<&StoredInitConfig>,
    home_dir: &Path,
    agent_name: &str,
) -> Result<PathBuf> {
    if let Some(path) = args.key_path.clone() {
        return Ok(path);
    }

    if let Some(config) = init_config {
        if let Some(path) = config
            .key_path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Ok(PathBuf::from(path));
        }
    }

    Ok(default_key_path(home_dir, agent_name))
}

fn ensure_identity_matches_init_config(
    init_config: Option<&StoredInitConfig>,
    resolved_did: &str,
) -> Result<()> {
    let Some(config) = init_config else {
        return Ok(());
    };
    if is_real_agent_did(&config.agent_did) && config.agent_did.trim() != resolved_did {
        anyhow::bail!(
            "initialized home agent DID {} does not match loaded identity DID {}; repair init.json or use the correct --key-path",
            config.agent_did,
            resolved_did
        );
    }
    Ok(())
}

fn ensure_key_path_exists_for_initialized_did(
    init_config: Option<&StoredInitConfig>,
    key_path: &Path,
) -> Result<()> {
    let Some(config) = init_config else {
        return Ok(());
    };
    if is_real_agent_did(&config.agent_did) && !key_path.exists() {
        anyhow::bail!(
            "initialized home agent DID {} requires identity key {} to already exist; restore the configured key, pass --key-path for the matching key, or bootstrap the host identity backend first",
            config.agent_did,
            key_path.display()
        );
    }
    Ok(())
}

fn is_real_agent_did(did: &str) -> bool {
    let did = did.trim();
    !did.is_empty() && !did.starts_with("did:defra-agent:")
}

fn default_p2p_secret_key_path(home_dir: &Path) -> PathBuf {
    home_dir.join("p2p-secret-key")
}

fn resolve_server_p2p_config(
    home_dir: &Path,
    args: &ServeArgs,
) -> Result<Option<defra_node::P2PConfig>> {
    let secret_key_path = args
        .p2p_secret_key_path
        .clone()
        .unwrap_or_else(|| default_p2p_secret_key_path(home_dir));
    if let Some(parent) = secret_key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating P2P key directory {}", parent.display()))?;
    }
    Ok(Some(defra_node::P2PConfig {
        port: args.p2p_port.unwrap_or(0),
        bind_addr: Some(
            args.p2p_bind_addr
                .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        ),
        relay_mode: match args.p2p_relay_mode {
            P2pRelayModeArg::Default => p2p::iroh::IrohRelayModeConfig::Default,
            P2pRelayModeArg::Disabled => p2p::iroh::IrohRelayModeConfig::Disabled,
        },
        discovery: match args.p2p_discovery {
            P2pDiscoveryArg::N0 => p2p::iroh::IrohDiscoveryConfig::N0,
            P2pDiscoveryArg::Disabled => p2p::iroh::IrohDiscoveryConfig::Disabled,
        },
        secret_key_path: Some(secret_key_path),
        load_persisted_collections: true,
        max_concurrent_dag_fetches: crate::DEFAULT_P2P_MAX_CONCURRENT_DAG_FETCHES,
        max_concurrent_push_tasks: crate::DEFAULT_P2P_MAX_CONCURRENT_PUSH_TASKS,
        rate_limit_burst: crate::DEFAULT_P2P_RATE_LIMIT_BURST,
        rate_limit_rate: crate::DEFAULT_P2P_RATE_LIMIT_RATE,
    }))
}

async fn load_local_server_p2p_status(
    node: &EmbeddedNode,
    transport: P2pTransportArg,
) -> Result<Value> {
    match transport {
        P2pTransportArg::None => Ok(json!({
            "enabled": false,
            "p2p_transport": transport.as_str(),
            "p2p_peer_id": Value::Null,
            "p2p_listen_addresses": [],
            "p2p_connected_peers": [],
        })),
        P2pTransportArg::Iroh => {
            let p2p = node.p2p().ok_or_else(|| {
                anyhow::anyhow!(
                    "P2P transport was requested but is not available on the embedded node"
                )
            })?;
            let peer_id = p2p
                .local_peer_id()
                .await
                .context("loading local P2P peer id from the embedded node")?;
            let listen_addresses = wait_for_p2p_listen_addresses(p2p).await?;
            let connected_peers = p2p
                .connected_peers()
                .await
                .context("loading connected P2P peers from the embedded node")?;
            Ok(json!({
                "enabled": true,
                "p2p_transport": transport.as_str(),
                "p2p_peer_id": peer_id,
                "p2p_listen_addresses": listen_addresses,
                "p2p_connected_peers": connected_peers,
            }))
        }
    }
}

async fn wait_for_p2p_listen_addresses(
    p2p: &dyn defra_p2p_adapter::P2POperations,
) -> Result<Vec<String>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listen_addresses = p2p
            .listen_addresses()
            .await
            .context("loading local P2P listen addresses from the embedded node")?;
        if !listen_addresses.is_empty() {
            return Ok(listen_addresses);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(listen_addresses);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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

/// Shim host for URLs handed to `defra-agent codex --remote`: IPv6 addresses
/// need bracketing to form a valid authority (`ws://[::1]:9292/`).
fn display_shim_host(host: IpAddr) -> String {
    let host_text = display_host(host);
    if host.is_ipv6() {
        format!("[{host_text}]")
    } else {
        host_text
    }
}

#[cfg(test)]
mod shim_host_tests {
    use super::*;

    #[test]
    fn ipv6_shim_hosts_are_bracketed() {
        assert_eq!(display_shim_host("::1".parse().unwrap()), "[::1]");
        assert_eq!(display_shim_host("127.0.0.1".parse().unwrap()), "127.0.0.1");
    }
}
