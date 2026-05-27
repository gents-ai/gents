#[path = "live_fixture/agent.rs"]
mod agent;
#[path = "live_fixture/backend.rs"]
mod backend;
#[path = "live_fixture/replication.rs"]
mod replication;
#[path = "live_fixture/workspace.rs"]
mod workspace;

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use defra_agent_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths, PeerRecord};
use defra_agent_desktop_core::local_runtime::DesktopInitSummary;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tracing_subscriber::prelude::*;

use crate::bridge::types::{DesktopBootstrapSummary, SavedPeerView};

use self::agent::{spawn_live_agent, RunningAgent};
use self::backend::AgentBackendConfig;
pub(crate) use self::backend::LiveBackendOverride;
pub(crate) use self::backend::LiveSubagentBackendOverride;
use self::replication::{
    configure_live_replicators, wait_for_connectable_iroh_addr, wait_for_connected_peer,
    wait_for_live_documents, write_peer_directory_records,
};
use self::workspace::seed_runner_agent_home;

const DEFAULT_DEPLOYMENT_LABEL: &str = "Amy Server";
const DEFAULT_AGENT_NAME: &str = "amy";

pub(crate) struct LiveBridgeFixture {
    runtime: Arc<Runtime>,
    _tempdir: tempfile::TempDir,
    desktop_paths: DesktopPaths,
    agent_home: PathBuf,
    desktop_core: Arc<ClientCore>,
    remote_core: Arc<ClientCore>,
    deployment_label: String,
    agent_did: String,
    tool_root: PathBuf,
    init_summary: DesktopInitSummary,
    bootstrap_saved_peers: Vec<SavedPeerView>,
    update_version: Arc<AtomicU64>,
    update_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    running_agent: Mutex<Option<RunningAgent>>,
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

    pub(crate) fn agent_did(&self) -> &str {
        &self.agent_did
    }

    pub(crate) fn deployment_label(&self) -> &str {
        &self.deployment_label
    }

    pub(crate) fn tool_root(&self) -> &Path {
        &self.tool_root
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

        if let Some(agent) = self.running_agent.lock().await.take() {
            agent.shutdown().await?;
        }

        self.remote_core.shutdown().await?;
        self.desktop_core.shutdown().await?;
        Ok(())
    }

    pub(crate) fn start(
        backend_override: Option<LiveBackendOverride>,
        subagent_backend_override: Option<LiveSubagentBackendOverride>,
    ) -> Result<Arc<Self>> {
        init_live_runner_tracing();

        let backend = AgentBackendConfig::resolve(backend_override.as_ref())?;
        let subagent_backend =
            AgentBackendConfig::resolve_subagent(subagent_backend_override.as_ref(), &backend)?;
        let runtime = live_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let agent_home = tempdir.path().join("agent-home");

        let remote_core = Arc::new(runtime.block_on(ClientCore::start_with_paths_and_options(
            remote_paths,
            live_core_options(),
        ))?);

        let agent_key = tempdir.path().join("agent").join("amy.key");
        let (running_agent, docs, tool_root) = runtime.block_on(spawn_live_agent(
            Arc::clone(&remote_core),
            agent_key,
            DEFAULT_AGENT_NAME,
            &backend,
            subagent_backend.as_ref(),
        ))?;

        let remote_addr = runtime.block_on(wait_for_connectable_iroh_addr(
            remote_core.as_ref(),
            DEFAULT_DEPLOYMENT_LABEL,
        ))?;
        let mut peer_record =
            PeerRecord::new(DEFAULT_DEPLOYMENT_LABEL, &remote_addr, &running_agent.did);
        peer_record.source = Some("bridge-runner".to_string());
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
        runtime.block_on(wait_for_live_documents(
            desktop_core.as_ref(),
            &running_agent.did,
            &docs,
        ))?;

        seed_runner_agent_home(
            &agent_home,
            DEFAULT_AGENT_NAME,
            &running_agent.did,
            remote_core.local_peer_id(),
            &remote_addr,
        )?;

        let remote_peer_id = remote_core.local_peer_id().to_string();
        let init_summary = DesktopInitSummary {
            status: "initialized",
            source: "bridge-runner",
            status_endpoint: None,
            agent_home: agent_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            peer_directory: desktop_paths.peer_directory_path().display().to_string(),
            label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            agent_name: DEFAULT_AGENT_NAME.to_string(),
            agent_did: running_agent.did.clone(),
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
            agent_did: peer_record.agent_did.clone(),
            addr: peer_record.addr.clone(),
            source: peer_record.source.clone(),
            graphql: peer_record.graphql.clone(),
        }];

        tracing::info!(
            agent_did = %running_agent.did,
            tool_root = %tool_root.display(),
            "live bridge fixture ready"
        );

        let update_version = Arc::new(AtomicU64::new(1));
        let update_task = {
            let desktop_core = Arc::clone(&desktop_core);
            let update_version = Arc::clone(&update_version);
            runtime.spawn(async move {
                let mut store_updates = desktop_core.store_updates();
                let mut health_updates = desktop_core.p2p_health_updates();
                loop {
                    tokio::select! {
                        changed = store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = health_updates.changed() => {
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
            _tempdir: tempdir,
            desktop_paths,
            agent_home,
            desktop_core,
            remote_core,
            deployment_label: DEFAULT_DEPLOYMENT_LABEL.to_string(),
            agent_did: running_agent.did.clone(),
            tool_root,
            init_summary,
            bootstrap_saved_peers,
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_agent: Mutex::new(Some(running_agent)),
            shutdown_started: AtomicBool::new(false),
        }))
    }

    pub(crate) fn start_desktop_only() -> Result<Arc<Self>> {
        init_live_runner_tracing();

        let runtime = live_runtime()?;
        let tempdir = tempfile::tempdir()?;
        let remote_paths = DesktopPaths::from_root(tempdir.path().join("remote-empty"));
        let desktop_paths = DesktopPaths::from_root(tempdir.path().join("desktop"));
        let agent_home = tempdir.path().join("agent-home");
        std::fs::create_dir_all(&agent_home)?;

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
            status_endpoint: None,
            agent_home: agent_home.display().to_string(),
            desktop_home: desktop_paths.root().display().to_string(),
            peer_directory: desktop_paths.peer_directory_path().display().to_string(),
            label: "Desktop Only".to_string(),
            agent_name: String::new(),
            agent_did: String::new(),
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
                let mut health_updates = desktop_core.p2p_health_updates();
                loop {
                    tokio::select! {
                        changed = store_updates.changed() => {
                            if changed.is_err() {
                                break;
                            }
                            update_version.fetch_add(1, Ordering::SeqCst);
                        }
                        changed = health_updates.changed() => {
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
            _tempdir: tempdir,
            desktop_paths,
            agent_home,
            desktop_core,
            remote_core,
            deployment_label: "Desktop Only".to_string(),
            agent_did: String::new(),
            tool_root: PathBuf::new(),
            init_summary,
            bootstrap_saved_peers: Vec::new(),
            update_version,
            update_task: Mutex::new(Some(update_task)),
            running_agent: Mutex::new(None),
            shutdown_started: AtomicBool::new(false),
        }))
    }

    pub(crate) async fn build_bootstrap_summary(&self) -> DesktopBootstrapSummary {
        DesktopBootstrapSummary {
            default_agent_home: self.agent_home.display().to_string(),
            init_agent_name: non_empty_clone(&self.init_summary.agent_name),
            init_agent_did: non_empty_clone(&self.init_summary.agent_did),
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
            log_file_path: self.desktop_paths.log_file_path().display().to_string(),
            agent_home_exists: self.agent_home.exists(),
            desktop_home_exists: self.desktop_paths.root().exists(),
            peer_directory_exists: self.desktop_paths.peer_directory_path().exists(),
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
        let filter = std::env::var("DEFRA_AGENT_DESKTOP_TEST_LOG")
            .map(tracing_subscriber::EnvFilter::new)
            .unwrap_or_else(|_| {
                tracing_subscriber::EnvFilter::new("warn,defra_agent_desktop_tauri=info")
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
