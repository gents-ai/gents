use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, HttpConfig, P2PConfig};
#[cfg(test)]
use defra_node::{NodeBuilder, StorageBackend};
use defra_p2p_adapter::P2POperations as P2POps;
use p2p::iroh::parse_public_peer_addr;
use tokio::sync::{mpsc, watch};
use tokio::time::{sleep, Instant};

use super::super::observe::{spawn_observer_with_selection, ObservedStore};
use super::super::paths::DesktopPaths;
use super::super::peer_directory::{PeerDirectory, PeerRecord};
use super::super::principal_identity::PrincipalIdentity;
use super::super::query::load_full_snapshot_with_peer_records;
use super::super::schema::subscribe_all_collections;
use super::p2p_ops::{
    p2p_connect_peer, p2p_connected_peers, p2p_listen_addresses, p2p_local_peer_id,
};
use super::route_manager::{is_enrollment_peer, ClientRouteManager};
use super::supervisor::spawn_p2p_supervisor_task;
use super::{
    ClientCore, ClientCoreOptions, ClientPeerStatus, P2PHealth, BOOTSTRAP_OPERATION_BACKOFF,
    BOOTSTRAP_OPERATION_TIMEOUT,
};

impl ClientCore {
    pub async fn start() -> Result<Self> {
        let paths = DesktopPaths::discover()?;
        Self::start_with_paths(paths).await
    }

    pub async fn start_with_paths(paths: DesktopPaths) -> Result<Self> {
        Self::start_with_paths_and_options(paths, ClientCoreOptions::default()).await
    }

    pub async fn start_with_paths_and_options(
        paths: DesktopPaths,
        options: ClientCoreOptions,
    ) -> Result<Self> {
        Self::start_reporting_stages(paths, options, None).await
    }

    pub async fn start_with_paths_reporting_stages(
        paths: DesktopPaths,
        options: ClientCoreOptions,
        completed_stage: watch::Sender<&'static str>,
    ) -> Result<Self> {
        Self::start_reporting_stages(paths, options, Some(completed_stage)).await
    }

    async fn start_reporting_stages(
        paths: DesktopPaths,
        mut options: ClientCoreOptions,
        completed_stage: Option<watch::Sender<&'static str>>,
    ) -> Result<Self> {
        let started = Instant::now();
        let mut previous = started;
        let mut checkpoint = |stage: &'static str| {
            let now = Instant::now();
            tracing::info!(target: "gents_desktop_core::startup", stage,
                stage_ms = now.duration_since(previous).as_millis(),
                elapsed_ms = now.duration_since(started).as_millis(),
                "client startup stage completed");
            previous = now;
            if let Some(completed_stage) = completed_stage.as_ref() {
                completed_stage.send_replace(stage);
            }
        };
        paths.ensure_root_dirs().await?;
        gents::store::local::storage_backend::reject_legacy_store(paths.node_data_dir())?;
        let store_lock =
            gents::store::local::home::lock_store(paths.root(), paths.node_data_dir())?;
        let store_key = open_or_create_client_store_key(&paths, &options, &mut checkpoint).await?;
        options.store_key_provider = None;

        let node_identity = PrincipalIdentity::load_or_create(&paths).await?;
        checkpoint("paths_and_identity");
        let mut node_builder =
            gents::store::local::store_key::persistent_builder(paths.node_data_dir(), &store_key)?
                .with_p2p(desktop_p2p_config(&paths, &options))
                .with_node_identity_did(node_identity.did());
        if let Some(http_addr) = options.http_addr {
            node_builder = node_builder.with_http(HttpConfig::with_addr(http_addr));
        }
        let node = Arc::new(
            node_builder
                .build()
                .await
                .context("starting embedded desktop node")?,
        );
        let store_access = match gents::store::config_client::ConfigAccess::claimed_local(
            node.clone(),
            store_lock,
        ) {
            Ok(access) => access,
            Err(error) => {
                node.shutdown().await;
                return Err(error.into());
            }
        };

        checkpoint("embedded_node");

        let result: Result<Self> = async {
            gents::store::schema::migration::ensure_all_runtime_migrations(node.clone())
                .await
                .map_err(|error| {
                    gents::store::local::storage_backend::classify_store_error(
                        error,
                        paths.node_data_dir(),
                    )
                })?;
            checkpoint("runtime_schemas");

            let loaded_peer_directory =
                PeerDirectory::open_writer(paths.peer_directory_path()).await?;
            let sync_state = super::sync_state::ClientSyncStateOwner::new(
                P2PHealth::default(),
                loaded_peer_directory,
                Vec::new(),
            );
            sync_state.clear_ephemeral_pairing_readiness().await?;
            let records = sync_state.records();
            checkpoint("peer_directory");
            gents::store::local::store_key::upgrade::finish(paths.node_data_dir())?;
            subscribe_all_collections(node.as_ref()).await?;
            checkpoint("collection_subscriptions");

            let observer_subscription = node.subscribe_document_changes();

            let observation_scope = super::observation_scope::ObservationScope::new();

            let initial_snapshot = {
                load_full_snapshot_with_peer_records(
                    node.as_ref(),
                    &records,
                    node_identity.did(),
                    options.operator_identity_interaction,
                )
                .await?
            };
            let (store, _store_updates) = ObservedStore::new(initial_snapshot);
            checkpoint("initial_snapshot");

            let p2p = node
                .p2p_arc()
                .context("desktop node started without P2P support")?;
            let local_peer_id = p2p_local_peer_id(&p2p)
                .await
                .context("reading desktop P2P peer id")?;
            let listen_addresses = p2p_listen_addresses(&p2p)
                .await
                .context("reading desktop P2P listen addresses")?;
            let route_manager = Arc::new(ClientRouteManager::new(
                Arc::clone(&node),
                Arc::clone(&p2p),
                Arc::new(node_identity.clone()),
                options.operator_identity_interaction,
            ));

            let (peer_statuses, bootstrap_errors) = bootstrap_saved_peers(
                &node,
                &p2p,
                &records,
                &options,
                &node_identity,
                &route_manager,
                &mut || checkpoint("saved_peer"),
            )
            .await;
            let (initial_health, initial_database_sync, initial_database_sync_error) =
                super::supervisor::probe_p2p_health(&p2p, &P2PHealth::default(), None, None).await;
            checkpoint("bootstrap_and_health");
            for status in peer_statuses {
                if let Some(expected) = records
                    .iter()
                    .find(|record| record.peer_id == status.peer_id)
                {
                    sync_state.replace_peer(expected, status);
                }
            }
            sync_state.replace_database_observation(
                initial_health,
                initial_database_sync,
                initial_database_sync_error,
            );
            let observer = spawn_observer_with_selection(
                Arc::clone(&node),
                Arc::clone(&store),
                sync_state.clone(),
                node_identity.did().to_string(),
                observer_subscription,
                observation_scope.subscribe(),
                options.operator_identity_interaction,
            );
            let (p2p_control, p2p_control_rx) = mpsc::channel(8);
            let p2p_supervisor = spawn_p2p_supervisor_task(
                Arc::clone(&node),
                Arc::clone(&p2p),
                sync_state.clone(),
                p2p_control_rx,
                Arc::new(node_identity.clone()),
                local_peer_id.clone(),
                Arc::clone(&route_manager),
                options.install_replicators_on_bootstrap,
            );
            Ok(Self {
                store_access: tokio::sync::Mutex::new(None),
                paths,
                options,
                node_identity,
                node: Arc::clone(&node),
                p2p,
                route_manager,
                store,
                observer: tokio::sync::Mutex::new(Some(observer)),
                sync_state,
                p2p_supervisor: tokio::sync::Mutex::new(Some(p2p_supervisor)),
                observation_scope,
                inference_observations: Default::default(),
                inference_observation_epoch: Default::default(),
                request_patch_signatures: tokio::sync::Mutex::new(
                    rapidhash::RapidHashMap::default(),
                ),
                context_details: super::context_details::ContextDetailsCache::default(),
                p2p_control: tokio::sync::Mutex::new(Some(p2p_control)),
                shutdown_completion: tokio::sync::Mutex::new(None),
                #[cfg(test)]
                shutdown_test_pause: tokio::sync::Mutex::new(None),
                last_mutation_error: std::sync::RwLock::new(None),
                local_peer_id,
                listen_addresses,
                bootstrap_errors,
            })
        }
        .await;
        match result {
            Ok(mut core) => {
                *core.store_access.get_mut() = Some(store_access);
                Ok(core)
            }
            Err(error) => {
                node.shutdown().await;
                Err(error)
            }
        }
    }
}

fn desktop_p2p_config(paths: &DesktopPaths, options: &ClientCoreOptions) -> P2PConfig {
    P2PConfig {
        port: options.port,
        bind_addr: options.bind_addr,
        relay_mode: options.relay_mode.clone(),
        discovery: options.discovery.clone(),
        allowlist: p2p::iroh::IrohAllowlistConfig::AcceptAll,
        max_concurrent_multipath_paths: None,
        secret_key_path: Some(paths.iroh_secret_key_path().to_path_buf()),
        load_persisted_collections: options.load_persisted_collections,
        max_concurrent_dag_fetches: options.max_concurrent_dag_fetches,
        max_concurrent_push_tasks: options.max_concurrent_push_tasks,
        rate_limit_burst: options.rate_limit_burst,
        rate_limit_rate: options.rate_limit_rate,
        max_doc_sync_request_doc_ids: p2p::sync::DEFAULT_MAX_DOC_SYNC_REQUEST_DOC_IDS,
        max_pending_dags: options.max_pending_dags,
        // DefraDB forwards merges to explicit downstream replicators itself.
        rebroadcast_on_merge: false,
    }
}

pub(super) async fn bootstrap_saved_peers(
    node: &Arc<EmbeddedNode>,
    p2p: &Arc<dyn P2POps>,
    records: &[PeerRecord],
    options: &ClientCoreOptions,
    _actor: &PrincipalIdentity,
    route_manager: &Arc<ClientRouteManager>,
    peer_settled: &mut (dyn FnMut() + Send),
) -> (Vec<ClientPeerStatus>, Vec<String>) {
    let mut statuses = Vec::with_capacity(records.len());
    let mut errors = Vec::new();

    for record in records {
        let mut status = ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            node_did: record.node_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: false,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
        };

        // Enrollment documents, including a current signed route receipt, are
        // the sole authority for reconnecting this peer.
        if bootstrap_deferred_to_enrollment_authority(record) {
            statuses.push(status);
            peer_settled();
            continue;
        }

        match connect_peer_with_retry(node.event_bus().as_ref(), p2p, &record.addr, &record.label)
            .await
        {
            Ok(()) => {
                status.dial_succeeded = true;

                if options.install_replicators_on_bootstrap {
                    match route_manager.lock().await.configure(record).await {
                        Ok(()) => {}
                        Err(error) => {
                            let message = format!(
                                "peer {} local runtime pairing failed: {}",
                                record.label, error
                            );
                            status.last_error = Some(message.clone());
                            errors.push(message);
                        }
                    }
                }
            }
            Err(error) => {
                let message = format!("peer {} dial failed: {}", record.label, error);
                status.last_error = Some(message.clone());
                errors.push(message);
            }
        }

        statuses.push(status);
        peer_settled();
    }

    (statuses, errors)
}

fn bootstrap_deferred_to_enrollment_authority(record: &PeerRecord) -> bool {
    is_enrollment_peer(record)
}

pub(super) async fn connect_peer_with_retry(
    bus: &dyn events::Bus,
    p2p: &Arc<dyn P2POps>,
    addr: &str,
    label: &str,
) -> Result<()> {
    connect_peer_with_retry_until(bus, p2p, addr, label, BOOTSTRAP_OPERATION_TIMEOUT).await
}

pub(super) async fn force_connect_peer_with_retry(
    bus: &dyn events::Bus,
    p2p: &Arc<dyn P2POps>,
    addr: &str,
    label: &str,
) -> Result<()> {
    force_connect_peer_with_retry_until(bus, p2p, addr, label, BOOTSTRAP_OPERATION_TIMEOUT).await
}

pub(super) async fn connect_peer_with_retry_until(
    bus: &dyn events::Bus,
    p2p: &Arc<dyn P2POps>,
    addr: &str,
    label: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let expected_peer_id = parse_public_peer_addr(addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string());

    loop {
        if let Some(peer_id) = expected_peer_id.as_deref() {
            if is_connected_peer(p2p, peer_id).await? {
                return Ok(());
            }
        }

        match p2p_connect_peer(p2p, addr).await {
            Ok(()) => {
                if let Some(peer_id) = expected_peer_id.as_deref() {
                    wait_for_connected_peer(bus, p2p, peer_id, deadline, label).await?;
                }
                return Ok(());
            }
            Err(error) => {
                if let Some(peer_id) = expected_peer_id.as_deref() {
                    if is_connected_peer(p2p, peer_id).await? {
                        return Ok(());
                    }
                }
                if Instant::now() >= deadline {
                    anyhow::bail!("timed out connecting bootstrap peer {label} at {addr}: {error}");
                }
                sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
            }
        }
    }
}

pub(super) async fn force_connect_peer_with_retry_until(
    bus: &dyn events::Bus,
    p2p: &Arc<dyn P2POps>,
    addr: &str,
    label: &str,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let expected_peer_id = parse_public_peer_addr(addr)
        .ok()
        .map(|(peer_id, _)| peer_id.to_string());

    loop {
        match p2p_connect_peer(p2p, addr).await {
            Ok(()) => {
                if let Some(peer_id) = expected_peer_id.as_deref() {
                    wait_for_connected_peer(bus, p2p, peer_id, deadline, label).await?;
                }
                return Ok(());
            }
            Err(error) => {
                if let Some(peer_id) = expected_peer_id.as_deref() {
                    if is_connected_peer(p2p, peer_id).await? {
                        return Ok(());
                    }
                }
                if Instant::now() >= deadline {
                    anyhow::bail!(
                        "timed out force-connecting bootstrap peer {label} at {addr}: {error}"
                    );
                }
                sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
            }
        }
    }
}

pub(super) async fn is_connected_peer(p2p: &Arc<dyn P2POps>, peer_id: &str) -> Result<bool> {
    let peers = p2p_connected_peers(p2p).await?;
    Ok(peers
        .iter()
        .any(|peer| transport_entry_names_peer(peer, peer_id)))
}

fn transport_entry_names_peer(entry: &str, peer_id: &str) -> bool {
    parse_public_peer_addr(entry)
        .map(|(parsed_peer_id, _)| parsed_peer_id.as_str() == peer_id)
        .unwrap_or_else(|_| entry.contains(peer_id))
}

async fn wait_for_connected_peer(
    bus: &dyn events::Bus,
    p2p: &Arc<dyn P2POps>,
    peer_id: &str,
    deadline: Instant,
    label: &str,
) -> Result<()> {
    // Subscribe before the first observation so a connection that lands
    // between them still wakes this wait.
    let mut connections = bus.subscribe(&[events::EventName::PeerConnection]);
    if is_connected_peer(p2p, peer_id).await? {
        return Ok(());
    }
    let connected = tokio::time::timeout_at(deadline, async {
        while let Some(message) = connections.recv().await {
            if message.as_peer_connection().is_some_and(|change| {
                change.connected && transport_entry_names_peer(&change.peer_id, peer_id)
            }) {
                return true;
            }
        }
        false
    })
    .await;
    match connected {
        Ok(true) => Ok(()),
        // A closed bus delivers no further events; fall back to one live read.
        Ok(false) if is_connected_peer(p2p, peer_id).await? => Ok(()),
        _ => anyhow::bail!("timed out waiting for bootstrap peer {peer_id} to connect for {label}"),
    }
}

pub(super) fn normalize_required<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    (!trimmed.is_empty())
        .then_some(trimmed)
        .with_context(|| format!("{field} must not be empty"))
}

// Key custody is durable before the first encrypted write; existing plaintext
// stores are promoted offline while the client store lock is held.
async fn open_or_create_client_store_key(
    paths: &DesktopPaths,
    options: &ClientCoreOptions,
    progress: &mut (dyn FnMut(&'static str) + Send),
) -> Result<gents::store::local::store_key::StoreKey> {
    use gents::store::local::store_key::{upgrade, StoreEncryption};
    let choice = options.store_key_custody;
    let record_path = paths.store_encryption_path();
    let record = match std::fs::read(record_path) {
        Ok(bytes) => Some(
            serde_json::from_slice::<StoreEncryption>(&bytes)
                .with_context(|| format!("decoding {}", record_path.display()))?,
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", record_path.display()))
        }
    };
    let pending = upgrade::pending_record(paths.node_data_dir())?;
    if let (Some(record), Some(pending)) = (&record, &pending) {
        anyhow::ensure!(
            record == pending,
            "client encryption metadata does not match its pending upgrade"
        );
    }
    if pending.is_some() || (record.is_none() && paths.node_data_dir().join("MANIFEST").exists()) {
        let record = match pending {
            Some(record) => record,
            None => {
                let record = StoreEncryption::prepare(
                    choice,
                    paths.store_key_path(),
                    &upgrade::staging_path(paths.node_data_dir()),
                )?;
                upgrade::begin(paths.node_data_dir(), &record)?;
                record
            }
        };
        let key = client_store_key(
            options,
            &record,
            paths.store_key_path(),
            &upgrade::key_store_path(paths.node_data_dir())?,
        )
        .await?;
        upgrade::encrypt_existing_with_progress(paths.node_data_dir(), &record, &key, progress)
            .await?;
        write_client_store_record(paths, &record)?;
        return Ok(key);
    }
    let record = match record {
        Some(record) => record,
        None => {
            let record =
                StoreEncryption::prepare(choice, paths.store_key_path(), paths.node_data_dir())?;
            write_client_store_record(paths, &record)?;
            return match &options.store_key_provider {
                Some(provider) => {
                    provider
                        .create(&record, paths.store_key_path(), paths.node_data_dir())
                        .await
                }
                None => Ok(record.initialize(paths.store_key_path(), paths.node_data_dir())?),
            };
        }
    };
    client_store_key(
        options,
        &record,
        paths.store_key_path(),
        paths.node_data_dir(),
    )
    .await
}

async fn client_store_key(
    options: &ClientCoreOptions,
    record: &gents::store::local::store_key::StoreEncryption,
    key_file: &std::path::Path,
    data_path: &std::path::Path,
) -> Result<gents::store::local::store_key::StoreKey> {
    if let Some(provider) = &options.store_key_provider {
        provider.load(record, key_file, data_path).await
    } else {
        Ok(record.initialize(key_file, data_path)?)
    }
}

fn write_client_store_record(
    paths: &DesktopPaths,
    record: &gents::store::local::store_key::StoreEncryption,
) -> Result<()> {
    let record_path = paths.store_encryption_path();
    let staged = tempfile::NamedTempFile::new_in(paths.root())
        .with_context(|| format!("staging {}", record_path.display()))?;
    serde_json::to_writer(staged.as_file(), record)?;
    staged.as_file().sync_all()?;
    staged
        .persist(record_path)
        .with_context(|| format!("writing {}", record_path.display()))?;
    #[cfg(unix)]
    std::fs::File::open(paths.root())?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct RecordingKeyProvider {
        created: std::sync::atomic::AtomicUsize,
        loaded: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl super::super::ClientStoreKeyProvider for RecordingKeyProvider {
        async fn create(
            &self,
            record: &gents::store::local::store_key::StoreEncryption,
            key_file: &std::path::Path,
            data_path: &std::path::Path,
        ) -> Result<gents::store::local::store_key::StoreKey> {
            self.created
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(record.initialize(key_file, data_path)?)
        }
        async fn load(
            &self,
            record: &gents::store::local::store_key::StoreEncryption,
            key_file: &std::path::Path,
            data_path: &std::path::Path,
        ) -> Result<gents::store::local::store_key::StoreKey> {
            self.loaded
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(record.initialize(key_file, data_path)?)
        }
    }

    #[tokio::test]
    async fn only_a_new_store_uses_credential_creation_and_reopening_uses_retained_custody() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path().join("client"));
        paths.ensure_root_dirs().await.unwrap();
        let _lock =
            gents::store::local::home::lock_store(paths.root(), paths.node_data_dir()).unwrap();
        let provider = Arc::new(RecordingKeyProvider::default());
        let options = ClientCoreOptions {
            store_key_provider: Some(provider.clone()),
            ..ClientCoreOptions::local_only()
        };
        let first = open_or_create_client_store_key(&paths, &options, &mut |_| {})
            .await
            .unwrap();
        let metadata = std::fs::read(paths.store_encryption_path()).unwrap();
        let reopened = open_or_create_client_store_key(&paths, &options, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(
            provider.created.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(provider.loaded.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            std::fs::read(paths.store_encryption_path()).unwrap(),
            metadata
        );
        first.with_secret(|left| reopened.with_secret(|right| assert!(left == right)));
    }

    #[tokio::test]
    async fn plaintext_client_upgrade_preserves_identity_history_and_subscription_metadata() {
        use gents::agent::p2p_reconcile::{EmbeddedRemoteP2pAdmin, RemoteP2pAdmin};
        use gents::store::config_client::ConfigAccess;
        use gents::store::graphql::escape_graphql_string;

        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path().join("client"));
        paths.ensure_root_dirs().await.unwrap();
        let node_identity = PrincipalIdentity::load_or_create(&paths).await.unwrap();
        let identity_bytes = std::fs::read(paths.identity_key_path()).unwrap();
        let plain = Arc::new(
            NodeBuilder::default()
                .data_path(paths.node_data_dir())
                .with_storage_backend(StorageBackend::Regolith)
                .with_node_identity_did(node_identity.did())
                .with_p2p(desktop_p2p_config(&paths, &ClientCoreOptions::local_only()))
                .build()
                .await
                .unwrap(),
        );
        let baseline = gents_migration::DEFAULT_BASELINE
            .iter()
            .copied()
            .filter(|entry| {
                ![
                    gents_protocol::schemas::TOOL_FILE_CHANGE_NAME,
                    gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME,
                ]
                .contains(&entry.name)
            })
            .collect::<Vec<_>>();
        let registry = gents_migration::Registry {
            baseline: &baseline,
            ..gents_migration::DEFAULT_REGISTRY
        };
        gents_migration::ensure_migrations_with_registry(&plain, &registry)
            .await
            .unwrap();
        let access = ConfigAccess::local(Arc::clone(&plain));
        assert!(access
            .collection_version(gents_protocol::schemas::TOOL_FILE_CHANGE_NAME)
            .await
            .unwrap()
            .is_none());
        let mut expected_versions = Vec::new();
        for entry in &baseline {
            let collection = access
                .collection_version(entry.name)
                .await
                .unwrap()
                .unwrap();
            expected_versions.push((entry.name, collection["VersionID"].clone()));
        }
        access
            .write(
                "test.client_upgrade.session",
                &format!(
                    r#"mutation {{ create_AgentSession(input: {{
                session_id: "upgrade-session", node_did: "{}", agent_id: "default",
                title: {{ text: "before upgrade", source: "user" }},
                created_at: "2026-09-30T00:00:00Z"
            }}) {{ _docID }} }}"#,
                    escape_graphql_string(node_identity.did()),
                ),
            )
            .await
            .unwrap();
        let query = r#"{ AgentSession(filter: {session_id: {_eq: "upgrade-session"}}) {
            _docID session_id node_did title created_at
        } }"#;
        let before = access.execute(query).await.unwrap();
        let doc_id = before["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap();
        access
            .write(
                "test.client_upgrade.title",
                &format!(
                    r#"mutation {{ update_AgentSession(docID: "{}", input: {{
                title: {{ text: "preserved conversation", source: "user" }}
            }}) {{ _docID }} }}"#,
                    escape_graphql_string(doc_id),
                ),
            )
            .await
            .unwrap();
        let history_query = format!(
            r#"{{ _commits(docID: ["{}"], depth: 20, filter: {{fieldName: {{_eq: "_C"}}}}) {{ cid height }} }}"#,
            escape_graphql_string(doc_id),
        );
        let expected_document = access.execute(query).await.unwrap();
        let expected_history = access.execute(&history_query).await.unwrap();
        assert!(
            expected_history["data"]["_commits"]
                .as_array()
                .unwrap()
                .len()
                >= 2
        );

        let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&plain));
        let sensitive = [
            "InferenceBackend".to_string(),
            "OAuthCredential".to_string(),
        ];
        admin.add_p2p_collections(&sensitive).await.unwrap();
        let old_subscriptions = admin.list_p2p_collections().await.unwrap();
        assert!(sensitive
            .iter()
            .all(|name| old_subscriptions.contains(name)));
        drop(admin);
        drop(access);
        plain.shutdown().await;
        drop(plain);
        assert!(!paths.store_encryption_path().exists());

        for _ in 0..2 {
            let options = ClientCoreOptions::local_only();
            assert!(!desktop_p2p_config(&paths, &options).load_persisted_collections);
            let core = ClientCore::start_with_paths_and_options(paths.clone(), options)
                .await
                .unwrap();
            assert_eq!(core.node_identity().did(), node_identity.did());
            assert_eq!(
                std::fs::read(paths.identity_key_path()).unwrap(),
                identity_bytes
            );
            let access = ConfigAccess::local(core.node_arc());
            for (name, version) in &expected_versions {
                let collection = access.collection_version(name).await.unwrap().unwrap();
                assert_eq!(&collection["VersionID"], version, "{name}");
            }
            assert!(access
                .collection_version(gents_protocol::schemas::TOOL_FILE_CHANGE_NAME)
                .await
                .unwrap()
                .is_some());
            assert!(access
                .collection_version(gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST_NAME)
                .await
                .unwrap()
                .is_some());
            assert_eq!(access.execute(query).await.unwrap(), expected_document);
            assert_eq!(
                access.execute(&history_query).await.unwrap(),
                expected_history
            );
            let admin = EmbeddedRemoteP2pAdmin::new(core.node_arc());
            let subscriptions = admin.list_p2p_collections().await.unwrap();
            assert!(sensitive.iter().all(|name| subscriptions.contains(name)));
            drop(admin);
            drop(access);
            let core = Arc::new(core);
            core.shutdown().await.unwrap();
            drop(core);
            assert!(paths.store_encryption_path().is_file());
        }
    }

    #[tokio::test]
    async fn failed_bootstrap_stops_its_peer_before_retrying_with_the_same_key() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path().join("client"));
        paths.ensure_root_dirs().await.unwrap();
        std::fs::create_dir(paths.peer_directory_path()).unwrap();
        let reservation = std::net::UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = reservation.local_addr().unwrap();
        let options = ClientCoreOptions {
            port: address.port(),
            ..ClientCoreOptions::local_only()
        };
        drop(reservation);
        let (stages, completed) = watch::channel("none");
        let failed =
            ClientCore::start_with_paths_reporting_stages(paths.clone(), options.clone(), stages)
                .await;
        assert!(failed.is_err());
        assert_eq!(*completed.borrow(), "runtime_schemas");
        let released = std::net::UdpSocket::bind(address)
            .expect("failed startup must shut down its native P2P endpoint");
        let key = std::fs::read(paths.store_key_path()).unwrap();
        let identity = std::fs::read(paths.identity_key_path()).unwrap();
        std::fs::remove_dir(paths.peer_directory_path()).unwrap();
        drop(released);

        let core = ClientCore::start_with_paths_and_options(paths.clone(), options)
            .await
            .unwrap();
        assert_eq!(std::fs::read(paths.store_key_path()).unwrap(), key);
        assert_eq!(std::fs::read(paths.identity_key_path()).unwrap(), identity);
        Arc::new(core).shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn overlapping_client_starts_cannot_replace_the_key_or_open_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(temp.path().join("client"));
        let core = ClientCore::start_with_paths_and_options(
            paths.clone(),
            ClientCoreOptions::local_only(),
        )
        .await
        .unwrap();
        let key = std::fs::read(paths.store_key_path()).unwrap();
        let did = core.node_identity().did().to_string();
        let error = match ClientCore::start_with_paths_and_options(
            paths.clone(),
            ClientCoreOptions::local_only(),
        )
        .await
        {
            Ok(_) => panic!("a second client must not open the claimed store"),
            Err(error) => error,
        };
        assert!(
            gents::store::local::home::store_lock_held(error.as_ref()).is_some(),
            "{error:#}"
        );
        assert_eq!(std::fs::read(paths.store_key_path()).unwrap(), key);
        let core = Arc::new(core);
        core.shutdown().await.unwrap();
        drop(core);
        let reopened =
            ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
                .await
                .unwrap();
        assert_eq!(reopened.node_identity().did(), did);
        Arc::new(reopened).shutdown().await.unwrap();
    }
}
