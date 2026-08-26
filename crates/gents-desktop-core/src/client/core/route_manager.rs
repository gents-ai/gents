//! Single owner for directional desktop-to-runtime route lifecycle.

use std::sync::{Arc, RwLock as StdRwLock};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use defra_p2p_adapter::P2POperations as P2POps;
use gents::agent::p2p_reconcile::{
    client_route_id, desired_route_is_applied, reconcile_peer_tick,
    teardown_owned_replicators_at_endpoint, ClientRouteIdentity, EmbeddedRemoteP2pAdmin,
    GraphqlPairingStateStore, PairingDesired, PairingDirection, PairingStateStore, RemoteP2pAdmin,
    RemoteP2pAdminError,
};
use gents_protocol::graphql::escape_graphql_string;
use p2p::iroh::parse_public_peer_addr;
use tokio::sync::{Mutex, MutexGuard, RwLock};
use tokio::time::{sleep, Instant};

use super::super::peer_directory::{PeerDirectory, PeerRecord};
use super::super::principal_identity::PrincipalIdentity;
use super::super::schema::subscribed_collection_names;
use super::bearer_pairing::saved_peer_replicator_collections;
use super::bearer_pairing::{current_local_endpoint, is_bearer_peer, publish_local_endpoint};
use super::bootstrap::{connect_peer_with_retry_until, is_connected_peer};
use super::p2p_ops::{p2p_disconnect_peer, p2p_remove_replicator};
use super::{
    ClientPeerStatus, ClientRouteStatus, PairingCollectionStatus, BOOTSTRAP_OPERATION_BACKOFF,
    P2P_OPERATION_TIMEOUT, PEER_ADD_OPERATION_TIMEOUT,
};
use crate::remote_admin::{classify_remote_admin_error, HttpRemoteP2pAdmin};

pub(super) struct ClientRouteManager {
    node: Arc<EmbeddedNode>,
    p2p: Arc<dyn P2POps>,
    actor: Arc<PrincipalIdentity>,
    /// The one serialization boundary for desktop route intent and effects.
    /// Directory generations, transport changes, desired/applied documents,
    /// remote return routes, and status publication must move under this gate.
    lifecycle: Mutex<()>,
}

pub(super) struct ClientRouteLifecycle<'a> {
    manager: &'a ClientRouteManager,
    _guard: MutexGuard<'a, ()>,
}

pub(super) struct RouteActivation {
    pub(super) record: PeerRecord,
    pub(super) connected: bool,
    pub(super) warning: Option<String>,
}

pub(super) struct RouteRemoval {
    pub(super) record: PeerRecord,
    pub(super) cleanup_error: Option<anyhow::Error>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PendingRemovalCleanup {
    Completed,
    Superseded,
}

const UNREADY_ROUTE_RETRY_INTERVAL: Duration = Duration::from_secs(5);

impl ClientRouteManager {
    pub(super) fn reconcile_due(
        pairing_ready: bool,
        last_run_elapsed: Option<Duration>,
        force: bool,
    ) -> bool {
        let interval = if pairing_ready {
            gents::agent::p2p_reconcile::PAIRING_SWEEP_INTERVAL
        } else {
            UNREADY_ROUTE_RETRY_INTERVAL
        };
        force || last_run_elapsed.is_none_or(|elapsed| elapsed >= interval)
    }

    pub(super) fn new(
        node: Arc<EmbeddedNode>,
        p2p: Arc<dyn P2POps>,
        actor: Arc<PrincipalIdentity>,
    ) -> Self {
        Self {
            node,
            p2p,
            actor,
            lifecycle: Mutex::new(()),
        }
    }

    pub(super) async fn lock(&self) -> ClientRouteLifecycle<'_> {
        ClientRouteLifecycle {
            manager: self,
            _guard: self.lifecycle.lock().await,
        }
    }

    pub(super) async fn activate_peer(
        &self,
        peer_directory: &Arc<RwLock<PeerDirectory>>,
        label: &str,
        addr: &str,
        agent_did: &str,
        graphql: Option<&str>,
        default_behavior_id: Option<&str>,
    ) -> Result<RouteActivation> {
        let lifecycle = self.lock().await;
        let record = peer_directory
            .write()
            .await
            .upsert_saved_peer_with_graphql(label, addr, agent_did, graphql, default_behavior_id)
            .await?;
        let mut warning = None;
        let connected = match connect_peer_with_retry_until(
            &self.p2p,
            &record.addr,
            &record.label,
            PEER_ADD_OPERATION_TIMEOUT,
        )
        .await
        {
            Ok(()) => true,
            Err(error) => {
                append_warning(
                    &mut warning,
                    format!("deployment saved but dial failed: {error}"),
                );
                false
            }
        };
        if let Err(error) = lifecycle.configure(&record).await {
            let prefix = if connected {
                "deployment connected"
            } else {
                "deployment saved"
            };
            append_warning(
                &mut warning,
                format!("{prefix} but reverse pairing failed: {error}"),
            );
        }
        Ok(RouteActivation {
            record,
            connected,
            warning,
        })
    }

    pub(super) async fn publish_status_if_current(
        &self,
        peer_directory: &Arc<RwLock<PeerDirectory>>,
        peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
        status: ClientPeerStatus,
    ) -> bool {
        let _lifecycle = self.lock().await;
        let current = peer_directory
            .read()
            .await
            .records()
            .iter()
            .any(|record| record.peer_id == status.peer_id && record.addr == status.addr);
        if !current {
            return false;
        }
        let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
        if let Some(existing) = statuses
            .iter_mut()
            .find(|existing| existing.peer_id == status.peer_id)
        {
            *existing = status;
        } else {
            statuses.push(status);
        }
        true
    }

    pub(super) async fn remove_peer(
        &self,
        peer_directory: &Arc<RwLock<PeerDirectory>>,
        peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
        peer_id: &str,
    ) -> Result<RouteRemoval> {
        let lifecycle = self.lock().await;
        let record = peer_directory
            .read()
            .await
            .records()
            .iter()
            .find(|record| record.peer_id == peer_id)
            .cloned()
            .with_context(|| format!("peer {peer_id} not found"))?;
        if record.source.as_deref() == Some("local-standard") {
            anyhow::bail!("the local runtime deployment cannot be removed");
        }
        let removed = peer_directory
            .write()
            .await
            .queue_removal(peer_id)
            .await
            .with_context(|| format!("queueing peer {peer_id} for durable route teardown"))?
            .with_context(|| format!("peer {peer_id} not found while queueing removal"))?;
        remove_peer_status(peer_statuses, &removed.peer_id);
        let cleanup_error = lifecycle
            .cleanup_pending_removal_if_current(peer_directory, &record)
            .await
            .err();
        Ok(RouteRemoval {
            record: removed,
            cleanup_error,
        })
    }

    pub(super) async fn retry_pending_removal(
        &self,
        peer_directory: &Arc<RwLock<PeerDirectory>>,
        peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
        record: &PeerRecord,
    ) -> Result<PendingRemovalCleanup> {
        let lifecycle = self.lock().await;
        remove_peer_status(peer_statuses, &record.peer_id);
        lifecycle
            .cleanup_pending_removal_if_current(peer_directory, record)
            .await
    }

    async fn configure(&self, record: &PeerRecord) -> Result<()> {
        let local = publish_local_endpoint(&self.node, &self.p2p, self.actor.as_ref())
            .await
            .context("resolving requester P2P endpoint for reciprocal pairing")?;
        let outbound = ClientRouteIdentity::new(
            &record.peer_id,
            &record.addr,
            self.actor.did(),
            &record.agent_did,
        )?;
        let inbound = ClientRouteIdentity::new(
            &record.peer_id,
            local.address,
            self.actor.did(),
            &record.agent_did,
        )?;

        self.upsert_desired(&outbound, PairingDirection::ClientToRuntime)
            .await?;
        self.upsert_desired(&inbound, PairingDirection::RuntimeToClient)
            .await
    }

    async fn refresh_endpoint(
        &self,
        peer_directory: &Arc<RwLock<PeerDirectory>>,
        saved: &PeerRecord,
    ) -> PeerRecord {
        if is_bearer_peer(saved) {
            return saved.clone();
        }
        let current = match gents::agent::p2p_reconcile::TransportEndpoint::parse(
            saved.addr.clone(),
        ) {
            Ok(current) => current,
            Err(error) => {
                tracing::warn!(
                    directory_peer_id = %saved.peer_id,
                    error = %error,
                    "saved deployment endpoint is not dialable; waiting for public add_peer repair"
                );
                return saved.clone();
            }
        };
        let endpoint = if let Some(graphql) = management_endpoint(saved) {
            match HttpRemoteP2pAdmin::new_with_actor(graphql, Arc::clone(&self.actor)) {
                Ok(admin) => admin
                    .peer_info()
                    .await
                    .ok()
                    .and_then(|addresses| replacement_endpoint(&current, addresses)),
                Err(_) => None,
            }
        } else if saved.source.is_none() {
            // Legacy rows have no authoritative server-status endpoint. A
            // signed, freshness-checked PeerEndpoint is their upgrade path.
            let identity: Arc<dyn gents::AgentIdentity> = self.actor.clone();
            let store = gents::agent::p2p_reconcile::GraphqlReciprocalStore::new(
                Arc::clone(&self.node),
                identity,
            );
            store
                .load_verified_endpoint_for_did(&saved.agent_did)
                .await
                .ok()
                .flatten()
                .and_then(|endpoint| replacement_endpoint(&current, [endpoint.address]))
        } else {
            None
        };
        let Some(endpoint) = endpoint else {
            return saved.clone();
        };
        let mut updated = saved.clone();
        updated.addr = endpoint;
        updated.pairing_ready = false;
        updated.updated_at = chrono::Utc::now().to_rfc3339();
        if let Err(error) = peer_directory.write().await.upsert(updated.clone()).await {
            tracing::warn!(
                target: "gents_desktop_core::pairing_reconcile",
                directory_peer_id = %saved.peer_id,
                error = %error,
                "failed to persist rotated saved deployment endpoint"
            );
            return saved.clone();
        }
        if let Err(error) = self.configure(&updated).await {
            tracing::warn!(
                target: "gents_desktop_core::pairing_reconcile",
                directory_peer_id = %saved.peer_id,
                error = %error,
                "failed to apply rotated saved deployment endpoint"
            );
        }
        updated
    }

    /// Repair both directional routes and return a durable readiness update.
    /// `None` preserves the last known value across transient admin/store errors.
    async fn reconcile(
        &self,
        record: &PeerRecord,
        peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    ) -> Option<bool> {
        let store = self.pairing_store();
        let outbound = self
            .reconcile_route(
                &EmbeddedRemoteP2pAdmin::new(Arc::clone(&self.node)),
                &store,
                PairingDirection::ClientToRuntime,
                record,
                peer_statuses,
            )
            .await;
        let inbound = match management_endpoint(record) {
            Some(graphql) => {
                match HttpRemoteP2pAdmin::new_with_actor(graphql, Arc::clone(&self.actor)) {
                    Ok(admin) => {
                        self.reconcile_route(
                            &admin.with_local_resolver(Arc::clone(&self.node)),
                            &store,
                            PairingDirection::RuntimeToClient,
                            record,
                            peer_statuses,
                        )
                        .await
                    }
                    Err(error) => {
                        self.publish_unavailable_route(
                            &store,
                            PairingDirection::RuntimeToClient,
                            record,
                            peer_statuses,
                            format!("invalid remote GraphQL endpoint: {error}"),
                            true,
                        )
                        .await
                    }
                }
            }
            None => {
                self.publish_unavailable_route(
                    &store,
                    PairingDirection::RuntimeToClient,
                    record,
                    peer_statuses,
                    "remote GraphQL endpoint is unavailable".to_string(),
                    true,
                )
                .await
            }
        };
        let readiness = combined_route_readiness(outbound, inbound);
        if readiness == Some(true) {
            if let Err(error) = self.cleanup_legacy(record).await {
                tracing::warn!(
                    target: "gents_desktop_core::pairing_reconcile",
                    directory_peer_id = %record.peer_id,
                    error = %error,
                    "client route is ready but legacy route cleanup will retry"
                );
            }
        }
        publish_chat_safety(record, peer_statuses, readiness);
        readiness
    }

    async fn reconcile_route(
        &self,
        admin: &dyn RemoteP2pAdmin,
        store: &GraphqlPairingStateStore,
        direction: PairingDirection,
        record: &PeerRecord,
        peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    ) -> RouteReconcileState {
        let route_id = client_route_id(&record.peer_id, direction);
        let (desired, desired_exists) = match store.load_desired(&route_id).await {
            Ok(Some(desired)) => (desired, true),
            Ok(None) => (PairingDesired::default(), false),
            Err(error) => {
                publish_route_status(
                    record,
                    peer_statuses,
                    route_status(record, direction, None, false, false, false),
                    Some(format!("load desired route failed: {error}")),
                    None,
                );
                return RouteReconcileState::Unavailable;
            }
        };
        let live_route_matches = match reconcile_peer_tick(admin, store, &route_id).await {
            Ok(outcome) if !outcome.desired_read_failed => outcome.live_route_matches,
            Ok(_) => {
                publish_route_status(
                    record,
                    peer_statuses,
                    route_status(
                        record,
                        direction,
                        Some(&desired),
                        desired_exists,
                        false,
                        false,
                    ),
                    Some("desired route read was unavailable".to_string()),
                    None,
                );
                return RouteReconcileState::Unavailable;
            }
            Err(error) => {
                let error_class = error
                    .downcast_ref::<RemoteP2pAdminError>()
                    .map(classify_remote_admin_error);
                if let Some(remote_error) = error.downcast_ref::<RemoteP2pAdminError>() {
                    record_failure(record, peer_statuses, &desired, remote_error);
                }
                record_pairing_error(record, peer_statuses, &error);
                publish_route_status(
                    record,
                    peer_statuses,
                    route_status(
                        record,
                        direction,
                        Some(&desired),
                        desired_exists,
                        false,
                        false,
                    ),
                    Some(format!("client route reconcile failed: {error:#}")),
                    error_class,
                );
                tracing::warn!(
                    target: "gents_desktop_core::pairing_reconcile",
                    directory_peer_id = %record.peer_id,
                    label = %record.label,
                    error = %format_args!("{error:#}"),
                    "client route reconcile tick failed"
                );
                return RouteReconcileState::Unavailable;
            }
        };
        match store.load_applied(&route_id).await {
            Ok(applied)
                if live_route_matches && desired_route_is_applied(&desired, &applied.state) =>
            {
                record_route_success(record, peer_statuses);
                publish_route_status(
                    record,
                    peer_statuses,
                    route_status(
                        record,
                        direction,
                        Some(&desired),
                        desired_exists,
                        true,
                        true,
                    ),
                    None,
                    None,
                );
                RouteReconcileState::Ready
            }
            Ok(applied) => {
                let applied_matches = desired_route_is_applied(&desired, &applied.state);
                publish_route_status(
                    record,
                    peer_statuses,
                    route_status(
                        record,
                        direction,
                        Some(&desired),
                        desired_exists,
                        applied_matches,
                        live_route_matches,
                    ),
                    Some("confirmed route drift; repair scheduled".to_string()),
                    None,
                );
                RouteReconcileState::Drifted
            }
            Err(error) => {
                publish_route_status(
                    record,
                    peer_statuses,
                    route_status(
                        record,
                        direction,
                        Some(&desired),
                        desired_exists,
                        false,
                        live_route_matches,
                    ),
                    Some(format!("load applied route failed: {error}")),
                    None,
                );
                RouteReconcileState::Unavailable
            }
        }
    }

    async fn publish_unavailable_route(
        &self,
        store: &GraphqlPairingStateStore,
        direction: PairingDirection,
        record: &PeerRecord,
        peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
        error: String,
        confirmed_drift: bool,
    ) -> RouteReconcileState {
        let route_id = client_route_id(&record.peer_id, direction);
        let desired = store.load_desired(&route_id).await.ok().flatten();
        let applied = match (&desired, store.load_applied(&route_id).await.ok()) {
            (Some(desired), Some(applied)) => desired_route_is_applied(desired, &applied.state),
            _ => false,
        };
        publish_route_status(
            record,
            peer_statuses,
            route_status(
                record,
                direction,
                desired.as_ref(),
                desired.is_some(),
                applied,
                false,
            ),
            Some(error),
            None,
        );
        if confirmed_drift {
            RouteReconcileState::Drifted
        } else {
            RouteReconcileState::Unavailable
        }
    }

    async fn upsert_desired(
        &self,
        route: &ClientRouteIdentity,
        direction: PairingDirection,
    ) -> Result<()> {
        use gents::agent::p2p_reconcile::templates::CLIENT_TEMPLATE;

        let peer_id = escape_graphql_string(&route.desired_id(direction));
        let agent_did = escape_graphql_string(&route.owner_agent_did);
        let address = escape_graphql_string(route.transport.address());
        let now = escape_graphql_string(
            &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
        let response = self
            .node
            .execute(&format!(
                r#"mutation {{ upsert_PeerPairingDesired(
                    filter: {{ peer_id: {{ _eq: "{peer_id}" }} }},
                    add: {{ peer_id: "{peer_id}", agent_did: "{agent_did}", collections: null,
                        template: "{CLIENT_TEMPLATE}", replicator_addresses: ["{address}"],
                        profiles: null, created_at: "{now}", updated_at: "{now}" }},
                    update: {{ agent_did: "{agent_did}", collections: null,
                        template: "{CLIENT_TEMPLATE}", replicator_addresses: ["{address}"],
                        profiles: null, updated_at: "{now}" }}
                ) {{ _docID }} }}"#
            ))
            .await;
        ensure_graphql_ok(&response, "write PeerPairingDesired")
    }

    async fn teardown_remote(&self, record: &PeerRecord) -> Result<()> {
        let route_id = client_route_id(&record.peer_id, PairingDirection::RuntimeToClient);
        let store = self.pairing_store();
        let mut addresses = store
            .load_desired(&route_id)
            .await?
            .unwrap_or_default()
            .replicator_addresses;
        if let Ok(applied) = store.load_applied(&route_id).await {
            addresses.extend(applied.state.replicator_addresses);
        }
        // Desired state can be missing after a partial local write. The live
        // desktop transport endpoint is still the authoritative target of the
        // remote return route and lets removal prove absence before committing.
        addresses.insert(
            current_local_endpoint(&self.p2p, self.actor.as_ref())
                .await?
                .address,
        );
        let graphql = record
            .graphql
            .as_deref()
            .context("remote GraphQL endpoint is required to teardown the return route")?;
        let admin = HttpRemoteP2pAdmin::new_with_actor(graphql, Arc::clone(&self.actor))?;
        for address in &addresses {
            teardown_owned_replicators_at_endpoint(&admin, address).await?;
        }
        Ok(())
    }

    async fn delete_local_state(&self, directory_id: &str) -> Result<bool> {
        let ids = [
            directory_id.to_string(),
            client_route_id(directory_id, PairingDirection::ClientToRuntime),
            client_route_id(directory_id, PairingDirection::RuntimeToClient),
        ]
        .map(|id| format!(r#""{}""#, escape_graphql_string(&id)))
        .join(", ");
        let response = self
            .node
            .execute(&format!(
                r#"mutation {{
                    desired: delete_PeerPairingDesired(filter: {{ peer_id: {{ _in: [{ids}] }} }}) {{ _docID }}
                    applied: delete_PeerPairingApplied(filter: {{ peer_id: {{ _in: [{ids}] }} }}) {{ _docID }}
                }}"#
            ))
            .await;
        ensure_graphql_ok(&response, "delete client pairing state")?;
        Ok(response
            .data
            .as_ref()
            .and_then(|data| data.get("desired"))
            .and_then(|rows| rows.as_array())
            .is_some_and(|rows| !rows.is_empty()))
    }

    async fn cleanup_legacy(&self, record: &PeerRecord) -> Result<()> {
        // Iroh authorizes one replicator per transport peer. Installing each
        // directional route replaces that peer's old broad collections and
        // filter atomically; deleting guessed collections here would instead
        // mutate the newly installed route.
        self.delete_legacy_state(&record.peer_id).await
    }

    async fn delete_legacy_state(&self, directory_id: &str) -> Result<()> {
        let id = escape_graphql_string(directory_id);
        let response = self
            .node
            .execute(&format!(
                r#"mutation {{
                    delete_PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{id}" }} }}) {{ _docID }}
                    delete_PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{id}" }} }}) {{ _docID }}
                }}"#
            ))
            .await;
        ensure_graphql_ok(&response, "delete legacy client pairing state")
    }

    fn pairing_store(&self) -> GraphqlPairingStateStore {
        GraphqlPairingStateStore::new(Arc::clone(&self.node), self.actor.clone())
    }
}

impl ClientRouteLifecycle<'_> {
    pub(super) async fn configure(&self, record: &PeerRecord) -> Result<()> {
        self.manager.configure(record).await
    }

    pub(super) async fn refresh_endpoint(
        &self,
        peer_directory: &Arc<RwLock<PeerDirectory>>,
        saved: &PeerRecord,
    ) -> PeerRecord {
        self.manager.refresh_endpoint(peer_directory, saved).await
    }

    pub(super) async fn reconcile(
        &self,
        record: &PeerRecord,
        peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    ) -> Option<bool> {
        self.manager.reconcile(record, peer_statuses).await
    }

    pub(super) async fn teardown_remote(&self, record: &PeerRecord) -> Result<()> {
        self.manager.teardown_remote(record).await
    }

    pub(super) async fn delete_local_state(&self, directory_id: &str) -> Result<bool> {
        self.manager.delete_local_state(directory_id).await
    }

    async fn cleanup_pending_removal_if_current(
        &self,
        peer_directory: &Arc<RwLock<PeerDirectory>>,
        record: &PeerRecord,
    ) -> Result<PendingRemovalCleanup> {
        if !peer_directory.read().await.has_pending_removal(record) {
            return Ok(PendingRemovalCleanup::Superseded);
        }
        let has_shared_transport_owner = peer_directory
            .read()
            .await
            .records()
            .iter()
            .any(|active| same_transport_peer(active, record));

        let cleanup = async {
            if !has_shared_transport_owner {
                let local_result = cleanup_saved_peer_p2p(&self.manager.p2p, record).await;
                let remote_result = if requires_managed_remote_teardown(record) {
                    self.teardown_remote(record).await
                } else {
                    Ok(())
                };
                if local_result.is_err() || remote_result.is_err() {
                    let mut failures = Vec::new();
                    if let Err(error) = local_result {
                        failures.push(format!("local transport cleanup: {error}"));
                    }
                    if let Err(error) = remote_result {
                        failures.push(format!("remote return-route cleanup: {error}"));
                    }
                    anyhow::bail!("{}", failures.join("; "));
                }
            }
            self.delete_local_state(&record.peer_id).await?;
            Result::<()>::Ok(())
        };
        tokio::time::timeout(P2P_OPERATION_TIMEOUT, cleanup)
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "cleanup exceeded {}s timeout",
                    P2P_OPERATION_TIMEOUT.as_secs()
                )
            })??;

        if !peer_directory
            .write()
            .await
            .complete_removal_if_matches(record)
            .await?
        {
            anyhow::bail!(
                "pending removal intent changed while route lifecycle owned {}",
                record.peer_id
            );
        }
        Ok(PendingRemovalCleanup::Completed)
    }

    #[cfg(test)]
    pub(super) async fn upsert_desired(
        &self,
        route: &ClientRouteIdentity,
        direction: PairingDirection,
    ) -> Result<()> {
        self.manager.upsert_desired(route, direction).await
    }
}

pub(super) async fn cleanup_saved_peer_p2p(
    p2p: &Arc<dyn P2POps>,
    record: &PeerRecord,
) -> Result<()> {
    let mut replicator_errors = Vec::new();
    if let Err(error) =
        p2p_remove_replicator(p2p, saved_peer_replicator_collections(record), &record.addr).await
    {
        replicator_errors.push(error.to_string());
    }
    if !record.is_bearer_pairing() {
        if let Err(error) = p2p_remove_replicator(
            p2p,
            subscribed_collection_names()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            &record.addr,
        )
        .await
        {
            replicator_errors.push(error.to_string());
        }
    }
    let replicator_result = if replicator_errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(replicator_errors.join("; ")))
    };
    let disconnect_result = async {
        p2p_disconnect_peer(p2p, &record.addr).await?;
        let Some(expected_peer_id) = parse_public_peer_addr(&record.addr)
            .ok()
            .map(|(peer_id, _)| peer_id.to_string())
        else {
            return Ok(());
        };
        let deadline = Instant::now() + PEER_ADD_OPERATION_TIMEOUT;
        loop {
            if !is_connected_peer(p2p, &expected_peer_id).await? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for peer {expected_peer_id} to disconnect");
            }
            sleep(BOOTSTRAP_OPERATION_BACKOFF).await;
        }
    }
    .await;
    match (replicator_result, disconnect_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(replicator_error), Ok(())) => anyhow::bail!(
            "transport disconnected but replicator cleanup failed for {} at {}: {}",
            record.label,
            record.addr,
            replicator_error
        ),
        (Ok(()), Err(disconnect_error)) => anyhow::bail!(
            "replicator removed but transport disconnect failed for {} at {}: {}",
            record.label,
            record.addr,
            disconnect_error
        ),
        (Err(replicator_error), Err(disconnect_error)) => anyhow::bail!(
            "replicator cleanup failed for {} at {}: {}; transport disconnect also failed: {}",
            record.label,
            record.addr,
            replicator_error,
            disconnect_error
        ),
    }
}

fn same_transport_peer(left: &PeerRecord, right: &PeerRecord) -> bool {
    match (
        gents::agent::p2p_reconcile::TransportEndpoint::parse(left.addr.clone()),
        gents::agent::p2p_reconcile::TransportEndpoint::parse(right.addr.clone()),
    ) {
        (Ok(left), Ok(right)) => left.peer_id() == right.peer_id(),
        _ => left.addr == right.addr,
    }
}

fn requires_managed_remote_teardown(record: &PeerRecord) -> bool {
    !record.is_bearer_pairing()
        && record
            .graphql
            .as_deref()
            .is_some_and(|endpoint| !endpoint.trim().is_empty())
}

fn remove_peer_status(peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>, peer_id: &str) {
    peer_statuses
        .write()
        .expect("peer status lock poisoned")
        .retain(|status| status.peer_id != peer_id);
}

fn append_warning(warning: &mut Option<String>, message: String) {
    match warning {
        Some(existing) => {
            existing.push_str("; ");
            existing.push_str(&message);
        }
        None => *warning = Some(message),
    }
}

#[derive(Clone, Copy)]
enum RouteReconcileState {
    Ready,
    Drifted,
    Unavailable,
}

fn combined_route_readiness(
    outbound: RouteReconcileState,
    inbound: RouteReconcileState,
) -> Option<bool> {
    match (outbound, inbound) {
        (RouteReconcileState::Ready, RouteReconcileState::Ready) => Some(true),
        (RouteReconcileState::Drifted, _) | (_, RouteReconcileState::Drifted) => Some(false),
        _ => None,
    }
}

fn management_endpoint(record: &PeerRecord) -> Option<&str> {
    record
        .graphql
        .as_deref()
        .map(str::trim)
        .filter(|endpoint| !endpoint.is_empty())
}

fn replacement_endpoint(
    current: &gents::agent::p2p_reconcile::TransportEndpoint,
    addresses: impl IntoIterator<Item = String>,
) -> Option<String> {
    let authoritative = addresses
        .into_iter()
        .filter_map(|address| {
            let endpoint =
                gents::agent::p2p_reconcile::TransportEndpoint::parse(address.clone()).ok()?;
            (endpoint.peer_id() == current.peer_id()).then_some((address, endpoint))
        })
        // `/p2p/info` may contain both a canonical shareable ticket and an
        // equivalent listen-address spelling. Pick one authoritative
        // candidate before comparing it with persisted state; otherwise an
        // old equivalent spelling can mask a newly rotated ticket.
        .max_by_key(|(address, endpoint)| {
            (
                address.starts_with("endpoint"),
                endpoint.dial_address_count(),
                address.clone(),
            )
        });
    authoritative
        .and_then(|(address, endpoint)| (!endpoint.equivalent_to(current)).then_some(address))
}

fn route_status(
    record: &PeerRecord,
    direction: PairingDirection,
    desired: Option<&PairingDesired>,
    desired_exists: bool,
    applied: bool,
    live_match: bool,
) -> ClientRouteStatus {
    let address = desired
        .and_then(|desired| desired.replicator_addresses.iter().next())
        .cloned();
    let transport_peer_id = address.as_ref().and_then(|address| {
        gents::agent::p2p_reconcile::TransportEndpoint::parse(address.clone())
            .ok()
            .map(|endpoint| endpoint.peer_id().to_string())
    });
    let filter_summary = desired.map_or_else(
        || "no route filter".to_string(),
        |desired| {
            let collections = desired
                .effective_replicator_collections()
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            format!(
                "{} collections; {} scoped filters [{}]",
                collections.len(),
                desired.replicator_filter.len(),
                collections.join(",")
            )
        },
    );
    ClientRouteStatus {
        route_id: client_route_id(&record.peer_id, direction),
        direction: direction.as_str().to_string(),
        directory_id: record.peer_id.clone(),
        transport_peer_id,
        address,
        template: desired.and_then(|desired| desired.template_ids.iter().next().cloned()),
        desired: desired_exists,
        applied,
        live_match,
        filter_summary,
        last_error: None,
        retry_count: 0,
        last_retry_at: None,
        last_retry_error_class: None,
    }
}

fn publish_route_status(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    mut route: ClientRouteStatus,
    error: Option<String>,
    error_class: Option<crate::remote_admin::PairingErrorClass>,
) {
    let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
    let Some(status) = statuses
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    else {
        return;
    };
    let previous = status
        .routes
        .iter()
        .find(|existing| existing.direction == route.direction);
    if let Some(error) = error {
        route.last_error = Some(error);
        route.retry_count = previous.map_or(1, |previous| previous.retry_count.saturating_add(1));
        route.last_retry_at = Some(SystemTime::now());
        route.last_retry_error_class =
            error_class.or_else(|| previous.and_then(|previous| previous.last_retry_error_class));
    }
    if let Some(index) = status
        .routes
        .iter()
        .position(|existing| existing.direction == route.direction)
    {
        status.routes[index] = route;
    } else {
        status.routes.push(route);
        status
            .routes
            .sort_by(|left, right| left.direction.cmp(&right.direction));
    }
}

fn publish_chat_safety(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    readiness: Option<bool>,
) {
    if let Some(status) = peer_statuses
        .write()
        .expect("peer status lock poisoned")
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    {
        status.chat_safe = readiness.unwrap_or(record.pairing_ready);
        if readiness == Some(true) {
            status.last_error = None;
        }
    }
}

fn record_pairing_error(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    error: &anyhow::Error,
) {
    if let Some(status) = peer_statuses
        .write()
        .expect("peer status lock poisoned")
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    {
        status.last_error = Some(format!("client route reconcile failed: {error:#}"));
    }
}

fn record_failure(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
    desired: &PairingDesired,
    error: &RemoteP2pAdminError,
) {
    let class = classify_remote_admin_error(error);
    let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
    let Some(status) = statuses
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    else {
        return;
    };
    for collection in desired.effective_replicator_collections() {
        let status = ensure_pairing_status(status, collection);
        status.record_retry(class);
        status.update_stuck_indicator(SystemTime::now());
    }
}

fn record_route_success(
    record: &PeerRecord,
    peer_statuses: &Arc<StdRwLock<Vec<ClientPeerStatus>>>,
) {
    let mut statuses = peer_statuses.write().expect("peer status lock poisoned");
    let Some(status) = statuses
        .iter_mut()
        .find(|status| status.peer_id == record.peer_id)
    else {
        return;
    };
    for collection in &mut status.pairing {
        collection.record_success();
    }
}

fn ensure_pairing_status<'a>(
    status: &'a mut ClientPeerStatus,
    collection: &str,
) -> &'a mut PairingCollectionStatus {
    if let Some(index) = status
        .pairing
        .iter()
        .position(|existing| existing.collection_id == collection)
    {
        &mut status.pairing[index]
    } else {
        status
            .pairing
            .push(PairingCollectionStatus::new(collection));
        status.pairing.last_mut().expect("pairing status inserted")
    }
}

fn ensure_graphql_ok(response: &defra_node::QueryResponse, operation: &str) -> Result<()> {
    if response.has_errors() {
        anyhow::bail!(
            "{operation} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_failure_preserves_last_known_readiness() {
        assert_eq!(
            combined_route_readiness(RouteReconcileState::Ready, RouteReconcileState::Unavailable),
            None
        );
        assert_eq!(
            combined_route_readiness(RouteReconcileState::Unavailable, RouteReconcileState::Ready),
            None
        );
        assert_eq!(
            combined_route_readiness(
                RouteReconcileState::Unavailable,
                RouteReconcileState::Drifted
            ),
            Some(false)
        );
    }

    #[test]
    fn missing_graphql_endpoint_cannot_be_ready() {
        let record = PeerRecord::new(
            "Mandrake",
            "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
            "did:key:mandrake",
        );
        assert_eq!(management_endpoint(&record), None);
        assert_eq!(
            combined_route_readiness(RouteReconcileState::Ready, RouteReconcileState::Drifted),
            Some(false)
        );
    }

    #[test]
    fn healthy_routes_use_slow_reconcile_cadence() {
        assert!(!ClientRouteManager::reconcile_due(
            true,
            Some(Duration::from_secs(2)),
            false
        ));
        assert!(ClientRouteManager::reconcile_due(
            true,
            Some(gents::agent::p2p_reconcile::PAIRING_SWEEP_INTERVAL),
            false
        ));
        assert!(ClientRouteManager::reconcile_due(
            false,
            Some(UNREADY_ROUTE_RETRY_INTERVAL),
            false
        ));
        assert!(!ClientRouteManager::reconcile_due(
            false,
            Some(Duration::from_secs(2)),
            false
        ));
    }

    #[test]
    fn endpoint_refresh_prefers_rotated_shareable_ticket_before_equivalence_check() {
        let old = "endpointacmyt7vq36usszcwsyes6mscehvncwfnfr7xrtdjvkmzvnyfsnqvuaibab7qaaab337ag";
        let new = "endpointacmyt7vq36usszcwsyes6mscehvncwfnfr7xrtdjvkmzvnyfsnqvuaibab7qaaabyteqg";
        let current = gents::agent::p2p_reconcile::TransportEndpoint::parse(old).unwrap();

        assert_eq!(
            replacement_endpoint(&current, [old.to_string(), new.to_string()]),
            Some(new.to_string())
        );
        let refreshed = gents::agent::p2p_reconcile::TransportEndpoint::parse(new).unwrap();
        assert_eq!(
            replacement_endpoint(&refreshed, [old.to_string(), new.to_string()]),
            None,
            "the canonical current ticket must not oscillate back to an older spelling"
        );
    }

    #[test]
    fn route_diagnostics_preserve_identity_filter_error_and_retry() {
        let record = PeerRecord::new(
            "Mandrake",
            "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
            "did:key:mandrake",
        );
        let route = ClientRouteIdentity::new(
            &record.peer_id,
            &record.addr,
            "did:key:phone",
            &record.agent_did,
        )
        .unwrap();
        let desired = route.desired(PairingDirection::ClientToRuntime);
        let statuses = Arc::new(StdRwLock::new(vec![ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: false,
        }]));
        record_failure(
            &record,
            &statuses,
            &desired,
            &RemoteP2pAdminError::RpcTimeout,
        );
        record_pairing_error(&record, &statuses, &anyhow::anyhow!("filter mismatch"));

        let status = statuses.read().unwrap()[0].clone();
        assert!(route
            .desired_id(PairingDirection::ClientToRuntime)
            .ends_with(":client-to-runtime"));
        assert_eq!(route.directory_id, record.peer_id);
        assert_eq!(
            route.transport.peer_id(),
            "6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb"
        );
        assert!(desired.replicator_filter.contains_key("AgentRequest"));
        assert!(status.last_error.unwrap().contains("filter mismatch"));
        assert!(status
            .pairing
            .iter()
            .all(|entry| entry.pairing_retry_count == 1));
    }

    #[test]
    fn route_owner_publishes_both_directional_statuses_and_chat_safety() {
        let record = PeerRecord::new(
            "Mandrake",
            "127.0.0.1:56000/p2p/6fe391e1c69d66de633034ca40cda6d39ca1a3c94792f2f510add7d1421ea7bb",
            "did:key:mandrake",
        );
        let statuses = Arc::new(StdRwLock::new(vec![ClientPeerStatus {
            peer_id: record.peer_id.clone(),
            label: record.label.clone(),
            agent_did: record.agent_did.clone(),
            addr: record.addr.clone(),
            dial_succeeded: true,
            last_error: None,
            pairing: Vec::new(),
            routes: Vec::new(),
            chat_safe: false,
        }]));
        let route = ClientRouteIdentity::new(
            &record.peer_id,
            &record.addr,
            "did:key:phone",
            &record.agent_did,
        )
        .unwrap();
        for direction in [
            PairingDirection::ClientToRuntime,
            PairingDirection::RuntimeToClient,
        ] {
            let desired = route.desired(direction);
            publish_route_status(
                &record,
                &statuses,
                route_status(&record, direction, Some(&desired), true, true, true),
                None,
                None,
            );
        }
        publish_chat_safety(&record, &statuses, Some(true));

        let status = statuses.read().unwrap()[0].clone();
        assert!(status.chat_safe);
        assert_eq!(status.routes.len(), 2);
        assert_eq!(status.routes[0].direction, "client-to-runtime");
        assert_eq!(status.routes[1].direction, "runtime-to-client");
        assert!(status.routes.iter().all(|route| {
            route.directory_id == record.peer_id
                && route.template.as_deref() == Some("client")
                && route.desired
                && route.applied
                && route.live_match
                && route.transport_peer_id.is_some()
                && route.address.is_some()
                && route.filter_summary.contains("scoped filters")
                && route.last_error.is_none()
                && route.retry_count == 0
        }));
    }
}
