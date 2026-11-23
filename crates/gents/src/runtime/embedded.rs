mod client_access;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::de::DeserializeOwned;
use tempfile::TempDir;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::defra_node::{Node as EmbeddedNode, P2PConfig, QueryResponse};
use crate::{DocumentRuntimeOptions, Gents};
use gents_store::local::home::{
    HomeServing, NodeConfigure, NodeProjection, ServedHome, ServedNode,
};
use gents_store::{identity::KeyIdentity, identity::NodeIdentity, schema::ensure_runtime_schemas};

/// Startup and shutdown bound for an embedded runtime. A full conformance run
/// starts many embedded DefraDB nodes in parallel, and on a busy host a healthy
/// runtime can spend more than 60 seconds on startup and recovery I/O before it
/// publishes its ready status row. Expiry fails the boot clearly; this is a host
/// mechanism bound, not configuration.
pub const RUNTIME_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// Builds a home's P2P configuration from its data directory, so a reopen
/// restores the same persisted key and collections.
pub type P2PConfigForPath = Arc<dyn Fn(&Path) -> P2PConfig + Send + Sync>;

/// Trial homes are harness scratch. Their file-custodied store key and data
/// live together; the key binds the fresh-store provenance, not host secrecy.
pub struct EmbeddedHome {
    home: ServedHome,
    evaluator: Option<(
        Arc<dyn NodeIdentity>,
        gents_store::config_client::ClientSigningCapability,
    )>,
}

impl std::fmt::Debug for EmbeddedHome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.home.fmt(f)
    }
}

impl std::ops::Deref for EmbeddedHome {
    type Target = ServedHome;

    fn deref(&self) -> &ServedHome {
        &self.home
    }
}

impl EmbeddedHome {
    pub async fn create_temp(prefix: &str) -> Result<Self> {
        let tempdir = tempfile::Builder::new()
            .prefix(&format!("gents-{prefix}-"))
            .tempdir()?;
        Self::in_tempdir(tempdir, None).await
    }

    pub async fn in_tempdir(tempdir: TempDir, p2p: Option<P2PConfigForPath>) -> Result<Self> {
        let mut home = Self::open_at(tempdir.path().to_path_buf(), p2p).await?;
        home.home.hold_tempdir(tempdir);
        Ok(home)
    }

    pub async fn create_retained(dir: &Path) -> Result<Self> {
        Self::create_retained_with_p2p(dir, None).await
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
        let (projection, _plugins) = trial_projection(&path).await?;
        let home = ServedHome::open_at(path, trial_serving(p2p), projection).await?;
        ensure_runtime_schemas(&home.node)
            .await
            .context("runtime schemas")?;
        Ok(Self {
            home,
            evaluator: None,
        })
    }

    pub async fn open_for_inspection(dir: &Path, expected_did: &str) -> Result<Self> {
        ensure!(
            dir.is_dir(),
            "retained trial home {} is missing",
            dir.display()
        );
        gents_store::local::storage_backend::require_existing_store(&dir.join("data"))?;
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_existing(dir.join("node.key"), None)
                .context("loading the retained trial identity")?,
        );
        ensure!(
            identity.did() == expected_did,
            "retained trial identity {} does not match recorded DID {expected_did}",
            identity.did()
        );
        let (projection, _plugins) = trial_projection(dir).await?;
        let home = ServedHome::open_with_identity(
            dir.to_path_buf(),
            identity,
            false,
            trial_serving(None),
            projection,
        )
        .await?;
        Ok(Self {
            home,
            evaluator: None,
        })
    }

    pub fn set_evaluator(
        &mut self,
        identity: Arc<dyn NodeIdentity>,
        capability: gents_store::config_client::ClientSigningCapability,
    ) {
        self.evaluator = Some((identity, capability));
    }

    pub fn evaluator(&self, did: &str) -> Result<&dyn NodeIdentity> {
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

    pub fn client_access(&self, did: &str) -> Result<gents_store::config_client::ConfigAccess> {
        self.evaluator(did)?;
        let (_, capability) = self
            .evaluator
            .as_ref()
            .context("evaluation requires the held client signing capability")?;
        Ok(gents_store::config_client::ConfigAccess::Graphql(
            crate::runtime::lifecycle_hooks::compose_endpoint(
                gents_store::config_client::GraphqlEndpoint::as_principal(
                    self.graphql_endpoint(),
                    did,
                )
                .with_client_signing_capability(capability.clone())?
                .with_client_signed_agent_creates(),
            )?,
        ))
    }

    pub async fn reopen(&mut self) -> Result<()> {
        let (projection, _plugins) = trial_projection(self.home.path()).await?;
        self.home.reopen(projection).await?;
        ensure_runtime_schemas(&self.home.node)
            .await
            .context("runtime schemas after reopen")?;
        Ok(())
    }
}

/// A trial node projects the plugins installed in its home with the
/// meta-only tool ceiling; the plugin store stays share-locked until the node
/// has started.
async fn trial_projection(
    home: &Path,
) -> Result<(NodeProjection, gents_pack::plugin::store::StoreGuard)> {
    let (plugins, guard) = crate::runtime::node_start::installed_plugins(home).await?;
    Ok((
        NodeProjection {
            tool_policy: Arc::new(gents_tools::surface::ToolCeiling::meta_only()),
            plugins,
            composition: gents_automation::lifecycle_hooks::composition(),
        },
        guard,
    ))
}

/// A trial endpoint serves the runtime's client routes beside the home's
/// own: goal control, inference settings and client access.
fn trial_serving(p2p: Option<P2PConfigForPath>) -> HomeServing {
    HomeServing {
        routes: Some(Arc::new(|served: ServedNode| {
            goal_control_routes(served.clone())
                .merge(trial_inference_settings_routes(served.clone()))
                .merge(client_access::routes(served))
        })),
        configure: p2p.map(|p2p| -> NodeConfigure {
            Arc::new(
                move |path: &Path, builder: crate::defra_node::NodeBuilder| {
                    builder.with_p2p(p2p(path))
                },
            )
        }),
    }
}

fn trial_inference_settings_routes(served: ServedNode) -> axum::Router {
    axum::Router::new().route(
        gents_observe::client_inference::INFERENCE_SETTINGS_PATH,
        axum::routing::get(move |identity: defra_http::ExtractIdentity, headers: axum::http::HeaderMap| {
            let served = served.clone();
            async move {
                use axum::response::IntoResponse;
                let Some((node, runtime)) = served.get().and_then(|(node, runtime)| Some((node.upgrade()?, runtime))) else {
                    return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                let Some(actor) = identity.did().cloned() else {
                    return axum::http::StatusCode::UNAUTHORIZED.into_response();
                };
                let authorization = match gents_store::config_client::client_authentication::ClientAuthorization::from_http(&identity, &headers) {
                    Ok(authorization) => authorization,
                    Err(_) => return axum::http::StatusCode::UNAUTHORIZED.into_response(),
                };
                let access = match gents_store::config_client::ConfigAccess::local_as(node, actor).with_client_authorization(authorization) {
                    Ok(access) => access,
                    Err(_) => return axum::http::StatusCode::FORBIDDEN.into_response(),
                };
                match gents_observe::client_inference::load_inference_setup_state(&access, runtime.did()).await {
                    Ok(settings) => axum::Json(gents_observe::client_inference::InferenceSettingsResponse {
                        contract_version: gents_observe::client_inference::INFERENCE_SETTINGS_CONTRACT_VERSION,
                        state: settings.for_client_observation(),
                    }).into_response(),
                    Err(_) => axum::http::StatusCode::FORBIDDEN.into_response(),
                }
            }
        }),
    )
}

/// The runtime's Agent routes a client reaches: identity provisioning and
/// admission facts from the home owner, and goal control.
pub fn agent_identity_routes(served: ServedNode) -> axum::Router {
    gents_store::local::home::agent_routes(served.clone()).merge(goal_control_routes(served))
}

fn goal_control_routes(served: ServedNode) -> axum::Router {
    axum::Router::new().route(
        gents_protocol::goal_control::APPLY_GOAL_CONTROL_PATH,
        axum::routing::post(
            move |identity: defra_http::ExtractIdentity,
                  headers: axum::http::HeaderMap,
                  axum::Json(request): axum::Json<
                gents_protocol::goal_control::ApplyGoalControl,
            >| {
                let served = served.clone();
                async move {
                    use axum::response::IntoResponse;
                    let Some((node, runtime)) = served
                        .get()
                        .and_then(|(node, runtime)| Some((node.upgrade()?, runtime)))
                    else {
                        return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
                    };
                    gents_automation::goal::client_application::apply_http(
                        &node, runtime, &identity, &headers, request,
                    )
                    .await
                }
            },
        ),
    )
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
pub async fn boot_runtime_within(
    home: &EmbeddedHome,
    identity: Arc<dyn NodeIdentity>,
    mut options: DocumentRuntimeOptions,
    ready_timeout: Duration,
) -> Result<(RunningRuntime, Gents)> {
    options.backend_admission = Some(home.backend_admission());
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
    let query = {
        use gents_protocol::document_query::{
            fields::{node_readiness, node_runtime},
            Filter, Query, QuerySet, Selection,
        };
        let readiness = Query::select(
            Selection::new()
                .field(node_readiness::NODE_DID)
                .field(node_readiness::SNAPSHOT_JSON)
                .field(node_readiness::UPDATED_AT),
        )
        .filter(Filter::eq(node_readiness::NODE_DID, node_did))
        .limit(1);
        let runtime = Query::select(
            Selection::new()
                .field(node_runtime::NODE_DID)
                .field(node_runtime::RECONCILE_PHASE)
                .field(node_runtime::LAST_RECONCILE_RESULT)
                .field(node_runtime::LAST_RECONCILE_ERROR),
        )
        .filter(Filter::eq(node_runtime::NODE_DID, node_did))
        .limit(1);
        QuerySet::new().plain(&readiness).plain(&runtime).render()
    };
    let response = gents_store::graphql::read_rendered(node, &query, "runtime snapshot").await?;
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

pub async fn fetch_node_readiness_snapshot(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Option<gents_protocol::node_readiness::NodeReadinessSnapshot>> {
    let query = {
        use gents_protocol::document_query::{fields::node_readiness, Filter, Query, Selection};
        Query::select(Selection::new().field(node_readiness::SNAPSHOT_JSON))
            .filter(Filter::eq(node_readiness::NODE_DID, node_did))
            .limit(1)
            .render()
    };
    let response =
        gents_store::graphql::read_rendered(node, &query, "agent readiness snapshot").await?;
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
        assert!(home.backend_admission().validate(&home.node).await.is_ok());
        let response = home.node.execute("query { AgentRequest { _docID } }").await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
    }

    #[tokio::test]
    async fn client_access_requires_the_held_evaluator_even_after_reopen() {
        let mut home = EmbeddedHome::create_temp("eval-held-client").await.unwrap();
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
        let previous_admission = home.backend_admission();
        home.reopen().await.unwrap();
        assert!(home.backend_admission().validate(&home.node).await.is_ok());
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

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
mod node_release_tests;

#[cfg(test)]
mod deferred_enrollment_tests;

#[cfg(test)]
mod agent_provisioning_tests;
