//! Embedded trial homes: one DefraDB data directory, one identity, one runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::defra_node::{EmbeddedNode, P2PConfig, QueryResponse};
use crate::graphql::escape_graphql_string;
use crate::{ensure_runtime_schemas, AgentIdentity, DocumentRuntimeOptions, Gents, KeyIdentity};

// A full conformance run starts many embedded DefraDB nodes in parallel. On a
// busy CI host, a healthy runtime can spend more than 60 seconds waiting for
// startup and recovery I/O before it publishes its ready status row.
const RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// Node-owned services can hold `EmbeddedNode` handles briefly after shutdown.
const NODE_RELEASE_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds a home's P2P configuration from its data directory, so a reopen
/// restores the same persisted key and collections.
pub type P2PConfigForPath = Arc<dyn Fn(&Path) -> P2PConfig + Send + Sync>;

pub struct EmbeddedHome {
    pub node: Arc<EmbeddedNode>,
    pub identity: Arc<dyn AgentIdentity>,
    did: String,
    path: PathBuf,
    p2p: Option<P2PConfigForPath>,
    /// Owns the directory for temporary homes so it lives as long as the home.
    #[allow(dead_code)]
    tempdir: Option<TempDir>,
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

    /// Open a home in a caller-owned temporary directory; the home owns it.
    pub async fn in_tempdir(tempdir: TempDir, p2p: Option<P2PConfigForPath>) -> Result<Self> {
        let mut home = Self::open_at(tempdir.path().to_path_buf(), p2p).await?;
        home.tempdir = Some(tempdir);
        Ok(home)
    }

    pub async fn create_retained(dir: &Path) -> Result<Self> {
        ensure!(!dir.exists(), "trial home {} already exists", dir.display());
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        Self::open_at(dir.to_path_buf(), None).await
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
        let identity: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(path.join("node.key"), None).context("node identity")?,
        );
        let did = identity.did().to_string();
        let mut builder = EmbeddedNode::builder()
            .data_path(&path)
            .with_node_identity_did(&did);
        if let Some(p2p) = &p2p {
            builder = builder.with_p2p(p2p(&path));
        }
        let node = Arc::new(builder.build().await.context("embedded node")?);
        ensure_runtime_schemas(&node)
            .await
            .context("runtime schemas")?;
        Ok(Self {
            node,
            identity,
            did,
            path,
            p2p,
            tempdir: None,
        })
    }

    pub fn did(&self) -> &str {
        &self.did
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Rebuild the node the way a crashed process would: the `EmbeddedNode`
    /// is shut down, must then be exclusively held, is dropped, and is opened
    /// again on the same directory, DID and P2P configuration.
    pub async fn reopen(&mut self) -> Result<()> {
        // Stopping the node closes its subscriptions and other node-owned
        // services; wait for their handles to be released before claiming an
        // exclusive durable-store reopen.
        self.node.shutdown().await;
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

        let mut builder = EmbeddedNode::builder()
            .data_path(&self.path)
            .with_node_identity_did(&self.did);
        if let Some(p2p) = &self.p2p {
            builder = builder.with_p2p(p2p(&self.path));
        }
        let reopened = builder.build().await.map_err(|e| {
            anyhow!(
                "simulate_process_crash: reopen durable store at {} failed: {e}",
                self.path.display()
            )
        })?;
        self.node = Arc::new(reopened);

        ensure_runtime_schemas(&self.node)
            .await
            .map_err(|e| anyhow!("simulate_process_crash: ensure schemas: {e}"))?;
        Ok(())
    }
}

pub struct RunningRuntime {
    pub shutdown: watch::Sender<bool>,
    pub handle: JoinHandle<Result<()>>,
    pub agent_did: String,
}

impl RunningRuntime {
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown.send(true);
        self.handle.await.context("runtime task")?
    }
}

pub async fn boot_runtime(
    home: &EmbeddedHome,
    identity: Arc<dyn AgentIdentity>,
    options: DocumentRuntimeOptions,
) -> Result<(RunningRuntime, Gents)> {
    let agent =
        Gents::from_default_behavior_documents(home.node.clone(), identity, options).await?;
    let agent_did = agent.agent_did().to_string();
    let (shutdown, shutdown_rx) = watch::channel(false);
    let handle = tokio::spawn(agent.clone().run(shutdown_rx));
    wait_for_runtime_ready(home.node.as_ref(), &agent_did).await?;
    Ok((
        RunningRuntime {
            shutdown,
            handle,
            agent_did,
        },
        agent,
    ))
}

pub async fn wait_for_runtime_ready(node: &EmbeddedNode, agent_did: &str) -> Result<()> {
    let deadline = tokio::time::Instant::now() + RUNTIME_READY_TIMEOUT;
    let mut sleep = Duration::from_millis(50);
    loop {
        let snapshot = fetch_runtime_snapshot(node, agent_did).await?;
        let readiness = fetch_behavior_readiness_snapshot(node, agent_did).await?;
        if let Some(snapshot) = &snapshot {
            if snapshot.process_state == "ready"
                && snapshot.reconcile_phase == "idle"
                && snapshot.active_generation >= 1
                && readiness.as_ref().is_some_and(|readiness| {
                    readiness.process_state.accepts_work()
                        && readiness.behaviors.iter().any(|behavior| {
                            behavior.state == gents_protocol::row::BehaviorReadinessState::Ready
                        })
                })
            {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "agent did not reach ready state within {RUNTIME_READY_TIMEOUT:?}; \
                 last runtime snapshot: {snapshot:?}; readiness: {readiness:?}"
            );
        }
        tokio::time::sleep(sleep).await;
        sleep = (sleep * 2).min(Duration::from_millis(250));
    }
}

/// Fields the readiness condition reads from the runtime and behavior-readiness rows.
#[derive(Debug)]
struct RuntimeSnapshot {
    process_state: String,
    reconcile_phase: String,
    active_generation: i64,
}

async fn fetch_runtime_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<RuntimeSnapshot>> {
    let escaped_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentBehaviorReadiness(
                filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }},
                limit: 1
            ) {{
                agent_did
                snapshot_json
                updated_at
            }}
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }},
                limit: 1
            ) {{
                agent_did
                reconcile_phase
                last_reconcile_result
                last_reconcile_error
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    let Some(diagnostic) =
        optional_row::<gents_protocol::row::AgentRuntimeRow>(&response, "AgentRuntime")?
    else {
        return Ok(None);
    };
    let Some(readiness_row) = optional_row::<gents_protocol::row::AgentBehaviorReadinessRow>(
        &response,
        "AgentBehaviorReadiness",
    )?
    else {
        return Ok(None);
    };
    let Some(readiness) =
        gents_protocol::row::decode_behavior_readiness_snapshot(&readiness_row, agent_did).ok()
    else {
        return Ok(None);
    };
    Ok(Some(RuntimeSnapshot {
        process_state: readiness.process_state.as_str().to_string(),
        reconcile_phase: diagnostic.reconcile_phase.unwrap_or_default(),
        active_generation: i64::try_from(readiness.active_generation).unwrap_or(i64::MAX),
    }))
}

async fn fetch_behavior_readiness_snapshot(
    node: &EmbeddedNode,
    agent_did: &str,
) -> Result<Option<gents_protocol::row::BehaviorReadinessSnapshot>> {
    let escaped_did = escape_graphql_string(agent_did);
    let query = format!(
        r#"{{
            AgentBehaviorReadiness(
                filter: {{ agent_did: {{ _eq: "{escaped_did}" }} }},
                limit: 1
            ) {{
                snapshot_json
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        bail!("behavior readiness query failed: {:?}", response.errors);
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentBehaviorReadiness"))
        .and_then(serde_json::Value::as_array)
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("snapshot_json"))
        .and_then(serde_json::Value::as_str)
        .and_then(|json| serde_json::from_str(json).ok()))
}

fn optional_row<T: DeserializeOwned>(response: &QueryResponse, key: &str) -> Result<Option<T>> {
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
        // ensure_runtime_schemas ran: AgentRequest is queryable.
        let response = home.node.execute("query { AgentRequest { _docID } }").await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }

    #[tokio::test]
    async fn a_retained_home_survives_reopen_and_keeps_its_did() {
        let dir = tempfile::tempdir().unwrap();
        let retained = dir.path().join("trial-home");
        let mut home = EmbeddedHome::create_retained(&retained).await.unwrap();
        let did = home.did().to_string();
        home.reopen().await.unwrap();
        assert_eq!(home.did(), did);
        drop(home);
        let reopened = EmbeddedHome::open_retained(&retained).await.unwrap();
        assert_eq!(reopened.did(), did);
    }

    #[tokio::test]
    async fn create_retained_refuses_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        let error = EmbeddedHome::create_retained(dir.path()).await.unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
    }
}
