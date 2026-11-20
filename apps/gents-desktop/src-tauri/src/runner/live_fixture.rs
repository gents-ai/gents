#[path = "live_fixture/agent.rs"]
mod agent;
#[path = "live_fixture/backend.rs"]
mod backend;
#[path = "live_fixture/connection.rs"]
mod connection;
#[path = "live_fixture/workspace.rs"]
mod workspace;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use gents::client_runtime::DesktopInitSummary;
use gents::runtime::embedded::EmbeddedHome;
use gents_desktop_bridge::client_runtime::{ClientRuntime, ClientRuntimeOptions};
use gents_protocol::runtime_connection::RuntimeConnection;
use gents_store::client_journal::paths::DesktopPaths;
use tokio::sync::Mutex;

use gents_desktop_bridge::types::{DesktopBootstrapSummary, InitConfigStatus, SavedRuntimeView};

use self::agent::{spawn_live_node, RunningNode};
use self::backend::LiveBackendConfig;
pub(crate) use self::backend::LiveBackendOverride;
pub(crate) use self::backend::LiveTargetBackendOverride;
use self::connection::{wait_for_live_documents, wait_for_runtime_iroh_addr};
use self::workspace::seed_runner_node_home;

const DEFAULT_DEPLOYMENT_LABEL: &str = "Fleet E2E Agent";
const DEFAULT_NODE_NAME: &str = "fleet-e2e-agent";

pub(crate) struct LiveBridgeFixture {
    tempdir: Mutex<Option<tempfile::TempDir>>,
    desktop_paths: DesktopPaths,
    node_home: PathBuf,
    desktop_core: Arc<ClientRuntime>,
    remote_core: Arc<ClientRuntime>,
    runtime_home: Option<EmbeddedHome>,
    deployment_label: String,
    node_did: String,
    tool_root: PathBuf,
    init_summary: DesktopInitSummary,
    bootstrap_saved_runtimes: Vec<SavedRuntimeView>,
    update_version: Arc<AtomicU64>,
    update_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    running_node: Mutex<Option<RunningNode>>,
    shutdown_started: AtomicBool,
    retain_data: bool,
    /// The node the runner's one frontend selected, as a window's selection.
    selected_node: std::sync::Mutex<Option<String>>,
}

/// Reads every node that has a connection, each through its own connection
/// into its own store.
pub(crate) async fn refresh_every_node(core: &ClientRuntime) -> Result<()> {
    for record in core.runtime_connections() {
        if core
            .operator_observation_graphql(&record.node_did)
            .is_some()
        {
            core.refresh_node(&record.node_did).await?;
        }
    }
    Ok(())
}

/// The first value `find` reports from any node's own store.
#[cfg(test)]
fn find_in_node_stores<T>(
    core: &ClientRuntime,
    mut find: impl FnMut(&gents_observe::client_projection::store::ClientStore) -> Option<T>,
) -> Option<T> {
    core.stores()
        .node_dids()
        .into_iter()
        .find_map(|node_did| find(&core.stores().snapshot(&node_did)))
}

impl LiveBridgeFixture {
    pub(crate) fn selected_node_did(&self) -> Option<String> {
        self.selected_node
            .lock()
            .expect("selected node lock poisoned")
            .clone()
    }

    pub(crate) fn select_node(&self, node_did: Option<String>) {
        *self
            .selected_node
            .lock()
            .expect("selected node lock poisoned") = node_did;
    }

    pub(crate) fn init_summary(&self) -> DesktopInitSummary {
        self.init_summary.clone()
    }

    pub(crate) fn desktop_core(&self) -> &Arc<ClientRuntime> {
        &self.desktop_core
    }

    pub(crate) fn remote_core(&self) -> &Arc<ClientRuntime> {
        &self.remote_core
    }

    pub(crate) fn runtime_node(&self) -> Result<&Arc<gents::defra_node::Node>> {
        Ok(&self
            .runtime_home
            .as_ref()
            .context("live runtime unavailable")?
            .node)
    }

    pub(crate) fn runtime_p2p(&self) -> Result<Arc<dyn defra_p2p_adapter::P2POperations>> {
        gents_p2p::node_access::operations(self.runtime_node()?.as_ref())
            .context("live runtime P2P unavailable")
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
        let store = self.desktop_core.stores().snapshot(node_did);
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

    pub(crate) fn node_home(&self) -> &Path {
        &self.node_home
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

        if let Some(home) = &self.runtime_home {
            home.node.shutdown().await?;
        }

        self.remote_core.shutdown().await?;
        self.desktop_core.shutdown().await?;
        if self.retain_data {
            if let Some(tempdir) = self.tempdir.lock().await.take() {
                let retained = tempdir.keep();
                tracing::info!(path = %retained.display(), "retained live bridge fixture data");
            }
        }
        Ok(())
    }

    pub(crate) async fn start(
        backend_override: Option<LiveBackendOverride>,
        agent_target_backend_override: Option<LiveTargetBackendOverride>,
        retain_data: bool,
    ) -> Result<Arc<Self>> {
        let backend = LiveBackendConfig::resolve(backend_override.as_ref())?;
        let agent_target_backend =
            LiveBackendConfig::resolve_target(agent_target_backend_override.as_ref(), &backend)?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        desktop_paths.ensure_root_dirs().await?;
        let config_actor =
            gents_store::client_journal::principal_identity::PrincipalIdentity::load_or_create(
                &desktop_paths,
            )
            .await?;
        let node_home = tempdir.path().join("agent-home");

        let p2p = Arc::new(move |path: &Path| gents::defra_node::P2PConfig {
            port: Some(0),
            bind_addr: Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            secret_key_path: Some(path.join("node.iroh.key")),
            ..Default::default()
        });
        let runtime_home = EmbeddedHome::create_retained_with_p2p(&node_home, Some(p2p)).await?;
        let operator_graphql = runtime_home
            .graphql_endpoint()
            .context("runtime endpoint missing")?
            .to_owned();
        let remote_core = Arc::new(
            ClientRuntime::start_with_paths_and_options(remote_paths, live_core_options()).await?,
        );

        let agent_root = tempdir.path().join("agent");
        let (running_node, docs, tool_root) = spawn_live_node(
            &runtime_home,
            &config_actor,
            agent_root,
            DEFAULT_NODE_NAME,
            &backend,
            agent_target_backend.as_ref(),
        )
        .await?;
        let runtime_p2p = gents_p2p::node_access::operations(&runtime_home.node)
            .context("runtime P2P unavailable")?;
        let remote_addr =
            wait_for_runtime_iroh_addr(&runtime_p2p, DEFAULT_DEPLOYMENT_LABEL).await?;
        let remote_peer_id = runtime_p2p.local_peer_id().await?;
        let connection = RuntimeConnection {
            node_did: running_node.node_did.clone(),
            label: DEFAULT_DEPLOYMENT_LABEL.to_owned(),
            graphql: operator_graphql.clone(),
            local_node_home: Some(node_home.display().to_string()),
            access_request: None,
            access_state: None,
        };

        let desktop_core = Arc::new(
            ClientRuntime::start_with_paths_and_options(desktop_paths.clone(), live_core_options())
                .await?,
        );

        desktop_core.connect_runtime(connection.clone()).await?;
        desktop_core.start_authored_delivery();
        remote_core.start_authored_delivery();
        wait_for_live_documents(desktop_core.as_ref(), &running_node.node_did, &docs).await?;

        seed_runner_node_home(
            &node_home,
            DEFAULT_NODE_NAME,
            &running_node.node_did,
            &docs.agent_did,
            &operator_graphql,
            &remote_peer_id,
            &remote_addr,
        )?;

        let init_summary = DesktopInitSummary {
            status: "initialized",
            node_home: node_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            runtime_directory: desktop_paths.runtime_directory_path().display().to_string(),
            label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            node_name: DEFAULT_NODE_NAME.to_string(),
            node_did: running_node.node_did.clone(),
            graphql: operator_graphql.clone(),
            next_steps: vec![],
        };

        let bootstrap_saved_runtimes = vec![SavedRuntimeView {
            label: connection.label,
            node_did: connection.node_did,
            graphql: Some(connection.graphql),
            local: true,
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
            tokio::spawn(async move {
                let mut store_updates = desktop_core.store_updates();
                let mut sync_updates = desktop_core.runtime_connection_updates();
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
            tempdir: Mutex::new(Some(tempdir)),
            desktop_paths,
            node_home,
            desktop_core,
            remote_core,
            runtime_home: Some(runtime_home),
            deployment_label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            node_did: running_node.node_did.clone(),
            tool_root,
            init_summary,
            bootstrap_saved_runtimes,
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_node: Mutex::new(Some(running_node)),
            shutdown_started: AtomicBool::new(false),
            retain_data,
            selected_node: std::sync::Mutex::new(None),
        }))
    }

    pub(crate) async fn start_desktop_only(retain_data: bool) -> Result<Arc<Self>> {
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote-empty"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let node_home = tempdir.path().join("agent-home");
        std::fs::create_dir_all(&node_home)?;

        let remote_core = Arc::new(
            ClientRuntime::start_with_paths_and_options(remote_paths, live_core_options()).await?,
        );
        let desktop_core = Arc::new(
            ClientRuntime::start_with_paths_and_options(desktop_paths.clone(), live_core_options())
                .await?,
        );
        desktop_core.start_authored_delivery();
        remote_core.start_authored_delivery();

        let init_summary = DesktopInitSummary {
            status: "initialized",
            node_home: node_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            runtime_directory: desktop_paths.runtime_directory_path().display().to_string(),
            label: "Desktop Only".to_string(),
            node_name: String::new(),
            node_did: String::new(),
            graphql: String::new(),
            next_steps: vec![],
        };

        tracing::info!("desktop-only bridge fixture ready");

        let update_version = Arc::new(AtomicU64::new(1));
        let update_task = {
            let desktop_core = Arc::clone(&desktop_core);
            let update_version = Arc::clone(&update_version);
            tokio::spawn(async move {
                let mut store_updates = desktop_core.store_updates();
                let mut sync_updates = desktop_core.runtime_connection_updates();
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
            tempdir: Mutex::new(Some(tempdir)),
            desktop_paths,
            node_home,
            desktop_core,
            remote_core,
            runtime_home: None,
            deployment_label: "Desktop Only".to_string(),
            node_did: String::new(),
            tool_root: PathBuf::new(),
            init_summary,
            bootstrap_saved_runtimes: Vec::new(),
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_node: Mutex::new(None),
            shutdown_started: AtomicBool::new(false),
            retain_data,
            selected_node: std::sync::Mutex::new(None),
        }))
    }

    pub(crate) async fn build_bootstrap_summary(&self) -> DesktopBootstrapSummary {
        DesktopBootstrapSummary {
            default_node_home: self.node_home.display().to_string(),
            init_config_status: Some(if self.node_did.is_empty() {
                InitConfigStatus::Absent
            } else {
                InitConfigStatus::Configured
            }),
            init_node_name: non_empty_clone(&self.init_summary.node_name),
            init_node_did: non_empty_clone(&self.init_summary.node_did),
            init_tool_ceiling: Some("Readwrite".to_string()),
            init_tool_root: self
                .tool_root
                .to_str()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            desktop_home: self.desktop_paths.root().display().to_string(),
            diagnostics_hint: gents::runtime::native_logging::diagnostics_hint().to_string(),
            node_home_exists: self.node_home.exists(),
            client_state_exists: self.desktop_paths.client_state_exists(),
            local_setup_unfinished: gents_desktop_core::home_reset::local_setup_unfinished(
                &self.node_home,
                non_empty_clone(&self.init_summary.node_did).as_deref(),
                self.bootstrap_saved_runtimes
                    .iter()
                    .map(|peer| peer.node_did.as_str()),
            ),
            saved_runtimes: self.bootstrap_saved_runtimes.clone(),
        }
    }
}

fn non_empty_clone(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_string())
}

/// Drives a live fixture to completion on the runtime it owns. Callers are
/// synchronous process or test entry points outside any runtime.
#[cfg(test)]
pub(crate) fn run_on_live_runtime<F>(future: F) -> Result<F::Output>
where
    F: std::future::Future,
{
    run_on_live_runtime_with_handle(|_| future)
}

pub(crate) fn run_on_live_runtime_with_handle<F, Fut>(start: F) -> Result<Fut::Output>
where
    F: FnOnce(tokio::runtime::Handle) -> Fut,
    Fut: std::future::Future,
{
    const STACK_BYTES: usize = 16 * 1024 * 1024;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .thread_stack_size(STACK_BYTES)
        .build()?;
    let handle = runtime.handle().clone();
    Ok(runtime.block_on(start(handle)))
}

fn live_core_options() -> ClientRuntimeOptions {
    ClientRuntimeOptions {
        store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::{find_in_node_stores, refresh_every_node};
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use axum::extract::State;
    use axum::http::{header, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use axum::Router;
    use gents_desktop_bridge::client_runtime::ClientRuntime;
    use gents_protocol::row::{decode_node_readiness_snapshot, AgentRequestRow};
    use serde_json::Value;
    use tokio::sync::oneshot;

    use super::{run_on_live_runtime, LiveBackendOverride, LiveBridgeFixture};
    use gents_desktop_bridge::commands::{
        delete_skill_config, save_skill_config, send_chat_message,
    };
    use gents_desktop_bridge::snapshot::build_session_snapshot_for_node_with_transcript;
    use gents_desktop_bridge::types::{ChatSendRequest, SkillDeleteRequest, SkillSaveRequest};

    const MODEL_NAME: &str = "desktop-live-skill-mock";

    #[test]
    fn live_fixture_writes_skill_create_delete_to_runtime() -> Result<()> {
        let _guard = live_fixture_test_lock();
        run_on_live_runtime(async {
            let mock = MockChatEndpoint::start(MODEL_NAME, "ok").await?;
            let fixture =
                LiveBridgeFixture::start(Some(mock.backend_override(MODEL_NAME)), None, false)
                    .await?;
            let result = skill_create_delete_case(fixture.as_ref()).await;
            fixture.shutdown().await?;
            result
        })?
    }

    #[test]
    fn live_fixture_desktop_chat_slash_skill_loads_on_node() -> Result<()> {
        let _guard = live_fixture_test_lock();
        run_on_live_runtime(async {
            let mock = MockChatEndpoint::start_with_held_prompt(MODEL_NAME, "skill loaded").await?;
            let fixture =
                LiveBridgeFixture::start(Some(mock.backend_override(MODEL_NAME)), None, false)
                    .await?;
            let result = slash_skill_chat_case(fixture.as_ref(), &mock).await;
            fixture.shutdown().await?;
            result
        })?
    }

    async fn skill_create_delete_case(fixture: &LiveBridgeFixture) -> Result<()> {
        let node_did = fixture.node_did().to_string();
        let skill_id = "desktop-crud-skill";
        let skill_body = "CRUD skill body should be written to the runtime.";

        save_skill_config(
            fixture.desktop_core().as_ref(),
            skill_save_request(&node_did, skill_id, skill_body),
        )
        .await?;
        wait_for_remote_skill(fixture.desktop_core().as_ref(), skill_id)
            .await
            .context("skill did not appear on the runtime after create")?;

        delete_skill_config(
            fixture.desktop_core().as_ref(),
            SkillDeleteRequest {
                skill_id: skill_id.to_string(),
                node_did: node_did.clone(),
            },
        )
        .await?;
        wait_for_remote_skill_absent(fixture.desktop_core().as_ref(), skill_id)
            .await
            .context("skill remained queryable on the node after delete")?;
        Ok(())
    }

    async fn slash_skill_chat_case(
        fixture: &LiveBridgeFixture,
        mock: &MockChatEndpoint,
    ) -> Result<()> {
        let node_did = fixture.node_did().to_string();
        let agent_did = fixture
            .desktop_core()
            .stores()
            .snapshot(&node_did)
            .default_agent_did_for_node(&node_did)
            .context("live fixture Node has no configured default agent")?
            .to_owned();
        let skill_id = "desktop-review";
        let skill_body = "UNIQUE_DESKTOP_SKILL_BODY_USE_THIS_REVIEW_PROTOCOL";
        let task = "summarize the current workspace state";

        save_skill_config(
            fixture.desktop_core().as_ref(),
            skill_save_request(&node_did, skill_id, skill_body),
        )
        .await?;
        wait_for_remote_skill(fixture.desktop_core().as_ref(), skill_id).await?;
        let generation_before_bind =
            wait_for_remote_runtime_generation(fixture.desktop_core().as_ref(), &node_did)
                .await
                .context("runtime status missing before skill binding")?;
        bind_skill_to_agent_context(fixture, &node_did, &agent_did, skill_id).await?;
        wait_for_remote_context_skill_ids(fixture.desktop_core().as_ref(), &agent_did, &[skill_id])
            .await?;
        wait_for_remote_runtime_generation_after(
            fixture.desktop_core().as_ref(),
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
                client_did: None,
                request_id: None,
                agent_did: Some(agent_did.clone()),
                session_id: None,
                content: HELD_PROMPT.to_string(),
                captured_context: None,
                attachments: None,
                delivery: None,
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
                fixture.desktop_core().refresh_node(&node_did).await?;
                let store = fixture.desktop_core().stores().snapshot(&node_did);
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
                client_did: None,
                request_id: None,
                agent_did: Some(agent_did.clone()),
                session_id: Some(initial.session_id.clone()),
                content: format!("/{skill_id}\n{task}"),
                captured_context: None,
                attachments: None,
                delivery: None,
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
                fixture.desktop_core().refresh_node(&node_did).await?;
                Ok(fixture
                    .desktop_core()
                    .stores()
                    .snapshot(&node_did)
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
        let access = fixture.desktop_core().operator_access(&node_did)?;
        let transcript_page =
            gents_observe::client_projection::query::load_session_transcript_page_on(
                &access,
                &submitted.session_id,
                Some(&node_did),
                Some(&requester_scope),
                None,
                None,
            )
            .await?;
        let context_store = gents_observe::client_projection::query::load_session_context_store_on(
            &access,
            &submitted.session_id,
            Some(&node_did),
            Some(&requester_scope),
        )
        .await?;
        let session = build_session_snapshot_for_node_with_transcript(
            fixture.desktop_core().as_ref(),
            &access,
            Some(&node_did),
            &submitted.session_id,
            Some(&submitted.request_id),
            Some(&transcript_page.store),
            Some(&transcript_page.canonical_dependencies),
            Some(&context_store),
            None,
            true,
            true,
            None,
        )
        .await?
        .context("desktop session snapshot missing after skill chat submit")?;
        assert_eq!(
            session.latest_request_id.as_deref(),
            Some(initial.request_id.as_str()),
            "initial={} submitted={} source_rows={:?} gate_permits={:?}",
            initial.request_id,
            submitted.request_id,
            fixture
                .desktop_core()
                .stores()
                .snapshot(&node_did)
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
            wait_for_remote_request(fixture.desktop_core().as_ref(), &submitted.request_id).await?;
        assert_eq!(
            request.requester_did.as_deref(),
            Some(requester_scope.as_str())
        );
        assert_eq!(request.content.as_deref(), Some(task));
        let input = request
            .input
            .as_ref()
            .context("runtime request is missing canonical input")?;
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
        agent_did: &str,
        skill_id: &str,
    ) -> Result<()> {
        use gents_documents::apply_desired_state_plan;
        use gents_documents::{
            read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        use gents_store::collection::Collection;
        fixture
            .desktop_core()
            .operator_access(node_did)?
            .transact_write("desktop.fixture.skill", |txn| {
                Box::pin(async move {
                    let (_, agent) = read_desired_state_record_in_txn(
                        txn,
                        Collection::Agent,
                        node_did,
                        agent_did,
                    )
                    .await?
                    .with_context(|| format!("agent {agent_did} is missing"))?;
                    let agent: gents_documents::Agent = serde_json::from_value(agent)?;
                    let context_id = agent.context_id.context("fixture agent has no context")?;
                    let (_, context) = read_desired_state_record_in_txn(
                        txn,
                        Collection::AgentContext,
                        node_did,
                        &context_id,
                    )
                    .await?
                    .context("fixture context is missing")?;
                    let mut context: gents_documents::AgentContext =
                        serde_json::from_value(context)?;
                    context.skill_ids = vec![skill_id.to_owned()];
                    let value = serde_json::to_value(context)?;
                    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                        collection: Collection::AgentContext,
                        add: value.clone(),
                        update: value,
                    }])?;
                    apply_desired_state_plan(
                        txn,
                        &plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        refresh_every_node(fixture.desktop_core()).await?;
        Ok(())
    }

    async fn wait_for_remote_context_skill_ids(
        core: &ClientRuntime,
        agent_did: &str,
        expected_skill_ids: &[&str],
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            refresh_every_node(core).await?;
            let observed = find_in_node_stores(core, |store| {
                store
                    .agents
                    .iter()
                    .find(|agent| agent.agent_did == agent_did)
                    .and_then(|agent| agent.context_id.as_deref())
                    .and_then(|context_id| {
                        store
                            .contexts
                            .iter()
                            .find(|context| context.context_id == context_id)
                    })
                    .map(|context| context.skill_ids.clone())
            });
            let observed = observed.as_deref();
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
                    "remote context for agent {agent_did} has skill IDs {observed:?}, expected {expected_skill_ids:?}"
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_remote_skill(core: &ClientRuntime, skill_id: &str) -> Result<()> {
        wait_for_condition("remote Skill create", Duration::from_secs(60), || async {
            refresh_every_node(core).await?;
            Ok(find_in_node_stores(core, |store| {
                store
                    .skills
                    .iter()
                    .any(|skill| skill.skill_id == skill_id)
                    .then_some(())
            })
            .is_some())
        })
        .await
    }

    async fn wait_for_remote_skill_absent(core: &ClientRuntime, skill_id: &str) -> Result<()> {
        wait_for_condition("remote Skill delete", Duration::from_secs(60), || async {
            refresh_every_node(core).await?;
            Ok(find_in_node_stores(core, |store| {
                store
                    .skills
                    .iter()
                    .any(|skill| skill.skill_id == skill_id)
                    .then_some(())
            })
            .is_none())
        })
        .await
    }

    async fn wait_for_remote_request(
        core: &ClientRuntime,
        request_id: &str,
    ) -> Result<AgentRequestRow> {
        wait_for_row("remote AgentRequest", Duration::from_secs(60), || async {
            refresh_every_node(core).await?;
            Ok(find_in_node_stores(core, |store| {
                store
                    .requests
                    .iter()
                    .find(|request| request.request_id == request_id)
                    .cloned()
            }))
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
        core: &ClientRuntime,
        node_did: &str,
    ) -> Result<Option<RemoteRuntimeObservation>> {
        core.refresh_node(node_did).await?;
        let store = core.stores().snapshot(node_did);
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

    async fn wait_for_remote_runtime_generation(
        core: &ClientRuntime,
        node_did: &str,
    ) -> Result<u64> {
        wait_for_row(
            "remote authoritative runtime generation",
            Duration::from_secs(60),
            || async { query_runtime_observation(core, node_did).await },
        )
        .await
        .map(|observation| observation.active_generation)
    }

    async fn wait_for_remote_runtime_generation_after(
        core: &ClientRuntime,
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

    // Drop unblocks the provider before fixture shutdown drains requests.
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
    }

    impl MockChatEndpoint {
        async fn start(model_name: &str, final_text: &str) -> Result<Self> {
            Self::start_inner(model_name, final_text, None).await
        }

        async fn start_with_held_prompt(model_name: &str, final_text: &str) -> Result<Self> {
            Self::start_inner(
                model_name,
                final_text,
                Some(Arc::new(tokio::sync::Semaphore::new(0))),
            )
            .await
        }

        async fn start_inner(
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
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .context("mock chat endpoint failed to bind a port")?;
            let port = listener.local_addr()?.port();
            let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
            let app = Router::new()
                .route("/v1/models", get(handle_models))
                .route("/models", get(handle_models))
                .route("/v1/chat/completions", post(handle_chat))
                .route("/chat/completions", post(handle_chat))
                .fallback(handle_fallback)
                .with_state(state);
            tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = shutdown_rx.await;
                    })
                    .await;
            });

            Ok(Self {
                endpoint: format!("http://127.0.0.1:{port}/v1"),
                held_response,
                captured,
                shutdown: Some(shutdown_tx),
            })
        }

        fn backend_override(&self, model_name: &str) -> LiveBackendOverride {
            LiveBackendOverride {
                inference_url: Some(self.endpoint.clone()),
                model_name: Some(model_name.to_string()),
                provider: Some(
                    gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible
                        .as_str()
                        .to_owned(),
                ),
                api_key: Some("desktop-live-test-key".to_string()),
                ..Default::default()
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
