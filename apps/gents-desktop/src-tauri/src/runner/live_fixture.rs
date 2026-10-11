#[path = "live_fixture/agent.rs"]
mod agent;
#[path = "live_fixture/backend.rs"]
mod backend;
#[path = "live_fixture/replication.rs"]
mod replication;
#[path = "live_fixture/workspace.rs"]
mod workspace;

use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths, PeerRecord};
use gents_desktop_core::local_runtime::DesktopInitSummary;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tracing_subscriber::prelude::*;

use gents_desktop_bridge::types::{DesktopBootstrapSummary, SavedPeerView};

use self::agent::{spawn_live_node, RunningNode};
use self::backend::LiveBackendConfig;
pub(crate) use self::backend::LiveBackendOverride;
pub(crate) use self::backend::LiveTargetBackendOverride;
use self::replication::{
    configure_live_replicators, wait_for_connectable_iroh_addr, wait_for_connected_peer,
    wait_for_live_documents, wait_for_ready_peer_status, write_peer_directory_records,
};
use self::workspace::seed_runner_node_home;

const DEFAULT_DEPLOYMENT_LABEL: &str = "Fleet E2E Agent";
const DEFAULT_NODE_NAME: &str = "fleet-e2e-agent";

pub(crate) struct LiveBridgeFixture {
    runtime: Arc<Runtime>,
    tempdir: Mutex<Option<tempfile::TempDir>>,
    desktop_paths: DesktopPaths,
    node_home: PathBuf,
    desktop_core: Arc<ClientCore>,
    remote_core: Arc<ClientCore>,
    deployment_label: String,
    node_did: String,
    tool_root: PathBuf,
    init_summary: DesktopInitSummary,
    bootstrap_saved_peers: Vec<SavedPeerView>,
    update_version: Arc<AtomicU64>,
    update_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    running_node: Mutex<Option<RunningNode>>,
    shutdown_started: AtomicBool,
}

impl LiveBridgeFixture {
    pub(crate) fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    pub(crate) fn init_summary(&self) -> DesktopInitSummary {
        self.init_summary.clone()
    }

    pub(crate) fn desktop_core(&self) -> &Arc<ClientCore> {
        &self.desktop_core
    }

    pub(crate) fn remote_core(&self) -> &Arc<ClientCore> {
        &self.remote_core
    }

    pub(crate) fn node_did(&self) -> &str {
        &self.node_did
    }

    pub(crate) fn deployment_label(&self) -> &str {
        &self.deployment_label
    }

    pub(crate) fn tool_root(&self) -> &Path {
        &self.tool_root
    }

    pub(crate) fn requester_scope(
        &self,
        node_did: Option<&str>,
        session_id: &str,
        request_id: Option<&str>,
    ) -> Option<String> {
        let node_did = node_did?;
        let store = self.desktop_core.store().snapshot();
        request_id
            .and_then(|request_id| {
                store.requests.iter().find(|request| {
                    request.request_id == request_id
                        && request.node_did.as_deref() == Some(node_did)
                        && request.session_id.as_deref() == Some(session_id)
                })
            })
            .and_then(|request| request.requester_did.clone())
            .or_else(|| {
                store
                    .sessions
                    .iter()
                    .find(|session| {
                        session.session_id == session_id && session.node_did == node_did
                    })
                    .and_then(|session| session.requester_did.clone())
            })
    }

    pub(crate) fn data_root(&self) -> &Path {
        self.node_home
            .parent()
            .expect("live fixture node home has a parent")
    }

    pub(crate) fn update_version(&self) -> u64 {
        self.update_version.load(Ordering::SeqCst)
    }

    pub(crate) async fn shutdown(&self) -> Result<()> {
        if self.shutdown_started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        if let Some(task) = self.update_task.lock().await.take() {
            task.abort();
            let _ = task.await;
        }

        if let Some(agent) = self.running_node.lock().await.take() {
            agent.shutdown().await?;
        }

        self.remote_core.shutdown().await?;
        self.desktop_core.shutdown().await?;
        if std::env::var_os("GENTS_TAURI_LIVE_RETAIN_DATA").is_some() {
            if let Some(tempdir) = self.tempdir.lock().await.take() {
                let retained = tempdir.keep();
                tracing::info!(path = %retained.display(), "retained live bridge fixture data");
            }
        }
        Ok(())
    }

    pub(crate) fn start(
        backend_override: Option<LiveBackendOverride>,
        agent_target_backend_override: Option<LiveTargetBackendOverride>,
    ) -> Result<Arc<Self>> {
        init_live_runner_tracing();

        let backend = LiveBackendConfig::resolve(backend_override.as_ref())?;
        let agent_target_backend =
            LiveBackendConfig::resolve_target(agent_target_backend_override.as_ref(), &backend)?;
        let runtime = live_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let node_home = tempdir.path().join("agent-home");

        let port_probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let operator_addr = port_probe.local_addr()?;
        drop(port_probe);
        let operator_graphql = format!("http://{operator_addr}/api/v0/graphql");
        let mut remote_options = live_core_options();
        remote_options.http_addr = Some(operator_addr);
        let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            remote_paths,
            remote_options,
        ))?);

        let agent_key = tempdir.path().join("agent").join("fleet-e2e-agent.key");
        let (running_node, docs, tool_root) = runtime.block_on(spawn_live_node(
            Arc::clone(&remote_core),
            agent_key,
            DEFAULT_NODE_NAME,
            &backend,
            agent_target_backend.as_ref(),
        ))?;
        runtime.block_on(wait_for_operator_graphql(
            &operator_graphql,
            &running_node.node_did,
        ))?;

        let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
            remote_core.as_ref(),
            DEFAULT_DEPLOYMENT_LABEL,
        ))?;
        let mut peer_record = PeerRecord::local_standard(
            DEFAULT_DEPLOYMENT_LABEL,
            &remote_addr,
            &running_node.node_did,
            &operator_graphql,
        );
        // This fixture owns both nodes and installs both directional
        // replicators below. Keep the durable route truthful while the normal
        // supervisor is intentionally disabled for this manually managed
        // topology.
        peer_record.pairing_ready = true;
        write_peer_directory_records(&desktop_paths, &[peer_record.clone()])?;

        let desktop_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            desktop_paths.clone(),
            live_core_options(),
        ))?);

        runtime.block_on(configure_live_replicators(
            desktop_core.as_ref(),
            remote_core.as_ref(),
            DEFAULT_DEPLOYMENT_LABEL,
        ))?;
        runtime.block_on(wait_for_connected_peer(
            desktop_core.as_ref(),
            remote_core.local_peer_id(),
            "desktop -> amy",
        ))?;
        runtime.block_on(wait_for_connected_peer(
            remote_core.as_ref(),
            desktop_core.local_peer_id(),
            "amy -> desktop",
        ))?;
        runtime.block_on(desktop_core.add_local_standard_peer_route_for_test(
            &peer_record.label,
            &peer_record.addr,
            &peer_record.node_did,
            peer_record.graphql.as_deref().unwrap_or_default(),
            &node_home.display().to_string(),
        ))?;
        runtime.block_on(wait_for_ready_peer_status(
            desktop_core.as_ref(),
            &peer_record.peer_id,
            "desktop -> amy",
        ))?;
        runtime.block_on(wait_for_live_documents(
            desktop_core.as_ref(),
            &running_node.node_did,
            &docs,
        ))?;

        seed_runner_node_home(
            &node_home,
            DEFAULT_NODE_NAME,
            &running_node.node_did,
            remote_core.local_peer_id(),
            &remote_addr,
        )?;

        let remote_peer_id = remote_core.local_peer_id().to_string();
        let init_summary = DesktopInitSummary {
            status: "initialized",
            source: "bridge-runner",
            node_home: node_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            peer_directory: desktop_paths.peer_directory_path().display().to_string(),
            label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            node_name: DEFAULT_NODE_NAME.to_string(),
            node_did: running_node.node_did.clone(),
            graphql: String::new(),
            p2p_transport: "iroh".to_string(),
            p2p_peer_id: remote_peer_id.clone(),
            p2p_listen_address: remote_addr.clone(),
            peer_record_id: peer_record.peer_id.clone(),
            next_steps: vec![],
        };

        let bootstrap_saved_peers = vec![SavedPeerView {
            peer_id: peer_record.peer_id.clone(),
            label: peer_record.label.clone(),
            node_did: peer_record.node_did.clone(),
            addr: peer_record.addr.clone(),
            source: peer_record.source.clone(),
            graphql: peer_record.graphql.clone(),
        }];

        tracing::info!(
            node_did = %running_node.node_did,
            tool_root = %tool_root.display(),
            "live bridge fixture ready"
        );

        let update_version = Arc::new(AtomicU64::new(1));
        let update_task = {
            let desktop_core = Arc::clone(&desktop_core);
            let remote_core = Arc::clone(&remote_core);
            let update_version = Arc::clone(&update_version);
            runtime.spawn(async move {
                let mut store_updates = desktop_core.store_updates();
                let mut sync_updates = desktop_core.sync_state_updates();
                let mut remote_store_updates = remote_core.store_updates();
                loop {
                    tokio::select! {
                        changed = store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = sync_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = remote_store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            })
        };

        Ok(Arc::new(Self {
            runtime,
            tempdir: Mutex::new(Some(tempdir)),
            desktop_paths,
            node_home,
            desktop_core,
            remote_core,
            deployment_label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            node_did: running_node.node_did.clone(),
            tool_root,
            init_summary,
            bootstrap_saved_peers,
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_node: Mutex::new(Some(running_node)),
            shutdown_started: AtomicBool::new(false),
        }))
    }

    pub(crate) fn start_desktop_only() -> Result<Arc<Self>> {
        init_live_runner_tracing();

        let runtime = live_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote-empty"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let node_home = tempdir.path().join("agent-home");
        std::fs::create_dir_all(&node_home)?;

        let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            remote_paths,
            live_core_options(),
        ))?);
        let desktop_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            desktop_paths.clone(),
            live_core_options(),
        ))?);
        let p2p_listen_address = desktop_core
            .listen_addresses()
            .first()
            .cloned()
            .unwrap_or_default();

        let init_summary = DesktopInitSummary {
            status: "initialized",
            source: "bridge-runner-desktop-only",
            node_home: node_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            peer_directory: desktop_paths.peer_directory_path().display().to_string(),
            label: "Desktop Only".to_string(),
            node_name: String::new(),
            node_did: String::new(),
            graphql: String::new(),
            p2p_transport: "iroh".to_string(),
            p2p_peer_id: desktop_core.local_peer_id().to_string(),
            p2p_listen_address,
            peer_record_id: String::new(),
            next_steps: vec![],
        };

        tracing::info!("desktop-only bridge fixture ready");

        let update_version = Arc::new(AtomicU64::new(1));
        let update_task = {
            let desktop_core = Arc::clone(&desktop_core);
            let update_version = Arc::clone(&update_version);
            runtime.spawn(async move {
                let mut store_updates = desktop_core.store_updates();
                let mut sync_updates = desktop_core.sync_state_updates();
                loop {
                    tokio::select! {
                        changed = store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = sync_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                    }
                }
            })
        };

        Ok(Arc::new(Self {
            runtime,
            tempdir: Mutex::new(Some(tempdir)),
            desktop_paths,
            node_home,
            desktop_core,
            remote_core,
            deployment_label: "Desktop Only".to_string(),
            node_did: String::new(),
            tool_root: PathBuf::new(),
            init_summary,
            bootstrap_saved_peers: Vec::new(),
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_node: Mutex::new(None),
            shutdown_started: AtomicBool::new(false),
        }))
    }

    pub(crate) async fn build_bootstrap_summary(&self) -> DesktopBootstrapSummary {
        DesktopBootstrapSummary {
            default_node_home: self.node_home.display().to_string(),
            init_node_name: non_empty_clone(&self.init_summary.node_name),
            init_node_did: non_empty_clone(&self.init_summary.node_did),
            init_tool_ceiling: Some("Readwrite".to_string()),
            init_tool_root: self
                .tool_root
                .to_str()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            desktop_home: self.desktop_paths.root().display().to_string(),
            peer_directory_path: self
                .desktop_paths
                .peer_directory_path()
                .display()
                .to_string(),
            node_data_dir: self.desktop_paths.node_data_dir().display().to_string(),
            diagnostics_hint: gents::native_logging::diagnostics_hint().to_string(),
            node_home_exists: self.node_home.exists(),
            desktop_home_exists: self.desktop_paths.root().exists(),
            peer_directory_exists: self.desktop_paths.peer_directory_path().exists(),
            client_state_exists: self.desktop_paths.client_state_exists(),
            saved_peers: self.bootstrap_saved_peers.clone(),
        }
    }
}

fn non_empty_clone(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

fn live_runtime() -> Result<Arc<Runtime>> {
    const STACK_BYTES: usize = 16 * 1024 * 1024;
    Ok(Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .thread_stack_size(STACK_BYTES)
            .build()?,
    ))
}

async fn wait_for_operator_graphql(endpoint: &str, node_did: &str) -> Result<()> {
    let access = ConfigAccess::graphql(endpoint);
    let escaped_did = escape_graphql_string(node_did);
    let query =
        format!(r#"{{ Node(filter: {{ node_did: {{ _eq: "{escaped_did}" }} }}) {{ node_did }} }}"#);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        match access.execute(&query).await {
            Ok(response)
                if response
                    .pointer("/data/Node")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|rows| rows.iter().any(|row| row["node_did"] == node_did)) =>
            {
                return Ok(());
            }
            Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Ok(response) => {
                anyhow::bail!(
                    "live operator GraphQL {endpoint} did not expose node {node_did}: {response}"
                );
            }
            Err(error) => {
                anyhow::bail!("live operator GraphQL {endpoint} was not ready: {error:#}");
            }
        }
    }
}

fn live_core_options() -> ClientCoreOptions {
    let mut options = ClientCoreOptions::local_only();
    options.bind_addr = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));
    options.max_concurrent_push_tasks = 32;
    options.rate_limit_burst = 5_000;
    options.rate_limit_rate = 500.0;
    options.install_replicators_on_bootstrap = false;
    options
}

fn init_live_runner_tracing() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let filter = std::env::var("GENTS_DESKTOP_TEST_LOG")
            .map(tracing_subscriber::EnvFilter::new)
            .unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,gents_desktop_tauri=info")
            });
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_target(false)
                    .compact()
                    .without_time(),
            )
            .try_init();
    });
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use axum::extract::State;
    use axum::http::{header, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::Router;
    use gents::default_agent_id_for_node;
    use gents_desktop_core::client::ClientCore;
    use gents_protocol::row::{decode_node_readiness_snapshot, AgentRequestRow};
    use serde_json::Value;
    use tokio::sync::oneshot;

    use super::{LiveBackendOverride, LiveBridgeFixture};
    use gents_desktop_bridge::commands::{
        delete_skill_config, save_skill_config, send_chat_message,
    };
    use gents_desktop_bridge::snapshot::build_session_snapshot_for_node_with_transcript;
    use gents_desktop_bridge::types::{ChatSendRequest, SkillDeleteRequest, SkillSaveRequest};

    const MODEL_NAME: &str = "desktop-live-skill-mock";

    #[test]
    fn live_fixture_replicates_skill_create_delete_to_node() -> Result<()> {
        let _guard = live_fixture_test_lock();
        let mock = MockChatEndpoint::start(MODEL_NAME, "ok")?;
        let fixture = LiveBridgeFixture::start(Some(mock.backend_override(MODEL_NAME)), None)?;

        let result = fixture
            .runtime()
            .block_on(skill_create_delete_case(fixture.as_ref()));
        let shutdown_result = fixture.runtime().block_on(fixture.shutdown());
        shutdown_result?;
        result
    }

    #[test]
    fn live_fixture_desktop_chat_slash_skill_loads_on_node() -> Result<()> {
        let _guard = live_fixture_test_lock();
        let mock = MockChatEndpoint::start_with_held_prompt(MODEL_NAME, "skill loaded")?;
        let fixture = LiveBridgeFixture::start(Some(mock.backend_override(MODEL_NAME)), None)?;

        let result = fixture
            .runtime()
            .block_on(slash_skill_chat_case(fixture.as_ref(), &mock));
        let shutdown_result = fixture.runtime().block_on(fixture.shutdown());
        shutdown_result?;
        result
    }

    async fn skill_create_delete_case(fixture: &LiveBridgeFixture) -> Result<()> {
        let node_did = fixture.node_did().to_string();
        let skill_id = "desktop-crud-skill";
        let skill_body = "CRUD skill body should replicate to the node.";

        save_skill_config(
            fixture.desktop_core().as_ref(),
            skill_save_request(&node_did, skill_id, skill_body),
        )
        .await?;
        wait_for_remote_skill(fixture.remote_core().as_ref(), skill_id)
            .await
            .context("skill did not replicate to the node after create")?;

        delete_skill_config(
            fixture.desktop_core().as_ref(),
            SkillDeleteRequest {
                skill_id: skill_id.to_string(),
                node_did: node_did.clone(),
            },
        )
        .await?;
        wait_for_remote_skill_absent(fixture.remote_core().as_ref(), skill_id)
            .await
            .context("skill remained queryable on the node after delete")?;
        Ok(())
    }

    async fn slash_skill_chat_case(
        fixture: &LiveBridgeFixture,
        mock: &MockChatEndpoint,
    ) -> Result<()> {
        let node_did = fixture.node_did().to_string();
        let agent_id = default_agent_id_for_node(&node_did);
        let skill_id = "desktop-review";
        let skill_body = "UNIQUE_DESKTOP_SKILL_BODY_USE_THIS_REVIEW_PROTOCOL";
        let task = "summarize the current workspace state";

        save_skill_config(
            fixture.desktop_core().as_ref(),
            skill_save_request(&node_did, skill_id, skill_body),
        )
        .await?;
        wait_for_remote_skill(fixture.remote_core().as_ref(), skill_id).await?;
        let generation_before_bind =
            wait_for_remote_runtime_generation(fixture.remote_core().as_ref(), &node_did)
                .await
                .context("runtime status missing before skill binding")?;
        bind_skill_to_agent_context(fixture, &node_did, &agent_id, skill_id).await?;
        wait_for_remote_context_skill_ids(fixture.remote_core().as_ref(), &agent_id, &[skill_id])
            .await?;
        wait_for_remote_runtime_generation_after(
            fixture.remote_core().as_ref(),
            &node_did,
            generation_before_bind,
        )
        .await
        .context("node runtime did not reconcile the skill binding before chat submit")?;

        let release = HeldResponseRelease(mock.held_response.clone());
        let initial = send_chat_message(
            fixture.desktop_core().as_ref(),
            ChatSendRequest {
                node_did: node_did.clone(),
                agent_id: Some(agent_id.clone()),
                session_id: None,
                content: HELD_PROMPT.to_string(),
                caused_by_source_doc_id: None,
                answer: None,
                cwd: None,
            },
        )
        .await?;
        wait_for_captured_chat_request(mock, HELD_PROMPT).await?;
        wait_for_condition(
            "active initial desktop request",
            Duration::from_secs(60),
            || async {
                fixture.desktop_core().refresh_store().await?;
                let store = fixture.desktop_core().store().snapshot();
                Ok(store.requests.iter().any(|row| {
                    row.request_id == initial.request_id
                        && row.node_did.as_deref() == Some(node_did.as_str())
                        && row.session_id.as_deref() == Some(initial.session_id.as_str())
                        && matches!(row.lifecycle_state,
                            Some(gents_protocol::request_lifecycle::RequestLifecycleState::Claimed
                                | gents_protocol::request_lifecycle::RequestLifecycleState::Processing))
                }) && store
                    .derive_turn_for_node(&initial.session_id, &node_did)
                    .is_some_and(|turn| !turn.is_terminal())
                    && store
                        .latest_request_id_for_session_for_node(&initial.session_id, &node_did)
                        .as_deref()
                        == Some(initial.request_id.as_str()))
            },
        )
        .await?;

        let submitted = send_chat_message(
            fixture.desktop_core().as_ref(),
            ChatSendRequest {
                node_did: node_did.clone(),
                agent_id: Some(agent_id.clone()),
                session_id: Some(initial.session_id.clone()),
                content: format!("/{skill_id}\n{task}"),
                caused_by_source_doc_id: None,
                answer: None,
                cwd: None,
            },
        )
        .await?;

        let requester_scope = wait_for_row(
            "desktop submitted request scope",
            Duration::from_secs(60),
            || async {
                fixture.desktop_core().refresh_store().await?;
                Ok(fixture
                    .desktop_core()
                    .store()
                    .snapshot()
                    .requests
                    .iter()
                    .find(|row| {
                        row.request_id == submitted.request_id
                            && row.node_did.as_deref() == Some(node_did.as_str())
                            && row.session_id.as_deref() == Some(submitted.session_id.as_str())
                    })
                    .and_then(|row| row.requester_did.clone()))
            },
        )
        .await?;
        let transcript_page = gents_desktop_core::client::load_session_transcript_page(
            fixture.desktop_core().node(),
            &submitted.session_id,
            Some(&node_did),
            Some(&requester_scope),
            None,
            None,
        )
        .await?;
        let context_store = gents_desktop_core::client::load_session_context_store(
            fixture.desktop_core().node(),
            &submitted.session_id,
            Some(&node_did),
            Some(&requester_scope),
        )
        .await?;
        let session = build_session_snapshot_for_node_with_transcript(
            fixture.desktop_core().as_ref(),
            Some(&node_did),
            &submitted.session_id,
            Some(&submitted.request_id),
            Some(&transcript_page.store),
            Some(&transcript_page.canonical_dependencies),
            Some(&context_store),
            None,
            true,
            true,
        )
        .await
        .context("desktop session snapshot missing after skill chat submit")?;
        assert_eq!(
            session.latest_request_id.as_deref(),
            Some(initial.request_id.as_str()),
            "initial={} submitted={} source_rows={:?} gate_permits={:?}",
            initial.request_id,
            submitted.request_id,
            fixture
                .desktop_core()
                .store()
                .snapshot()
                .requests
                .iter()
                .filter(|row| row.request_id == initial.request_id
                    || row.request_id == submitted.request_id)
                .collect::<Vec<_>>(),
            mock.held_response
                .as_ref()
                .map(|gate| gate.available_permits()),
        );
        let queued_turn = session
            .queued_turns
            .iter()
            .find(|turn| turn.request_id == submitted.request_id)
            .context("desktop session snapshot did not expose the queued turn")?;
        assert_eq!(queued_turn.content, task);
        assert_eq!(queued_turn.selected_skill_ids, vec![skill_id.to_string()]);
        drop(release);

        let request =
            wait_for_remote_request(fixture.remote_core().as_ref(), &submitted.request_id).await?;
        assert_eq!(
            request.requester_did.as_deref(),
            Some(requester_scope.as_str())
        );
        assert_eq!(request.content.as_deref(), Some(task));
        let input = request
            .input
            .as_ref()
            .context("replicated request is missing canonical input")?;
        assert_eq!(input.selected_skill_ids, vec![skill_id.to_string()]);
        assert_eq!(
            input
                .queue
                .as_ref()
                .and_then(|queue| queue.queued_after_request_id.as_deref()),
            Some(initial.request_id.as_str())
        );

        let captured = wait_for_row(
            "queued skill provider request",
            Duration::from_secs(120),
            || async {
                Ok(mock.captured_chat_requests().into_iter().find(|request| {
                    request.to_string().contains(skill_body)
                        && request["messages"].as_array().is_some_and(|messages| {
                            messages.iter().any(|message| {
                                message["role"] == "user"
                                    && message["content"].to_string().contains(task)
                            })
                        })
                }))
            },
        )
        .await?;
        assert!(
            captured["messages"]
                .as_array()
                .is_some_and(
                    |messages| messages.iter().any(|message| message["role"] == "user"
                        && message["content"].to_string().contains(task))
                ),
            "mock model request did not include the queued task: {captured}"
        );
        assert!(
            captured.to_string().contains(skill_body),
            "mock model request did not include selected skill body: {captured}"
        );

        Ok(())
    }

    fn live_fixture_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("live fixture test lock poisoned")
    }

    async fn bind_skill_to_agent_context(
        fixture: &LiveBridgeFixture,
        node_did: &str,
        agent_id: &str,
        skill_id: &str,
    ) -> Result<()> {
        use gents::collection::Collection;
        use gents::config_client::{
            apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
            DesiredStateApplyPlan,
        };
        fixture
            .desktop_core()
            .operator_access(node_did)?
            .transact("desktop.fixture.skill", |txn| {
                Box::pin(async move {
                    let (_, agent) = read_desired_state_record_in_txn(
                        txn,
                        Collection::Agent,
                        node_did,
                        agent_id,
                    )
                    .await?
                    .with_context(|| format!("agent {agent_id} is missing"))?;
                    let agent: gents::AgentDocument = serde_json::from_value(agent)?;
                    let context_id = agent.context_id.context("fixture agent has no context")?;
                    let (_, context) = read_desired_state_record_in_txn(
                        txn,
                        Collection::AgentContext,
                        node_did,
                        &context_id,
                    )
                    .await?
                    .context("fixture context is missing")?;
                    let mut context: gents::document_config::AgentContext =
                        serde_json::from_value(context)?;
                    context.skill_ids = vec![skill_id.to_owned()];
                    let value = serde_json::to_value(context)?;
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: Collection::AgentContext,
                        add: value.clone(),
                        update: value,
                    }])?;
                    apply_desired_state_plan(txn, &plan).await?;
                    Ok(())
                })
            })
            .await?;
        fixture.desktop_core().refresh_store().await?;
        Ok(())
    }

    async fn wait_for_remote_context_skill_ids(
        core: &ClientCore,
        agent_id: &str,
        expected_skill_ids: &[&str],
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            core.refresh_store().await?;
            let store = core.store().snapshot();
            let observed = store
                .agents
                .iter()
                .find(|agent| agent.agent_id == agent_id)
                .and_then(|agent| agent.context_id.as_deref())
                .and_then(|context_id| {
                    store
                        .contexts
                        .iter()
                        .find(|context| context.context_id == context_id)
                })
                .map(|context| context.skill_ids.as_slice());
            let matches = observed.is_some_and(|skill_ids| {
                skill_ids
                    .iter()
                    .map(String::as_str)
                    .eq(expected_skill_ids.iter().copied())
            });
            if matches {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "remote context for agent {agent_id} has skill IDs {observed:?}, expected {expected_skill_ids:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_remote_skill(core: &ClientCore, skill_id: &str) -> Result<()> {
        wait_for_condition("remote Skill create", Duration::from_secs(60), || async {
            core.refresh_store().await?;
            Ok(core
                .store()
                .snapshot()
                .skills
                .iter()
                .any(|skill| skill.skill_id == skill_id))
        })
        .await
    }

    async fn wait_for_remote_skill_absent(core: &ClientCore, skill_id: &str) -> Result<()> {
        wait_for_condition("remote Skill delete", Duration::from_secs(60), || async {
            core.refresh_store().await?;
            Ok(!core
                .store()
                .snapshot()
                .skills
                .iter()
                .any(|skill| skill.skill_id == skill_id))
        })
        .await
    }

    async fn wait_for_remote_request(
        core: &ClientCore,
        request_id: &str,
    ) -> Result<AgentRequestRow> {
        wait_for_row("remote AgentRequest", Duration::from_secs(60), || async {
            core.refresh_store().await?;
            Ok(core
                .store()
                .snapshot()
                .requests
                .iter()
                .find(|request| request.request_id == request_id)
                .cloned())
        })
        .await
    }

    struct RemoteRuntimeObservation {
        active_generation: u64,
        reconcile_phase: Option<String>,
        last_reconcile_result: Option<String>,
        last_reconcile_error: Option<String>,
    }

    async fn query_runtime_observation(
        core: &ClientCore,
        node_did: &str,
    ) -> Result<Option<RemoteRuntimeObservation>> {
        core.refresh_store().await?;
        let store = core.store().snapshot();
        let Some(readiness_row) = store.node_readiness(node_did) else {
            return Ok(None);
        };
        let readiness = decode_node_readiness_snapshot(readiness_row, node_did)
            .map_err(|reason| anyhow::anyhow!("invalid node readiness: {reason:?}"))?;
        let Some(runtime) = store.latest_runtime(node_did) else {
            return Ok(None);
        };
        Ok(Some(RemoteRuntimeObservation {
            active_generation: readiness.active_generation,
            reconcile_phase: runtime.reconcile_phase.clone(),
            last_reconcile_result: runtime.last_reconcile_result.clone(),
            last_reconcile_error: runtime.last_reconcile_error.clone(),
        }))
    }

    async fn wait_for_remote_runtime_generation(core: &ClientCore, node_did: &str) -> Result<u64> {
        wait_for_row(
            "remote authoritative runtime generation",
            Duration::from_secs(60),
            || async { query_runtime_observation(core, node_did).await },
        )
        .await
        .map(|observation| observation.active_generation)
    }

    async fn wait_for_remote_runtime_generation_after(
        core: &ClientCore,
        node_did: &str,
        previous_generation: u64,
    ) -> Result<()> {
        wait_for_condition(
            "remote authoritative runtime generation advance",
            Duration::from_secs(90),
            || async {
                let Some(observation) = query_runtime_observation(core, node_did).await? else {
                    return Ok(false);
                };
                if observation.last_reconcile_result.as_deref() == Some("error") {
                    bail!(
                        "runtime reconcile failed while waiting for skill binding: {}",
                        observation
                            .last_reconcile_error
                            .as_deref()
                            .unwrap_or("unknown error")
                    );
                }
                Ok(observation.active_generation > previous_generation
                    && observation.reconcile_phase.as_deref() == Some("idle"))
            },
        )
        .await
    }

    fn has_exact_user_text(request: &Value, text: &str) -> bool {
        request["messages"].as_array().is_some_and(|messages| {
            messages.iter().any(|message| {
                message["role"] == "user"
                    && (message["content"].as_str() == Some(text)
                        || message["content"].as_array().is_some_and(|parts| {
                            parts.len() == 1 && parts[0]["text"].as_str() == Some(text)
                        }))
            })
        })
    }

    #[test]
    fn held_provider_match_excludes_title_wrappers() {
        assert!(has_exact_user_text(
            &serde_json::json!({"messages": [
                {"role": "user", "content": HELD_PROMPT}
            ]}),
            HELD_PROMPT
        ));
        assert!(has_exact_user_text(
            &serde_json::json!({"messages": [
                {"role": "user", "content": [{"type": "text", "text": HELD_PROMPT}]}
            ]}),
            HELD_PROMPT
        ));
        assert!(!has_exact_user_text(
            &serde_json::json!({"messages": [
                {"role": "user", "content": format!("First user request: {HELD_PROMPT}")}
            ]}),
            HELD_PROMPT
        ));
    }

    async fn wait_for_captured_chat_request(
        mock: &MockChatEndpoint,
        needle: &str,
    ) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let captured = mock.captured_chat_requests();
            if let Some(request) = captured
                .iter()
                .find(|request| has_exact_user_text(request, needle))
            {
                return Ok(request.clone());
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for mock chat request containing {needle:?}; captured={captured:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_for_row<T, F, Fut>(
        label: &'static str,
        timeout: Duration,
        mut check: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<Option<T>>>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(row) = check().await? {
                return Ok(row);
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for {label}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn wait_for_condition<F, Fut>(
        label: &'static str,
        timeout: Duration,
        mut check: F,
    ) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<bool>>,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if check().await? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for {label}");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn skill_save_request(node_did: &str, skill_id: &str, instructions: &str) -> SkillSaveRequest {
        SkillSaveRequest {
            document: serde_json::from_value(serde_json::json!({
                "skill_id": skill_id, "node_did": node_did, "name": skill_id,
                "description": format!("Test skill {skill_id}"),
                "instructions": instructions, "display_name": skill_id,
            }))
            .expect("canonical fixture skill"),
        }
    }

    const HELD_PROMPT: &str = "HOLD_FIRST_SLASH_SKILL_FIXTURE_TURN";

    /// Unblock the provider on early return before fixture shutdown drains requests.
    struct HeldResponseRelease(Option<Arc<tokio::sync::Semaphore>>);

    impl Drop for HeldResponseRelease {
        fn drop(&mut self) {
            if let Some(gate) = self.0.take() {
                gate.add_permits(1);
            }
        }
    }

    #[derive(Clone)]
    struct MockState {
        model_name: String,
        final_text: String,
        held_response: Option<Arc<tokio::sync::Semaphore>>,
        captured: Arc<Mutex<Vec<Value>>>,
    }

    struct MockChatEndpoint {
        endpoint: String,
        held_response: Option<Arc<tokio::sync::Semaphore>>,
        captured: Arc<Mutex<Vec<Value>>>,
        shutdown: Option<oneshot::Sender<()>>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl MockChatEndpoint {
        fn start(model_name: &str, final_text: &str) -> Result<Self> {
            Self::start_inner(model_name, final_text, None)
        }

        fn start_with_held_prompt(model_name: &str, final_text: &str) -> Result<Self> {
            Self::start_inner(
                model_name,
                final_text,
                Some(Arc::new(tokio::sync::Semaphore::new(0))),
            )
        }

        fn start_inner(
            model_name: &str,
            final_text: &str,
            held_response: Option<Arc<tokio::sync::Semaphore>>,
        ) -> Result<Self> {
            let captured = Arc::new(Mutex::new(Vec::new()));
            let state = Arc::new(MockState {
                model_name: model_name.to_string(),
                final_text: final_text.to_string(),
                held_response: held_response.clone(),
                captured: Arc::clone(&captured),
            });
            let (port_tx, port_rx) = std::sync::mpsc::channel::<u16>();
            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let join = std::thread::spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    let Ok(listener) = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await else {
                        return;
                    };
                    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
                    let _ = port_tx.send(port);
                    let app = Router::new()
                        .route("/v1/models", get(handle_models))
                        .route("/models", get(handle_models))
                        .route("/v1/chat/completions", post(handle_chat))
                        .route("/chat/completions", post(handle_chat))
                        .fallback(handle_fallback)
                        .with_state(state);
                    let _ = axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = shutdown_rx.await;
                        })
                        .await;
                });
            });

            let port = port_rx
                .recv()
                .context("mock chat endpoint failed to bind a port")?;
            Ok(Self {
                endpoint: format!("http://127.0.0.1:{port}/v1"),
                held_response,
                captured,
                shutdown: Some(shutdown_tx),
                join: Some(join),
            })
        }

        fn backend_override(&self, model_name: &str) -> LiveBackendOverride {
            LiveBackendOverride {
                inference_url: Some(self.endpoint.clone()),
                model_name: Some(model_name.to_string()),
                provider: Some(
                    gents::BackendProviderKind::OpenAiCompatible
                        .as_str()
                        .to_owned(),
                ),
                api_key: Some("desktop-live-test-key".to_string()),
                api_key_env_var: None,
            }
        }

        fn captured_chat_requests(&self) -> Vec<Value> {
            self.captured
                .lock()
                .expect("captured mock request mutex poisoned")
                .clone()
        }
    }

    impl Drop for MockChatEndpoint {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    async fn handle_models(State(state): State<Arc<MockState>>) -> Response {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::json!({ "data": [{ "id": state.model_name }] }).to_string(),
        )
            .into_response()
    }

    async fn handle_chat(State(state): State<Arc<MockState>>, body: String) -> Response {
        let request_json = match serde_json::from_str::<Value>(&body) {
            Ok(value) => value,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    [(header::CONTENT_TYPE, "application/json")],
                    r#"{"error":"invalid json"}"#,
                )
                    .into_response()
            }
        };
        let held = has_exact_user_text(&request_json, HELD_PROMPT);
        state
            .captured
            .lock()
            .expect("captured mock request mutex poisoned")
            .push(request_json);
        if held {
            if let Some(gate) = &state.held_response {
                let _permit = gate
                    .acquire()
                    .await
                    .expect("fixture response gate remains open");
            }
        }
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            completion_text_sse(&state.final_text),
        )
            .into_response()
    }

    async fn handle_fallback() -> Response {
        (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":"not found"}"#,
        )
            .into_response()
    }

    fn completion_text_sse(text: &str) -> String {
        let chunk_1 = serde_json::json!({
            "choices": [{ "delta": { "content": text }, "finish_reason": null }],
            "usage": null
        });
        let chunk_2 = serde_json::json!({
            "choices": [{
                "delta": { "content": null, "tool_calls": [] },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 24, "completion_tokens": 6, "total_tokens": 30 }
        });
        format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            serde_json::to_string(&chunk_1).expect("serialize completion chunk 1"),
            serde_json::to_string(&chunk_2).expect("serialize completion chunk 2"),
        )
    }
}
