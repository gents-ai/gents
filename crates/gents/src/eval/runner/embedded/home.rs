use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::defra_node::{Node as EmbeddedNode, P2PConfig, QueryResponse};
use crate::store::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::{ensure_runtime_schemas, DocumentRuntimeOptions, Gents, KeyIdentity, NodeIdentity};

// A full conformance run starts many embedded DefraDB nodes in parallel. On a
// busy CI host, a healthy runtime can spend more than 60 seconds waiting for
// startup and recovery I/O before it publishes its ready status row.
pub(crate) const RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// Node-owned services can hold `EmbeddedNode` handles briefly after shutdown.
const NODE_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds a home's P2P configuration from its data directory, so a reopen
/// restores the same persisted key and collections.
pub type P2PConfigForPath = Arc<dyn Fn(&Path) -> P2PConfig + Send + Sync>;

/// Trial homes are harness scratch. Their file-custodied store key and data
/// live together; the key binds the fresh-store provenance, not host secrecy.
pub struct EmbeddedHome {
    pub node: Arc<EmbeddedNode>,
    pub identity: Arc<dyn NodeIdentity>,
    evaluator: Option<(
        Arc<dyn NodeIdentity>,
        crate::store::config_client::ClientSigningCapability,
    )>,
    client_endpoint: Option<String>,
    did: String,
    path: PathBuf,
    data_path: PathBuf,
    store_key: crate::store::local::store_key::StoreKey,
    store_lock: crate::store::local::home::StoreLock,
    backend_admission: crate::store::local::backend_protection::BackendAdmission,
    p2p: Option<P2PConfigForPath>,
    node_acp_enabled: bool,
    #[allow(dead_code)]
    tempdir: Option<TempDir>,
}

#[cfg(test)]
pub(super) fn bind_test_evaluator(home: &mut EmbeddedHome) -> Arc<KeyIdentity> {
    let identity =
        Arc::new(KeyIdentity::load_or_create(home.path().join("evaluator.key"), None).unwrap());
    home.set_evaluator(
        identity.clone(),
        identity.client_signing_capability().unwrap(),
    );
    identity
}

impl std::fmt::Debug for EmbeddedHome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedHome")
            .field("did", &self.did)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl EmbeddedHome {
    pub async fn create_temp(prefix: &str) -> Result<Self> {
        let tempdir = tempfile::Builder::new()
            .prefix(&format!("gents-{prefix}-"))
            .tempdir()?;
        Self::in_tempdir(tempdir, None).await
    }

    /// Create a temporary home with native Node access control enabled.
    pub async fn create_temp_with_node_acp(prefix: &str) -> Result<Self> {
        let tempdir = tempfile::Builder::new()
            .prefix(&format!("gents-{prefix}-"))
            .tempdir()?;
        let mut home = Self::open_at_mode(tempdir.path().to_path_buf(), None, true).await?;
        home.tempdir = Some(tempdir);
        Ok(home)
    }

    #[cfg(test)]
    pub(super) async fn create_trial_temp(prefix: &str) -> Result<Self> {
        Self::create_temp_with_node_acp(prefix).await
    }

    pub async fn in_tempdir(tempdir: TempDir, p2p: Option<P2PConfigForPath>) -> Result<Self> {
        let mut home = Self::open_at(tempdir.path().to_path_buf(), p2p).await?;
        home.tempdir = Some(tempdir);
        Ok(home)
    }

    pub async fn create_retained(dir: &Path) -> Result<Self> {
        Self::create_retained_with_p2p(dir, None).await
    }

    pub(super) async fn create_trial_retained(dir: &Path) -> Result<Self> {
        ensure!(!dir.exists(), "trial home {} already exists", dir.display());
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        Self::open_at_mode(dir.to_path_buf(), None, true).await
    }

    pub async fn create_retained_with_p2p(
        dir: &Path,
        p2p: Option<P2PConfigForPath>,
    ) -> Result<Self> {
        ensure!(!dir.exists(), "trial home {} already exists", dir.display());
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        Self::open_at(dir.to_path_buf(), p2p).await
    }

    pub async fn open_retained(dir: &Path) -> Result<Self> {
        ensure!(
            dir.join("node.key").exists(),
            "no trial home at {}",
            dir.display()
        );
        Self::open_at(dir.to_path_buf(), None).await
    }

    async fn open_at(path: PathBuf, p2p: Option<P2PConfigForPath>) -> Result<Self> {
        Self::open_at_mode(path, p2p, false).await
    }

    async fn open_at_mode(
        path: PathBuf,
        p2p: Option<P2PConfigForPath>,
        node_acp_enabled: bool,
    ) -> Result<Self> {
        ensure!(
            !tokio::fs::try_exists(path.join("MANIFEST")).await?,
            "legacy trial store has no backend protection provenance"
        );
        let retained = tokio::fs::try_exists(path.join("data").join("MANIFEST")).await?;
        let identity_path = path.join("node.key");
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            tokio::task::spawn_blocking(move || {
                if retained {
                    KeyIdentity::load_existing(identity_path, None)
                } else {
                    KeyIdentity::load_or_create(identity_path, None)
                }
            })
            .await?
            .context("node identity")?,
        );
        Self::open_with_identity(path, p2p, identity, true, node_acp_enabled).await
    }

    pub(super) async fn open_trial_retained(dir: &Path) -> Result<Self> {
        ensure!(
            dir.join("node.key").exists(),
            "no trial home at {}",
            dir.display()
        );
        Self::open_at_mode(dir.to_path_buf(), None, true).await
    }

    pub(super) async fn open_for_inspection(dir: &Path, expected_did: &str) -> Result<Self> {
        ensure!(
            dir.is_dir(),
            "retained trial home {} is missing",
            dir.display()
        );
        crate::store::local::storage_backend::require_existing_store(&dir.join("data"))?;
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_existing(dir.join("node.key"), None)
                .context("loading the retained trial identity")?,
        );
        ensure!(
            identity.did() == expected_did,
            "retained trial identity {} does not match recorded DID {expected_did}",
            identity.did()
        );
        Self::open_with_identity(dir.to_path_buf(), None, identity, false, true).await
    }

    async fn open_with_identity(
        path: PathBuf,
        p2p: Option<P2PConfigForPath>,
        identity: Arc<dyn NodeIdentity>,
        initialize_schemas: bool,
        _node_acp_enabled: bool,
    ) -> Result<Self> {
        let did = identity.did().to_string();
        ensure!(
            !tokio::fs::try_exists(path.join("MANIFEST")).await?,
            "legacy trial store has no backend protection provenance"
        );
        let data_path = path.join("data");
        if !tokio::fs::try_exists(&data_path).await? {
            ensure!(initialize_schemas, "retained trial store is unavailable");
            tokio::fs::create_dir_all(&data_path).await?;
        }
        let lock_home = path.clone();
        let lock_data = data_path.clone();
        let store_lock = tokio::task::spawn_blocking(move || {
            crate::store::local::home::lock_store(&lock_home, &lock_data)
        })
        .await??;
        let fresh = tokio::fs::read_dir(&data_path)
            .await?
            .next_entry()
            .await?
            .is_none();
        let fresh_store = if fresh {
            ensure!(initialize_schemas, "retained trial store is unavailable");
            Some(
                crate::store::local::backend_protection::FreshBackendStore::observe(
                    &data_path,
                    &store_lock,
                )
                .await?,
            )
        } else {
            None
        };
        let store_key = trial_store_key(&path, &data_path, fresh).await?;
        if let Some(fresh_store) = fresh_store {
            fresh_store.initialize(&store_key, Some(&did)).await?;
        }
        let protected = crate::store::local::backend_protection::ProtectedBackendStore::open(
            &data_path,
            &store_key,
            &store_lock,
        )
        .await?
        .preflight(&store_key, Some(&did))
        .await?;
        let (http, client_endpoint) = trial_http().await?;
        let (node, backend_admission) = protected
            .start_node(
                &store_key,
                identity.clone(),
                Some(http),
                p2p.as_ref().map(|p| p(&path)),
            )
            .await?;
        if initialize_schemas {
            ensure_runtime_schemas(&node)
                .await
                .context("runtime schemas")?;
        }
        Ok(Self {
            node,
            identity,
            evaluator: None,
            client_endpoint: Some(client_endpoint),
            did,
            path,
            data_path,
            store_key,
            store_lock,
            backend_admission,
            p2p,
            node_acp_enabled: true,
            tempdir: None,
        })
    }

    pub fn did(&self) -> &str {
        &self.did
    }

    /// Address of this home's running GraphQL endpoint, when one is installed.
    pub fn graphql_endpoint(&self) -> Option<&str> {
        self.client_endpoint.as_deref()
    }

    pub fn set_evaluator(
        &mut self,
        identity: Arc<dyn NodeIdentity>,
        capability: crate::store::config_client::ClientSigningCapability,
    ) {
        self.evaluator = Some((identity, capability));
    }

    pub(super) fn evaluator(&self, did: &str) -> Result<&dyn NodeIdentity> {
        let identity = self
            .evaluator
            .as_ref()
            .map(|(identity, _)| identity.as_ref())
            .context("evaluation requires the held client signing identity")?;
        ensure!(
            identity.did() == did,
            "held evaluator identity does not match the frozen evaluator DID"
        );
        Ok(identity)
    }

    pub fn client_access(&self, did: &str) -> Result<crate::ConfigAccess> {
        self.evaluator(did)?;
        let endpoint = self
            .client_endpoint
            .as_ref()
            .context("trial client HTTP endpoint is unavailable")?;
        let (_, capability) = self
            .evaluator
            .as_ref()
            .context("evaluation requires the held client signing capability")?;
        Ok(crate::ConfigAccess::Graphql(
            crate::store::config_client::GraphqlEndpoint::as_principal(endpoint, did)
                .with_client_signing_capability(capability.clone())?
                .with_client_signed_agent_creates(),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn node_acp_enabled(&self) -> bool {
        self.node_acp_enabled
    }

    /// The admission is bound to this home's protected native node.
    pub fn backend_admission(&self) -> crate::store::local::backend_protection::BackendAdmission {
        self.backend_admission.clone()
    }

    pub async fn reopen(&mut self) -> Result<()> {
        // Stopping the node closes its subscriptions and other node-owned
        // services; wait for their handles to be released before claiming an
        // exclusive durable-store reopen.
        self.node
            .shutdown()
            .await
            .context("shutting down trial node")?;
        let release_deadline = tokio::time::Instant::now() + NODE_RELEASE_TIMEOUT;
        while Arc::strong_count(&self.node) != 1 && tokio::time::Instant::now() < release_deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let strong = Arc::strong_count(&self.node);
        if strong != 1 {
            bail!(
                "simulate_process_crash: stopped EmbeddedNode retained live owners \
                 (strong_count={strong}); crash boundary would not clear process state"
            );
        }

        let stand_in = Arc::new(
            EmbeddedNode::builder()
                .build()
                .await
                .map_err(|e| anyhow!("simulate_process_crash: stand-in node: {e}"))?,
        );
        let old = std::mem::replace(&mut self.node, stand_in);

        match Arc::try_unwrap(old) {
            Ok(owned) => drop(owned),
            Err(shared) => {
                let count = Arc::strong_count(&shared);
                self.node = shared;
                bail!(
                    "simulate_process_crash: cannot exclusively drop EmbeddedNode \
                     (strong_count={count} after replace); restored handle is shut \
                     down and unusable — fix outstanding Arc clones before Crash"
                );
            }
        }

        let protected = crate::store::local::backend_protection::ProtectedBackendStore::open(
            &self.data_path,
            &self.store_key,
            &self.store_lock,
        )
        .await?
        .preflight(&self.store_key, Some(&self.did))
        .await?;
        let (http, client_endpoint) = trial_http().await?;
        let (node, admission) = protected
            .start_node(
                &self.store_key,
                self.identity.clone(),
                Some(http),
                self.p2p.as_ref().map(|p| p(&self.path)),
            )
            .await?;
        self.node = node;
        self.backend_admission = admission;
        self.client_endpoint = Some(client_endpoint);

        ensure_runtime_schemas(&self.node)
            .await
            .map_err(|e| anyhow!("simulate_process_crash: ensure schemas: {e}"))?;
        Ok(())
    }
}

async fn trial_http() -> Result<(crate::defra_node::HttpConfig, String)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let config = crate::defra_node::HttpConfig::with_listener(listener.into_std()?)?;
    Ok((config, endpoint))
}

async fn trial_store_key(
    path: &Path,
    data: &Path,
    fresh: bool,
) -> Result<crate::store::local::store_key::StoreKey> {
    use crate::store::local::store_key::{StoreEncryption, StoreKeyCustodyChoice};
    use tokio::io::AsyncWriteExt;
    let key_file = path.join("store.key");
    let record_path = path.join("store-custody.json");
    let record = if fresh {
        let (key_file, data) = (key_file.clone(), data.to_owned());
        let record = tokio::task::spawn_blocking(move || {
            StoreEncryption::prepare(StoreKeyCustodyChoice::File, &key_file, &data)
        })
        .await??;
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&record_path)
            .await?;
        file.write_all(&serde_json::to_vec(&record)?).await?;
        file.sync_all().await?;
        record
    } else {
        serde_json::from_slice::<StoreEncryption>(&tokio::fs::read(&record_path).await?)?
    };
    let data = data.to_owned();
    tokio::task::spawn_blocking(move || {
        if fresh {
            record.initialize(&key_file, &data)
        } else {
            record.load(&key_file, &data)
        }
    })
    .await?
    .map_err(Into::into)
}

pub struct RunningRuntime {
    pub shutdown: watch::Sender<bool>,
    pub handle: JoinHandle<Result<()>>,
    pub node_did: String,
}

impl RunningRuntime {
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown.send(true);
        self.handle.await.context("runtime task")?
    }
}

pub async fn boot_runtime(
    home: &EmbeddedHome,
    identity: Arc<dyn NodeIdentity>,
    options: DocumentRuntimeOptions,
) -> Result<(RunningRuntime, Gents)> {
    boot_runtime_within(home, identity, options, RUNTIME_READY_TIMEOUT).await
}

/// [`boot_runtime`] with an explicit readiness budget.
///
/// A runtime that stops during startup ends the wait with its own error. One
/// that is still running but never became ready still holds the node, so it
/// is stopped before the readiness error is reported. That stop is bounded, so
/// a runtime that does not answer its shutdown signal cannot turn a reported
/// failure into a hang.
pub(crate) async fn boot_runtime_within(
    home: &EmbeddedHome,
    identity: Arc<dyn NodeIdentity>,
    mut options: DocumentRuntimeOptions,
    ready_timeout: Duration,
) -> Result<(RunningRuntime, Gents)> {
    options.backend_admission = Some(home.backend_admission.clone());
    let agent = Gents::from_default_agent_documents(home.node.clone(), identity, options).await?;
    let node_did = agent.node_did().to_string();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let mut handle = tokio::spawn(agent.clone().run(shutdown_rx));
    let ready = tokio::select! {
        ready = wait_for_runtime_ready_within(home.node.as_ref(), &node_did, ready_timeout) => ready,
        stopped = &mut handle => {
            return Err(match stopped {
                Ok(Ok(())) => anyhow!("the runtime stopped before it became ready"),
                Ok(Err(error)) => error.context("the runtime stopped before it became ready"),
                Err(join) => anyhow!("the runtime task ended before it became ready: {join}"),
            });
        }
    };
    let runtime = RunningRuntime {
        shutdown,
        handle,
        node_did: node_did.clone(),
    };
    if let Err(error) = ready {
        match tokio::time::timeout(RUNTIME_READY_TIMEOUT, runtime.shutdown()).await {
            Ok(Ok(())) => {}
            Ok(Err(stopping)) => tracing::warn!(
                error = %format!("{stopping:#}"),
                node_did = %node_did,
                "runtime that never became ready did not stop cleanly"
            ),
            Err(_) => tracing::warn!(
                node_did = %node_did,
                timeout = ?RUNTIME_READY_TIMEOUT,
                "runtime that never became ready did not stop within its budget; abandoning it"
            ),
        }
        return Err(error);
    }
    Ok((runtime, agent))
}

pub async fn wait_for_runtime_ready(node: &EmbeddedNode, node_did: &str) -> Result<()> {
    wait_for_runtime_ready_within(node, node_did, RUNTIME_READY_TIMEOUT).await
}

async fn wait_for_runtime_ready_within(
    node: &EmbeddedNode,
    node_did: &str,
    ready_timeout: Duration,
) -> Result<()> {
    let deadline = tokio::time::Instant::now() + ready_timeout;
    let mut sleep = Duration::from_millis(50);
    loop {
        let snapshot = fetch_runtime_snapshot(node, node_did).await?;
        let readiness = fetch_node_readiness_snapshot(node, node_did).await?;
        if let Some(snapshot) = &snapshot {
            if snapshot.process_state == "ready"
                && snapshot.reconcile_phase == "idle"
                && snapshot.active_generation >= 1
                && readiness.as_ref().is_some_and(|readiness| {
                    readiness.process_state.accepts_work()
                        && readiness.agents.iter().any(|agent| {
                            agent.state
                                == gents_protocol::node_readiness::AgentReadinessState::Ready
                        })
                })
            {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "node did not reach ready state within {ready_timeout:?}; \
                 last runtime snapshot: {snapshot:?}; readiness: {readiness:?}"
            );
        }
        tokio::time::sleep(sleep).await;
        sleep = (sleep * 2).min(Duration::from_millis(250));
    }
}

#[derive(Debug)]
struct RuntimeSnapshot {
    process_state: String,
    reconcile_phase: String,
    active_generation: i64,
}

async fn fetch_runtime_snapshot(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Option<RuntimeSnapshot>> {
    let escaped_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeReadiness(
                filter: {{ node_did: {{ _eq: "{escaped_did}" }} }},
                limit: 1
            ) {{
                node_did
                snapshot_json
                updated_at
            }}
            NodeRuntime(
                filter: {{ node_did: {{ _eq: "{escaped_did}" }} }},
                limit: 1
            ) {{
                node_did
                reconcile_phase
                last_reconcile_result
                last_reconcile_error
            }}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "runtime snapshot").await?;
    let Some(diagnostic) =
        optional_row::<gents_protocol::row::NodeRuntimeRow>(&response, "NodeRuntime")?
    else {
        return Ok(None);
    };
    let Some(readiness_row) = optional_row::<gents_protocol::node_readiness::NodeReadinessRow>(
        &response,
        "NodeReadiness",
    )?
    else {
        return Ok(None);
    };
    let Some(readiness) =
        gents_protocol::node_readiness::decode_node_readiness_snapshot(&readiness_row, node_did)
            .ok()
    else {
        return Ok(None);
    };
    Ok(Some(RuntimeSnapshot {
        process_state: readiness.process_state.as_str().to_string(),
        reconcile_phase: diagnostic.reconcile_phase.unwrap_or_default(),
        active_generation: i64::try_from(readiness.active_generation).unwrap_or(i64::MAX),
    }))
}

async fn fetch_node_readiness_snapshot(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Option<gents_protocol::node_readiness::NodeReadinessSnapshot>> {
    let escaped_did = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            NodeReadiness(
                filter: {{ node_did: {{ _eq: "{escaped_did}" }} }},
                limit: 1
            ) {{
                snapshot_json
            }}
        }}"#
    );
    let response = graphql_with_transaction_retry(node, &query, "agent readiness snapshot").await?;
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("NodeReadiness"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("snapshot_json"))
        .and_then(serde_json::Value::as_str)
        .and_then(|json| serde_json::from_str(json).ok()))
}

fn optional_row<T>(response: &QueryResponse, key: &str) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    if response.has_errors() {
        bail!("{key} query failed: {:?}", response.errors);
    }
    let Some(value) = response
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
    else {
        return Ok(None);
    };
    Ok(Some(
        serde_json::from_value(value.clone()).with_context(|| format!("decode {key}"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_temp_home_has_a_node_an_identity_and_runtime_schemas() {
        let home = EmbeddedHome::create_temp("eval-home").await.unwrap();
        assert!(home.did().starts_with("did:"));
        assert!(home.path().join("node.key").exists());
        assert!(home.backend_admission.validate(&home.node).await.is_ok());
        let response = home.node.execute("query { AgentRequest { _docID } }").await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }

    #[tokio::test]
    async fn client_access_requires_the_held_evaluator_even_after_reopen() {
        let mut home = EmbeddedHome::create_trial_temp("eval-held-client")
            .await
            .unwrap();
        let evaluator =
            Arc::new(KeyIdentity::load_or_create(home.path().join("evaluator.key"), None).unwrap());
        let evaluator_did = evaluator.did().to_owned();
        assert!(home.client_access(&evaluator_did).is_err());
        let capability = evaluator.client_signing_capability().unwrap();
        home.set_evaluator(evaluator, capability);
        assert!(home.client_access(home.did()).is_err());
        assert_eq!(
            home.client_access(&evaluator_did)
                .unwrap()
                .actor_did()
                .as_deref(),
            Some(evaluator_did.as_str())
        );
        let previous_admission = home.backend_admission.clone();
        home.reopen().await.unwrap();
        assert!(home.backend_admission.validate(&home.node).await.is_ok());
        assert!(previous_admission.validate(&home.node).await.is_err());
        assert_eq!(
            home.client_access(&evaluator_did)
                .unwrap()
                .actor_did()
                .as_deref(),
            Some(evaluator_did.as_str())
        );
        let other =
            Arc::new(KeyIdentity::load_or_create(home.path().join("other.key"), None).unwrap());
        let wrong_capability = other.client_signing_capability().unwrap();
        let evaluator =
            Arc::new(KeyIdentity::load_existing(home.path().join("evaluator.key"), None).unwrap());
        home.set_evaluator(evaluator, wrong_capability);
        assert!(home.client_access(&evaluator_did).is_err());
        home.node.shutdown().await.unwrap();
    }
}
