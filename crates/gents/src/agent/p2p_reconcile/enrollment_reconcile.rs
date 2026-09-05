//! Single I/O owner for authenticated enrollment authority and its effects.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::identity::AgentIdentity;

use super::enrollment_store::{EnrollmentProjection, GraphqlEnrollmentStore};
use super::graphql_helpers::{ensure_no_errors, graphql_string_list_literal, rows};
use super::templates::{resolve_template, CLIENT_TEMPLATE};
use super::{
    desired_route_is_applied, observe_owned_pairing_live_matches, EmbeddedRemoteP2pAdmin,
    GraphqlPairingStateStore, PairingStateStore,
};

const SOURCE_ENROLLMENT: &str = "enrollment";

#[derive(Debug, Clone, PartialEq, Eq)]
enum EnrollmentAuthorityState {
    Pending,
    Ready(Arc<EnrollmentProjection>),
    Failed(Arc<str>),
}

#[derive(Clone)]
pub struct EnrollmentAuthorityHandle {
    receiver: watch::Receiver<EnrollmentAuthorityState>,
    commands: mpsc::Sender<EnrollmentAuthorityCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentAuthorizationFence {
    pub network_id: String,
    pub request_id: String,
    pub admin_did: String,
    pub member_did: String,
    pub member_peer: String,
    pub member_ticket: String,
    pub owner_agent: String,
    pub request_digest: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
}

enum EnrollmentAuthorityCommand {
    FreshAuthorization {
        member_did: String,
        member_peer: String,
        ack: oneshot::Sender<Result<Option<EnrollmentAuthorizationFence>>>,
    },
    FreshMemberAdmission {
        member_did: String,
        ack: oneshot::Sender<Result<bool>>,
    },
    FreshPeerAuthorization {
        member_peer: String,
        ack: oneshot::Sender<Result<Option<EnrollmentAuthorizationFence>>>,
    },
    FreshMemberAuthorization {
        member_did: String,
        ack: oneshot::Sender<Result<Option<EnrollmentAuthorizationFence>>>,
    },
}

#[async_trait]
trait EnrollmentProjectionReader: Send + Sync {
    async fn load_authority_projection(&self) -> Result<EnrollmentProjection>;
}

#[async_trait]
impl EnrollmentProjectionReader for GraphqlEnrollmentStore {
    async fn load_authority_projection(&self) -> Result<EnrollmentProjection> {
        self.load_projection().await
    }
}

#[async_trait]
pub trait PeerAdmissionAuthority: Send + Sync {
    async fn fresh_member_authorized(&self, member_did: &str) -> Result<bool>;

    async fn fresh_member_authorized_for_agent(
        &self,
        member_did: &str,
        owner_agent: &str,
    ) -> Result<bool>;
}

impl EnrollmentAuthorityHandle {
    /// Read the last complete projection. Pending and failed reads are
    /// intentionally errors so pairing and hydration remain fail closed.
    pub fn current(&self) -> Result<Arc<EnrollmentProjection>> {
        match self.receiver.borrow().clone() {
            EnrollmentAuthorityState::Ready(projection) => Ok(projection),
            EnrollmentAuthorityState::Pending => {
                anyhow::bail!("enrollment authority has not completed its first projection")
            }
            EnrollmentAuthorityState::Failed(error) => {
                anyhow::bail!("enrollment authority projection failed: {error}")
            }
        }
    }

    /// Ask the single durable authority owner to reload the document set and
    /// return the exact current authorization generation for one member.
    pub async fn fresh_authorization(
        &self,
        member_did: &str,
        member_peer: &str,
    ) -> Result<Option<EnrollmentAuthorizationFence>> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(EnrollmentAuthorityCommand::FreshAuthorization {
                member_did: member_did.to_string(),
                member_peer: member_peer.to_string(),
                ack,
            })
            .await
            .context("enrollment authority owner stopped")?;
        result
            .await
            .context("enrollment authority owner dropped fresh authorization response")?
    }

    /// Reload authority through the sole owner and resolve one transport peer
    /// to its exact current generation. Transport reconciliation has the peer
    /// identity before it has a trusted member DID, so this lookup must derive
    /// the DID from the signed authority projection rather than a desired row.
    pub async fn fresh_peer_authorization(
        &self,
        member_peer: &str,
    ) -> Result<Option<EnrollmentAuthorizationFence>> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(EnrollmentAuthorityCommand::FreshPeerAuthorization {
                member_peer: member_peer.to_string(),
                ack,
            })
            .await
            .context("enrollment authority owner stopped")?;
        result
            .await
            .context("enrollment authority owner dropped fresh peer authorization response")?
    }

    /// Reload through the sole owner and resolve an exact current generation
    /// for a signed request principal. Duplicate active identities fail closed.
    pub async fn fresh_member_authorization(
        &self,
        member_did: &str,
    ) -> Result<Option<EnrollmentAuthorizationFence>> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(EnrollmentAuthorityCommand::FreshMemberAuthorization {
                member_did: member_did.to_string(),
                ack,
            })
            .await
            .context("enrollment authority owner stopped")?;
        result
            .await
            .context("enrollment authority owner dropped fresh member response")?
    }
}

#[async_trait]
impl PeerAdmissionAuthority for EnrollmentAuthorityHandle {
    async fn fresh_member_authorized(&self, member_did: &str) -> Result<bool> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(EnrollmentAuthorityCommand::FreshMemberAdmission {
                member_did: member_did.to_string(),
                ack,
            })
            .await
            .context("enrollment authority owner stopped")?;
        result
            .await
            .context("enrollment authority owner dropped peer admission response")?
    }

    async fn fresh_member_authorized_for_agent(
        &self,
        member_did: &str,
        owner_agent: &str,
    ) -> Result<bool> {
        Ok(self
            .fresh_member_authorization(member_did)
            .await?
            .is_some_and(|authorization| authorization.owner_agent == owner_agent))
    }
}

pub struct EnrollmentAuthorityOwner {
    sender: watch::Sender<EnrollmentAuthorityState>,
    commands: mpsc::Receiver<EnrollmentAuthorityCommand>,
}

pub fn enrollment_authority_channel() -> (EnrollmentAuthorityOwner, EnrollmentAuthorityHandle) {
    let (sender, receiver) = watch::channel(EnrollmentAuthorityState::Pending);
    let (commands, command_receiver) = mpsc::channel(32);
    (
        EnrollmentAuthorityOwner {
            sender,
            commands: command_receiver,
        },
        EnrollmentAuthorityHandle { receiver, commands },
    )
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestEnrollmentAuthority {
    current: Arc<tokio::sync::RwLock<Option<EnrollmentAuthorizationFence>>>,
}

#[cfg(test)]
impl TestEnrollmentAuthority {
    pub(crate) async fn replace(&self, value: Option<EnrollmentAuthorizationFence>) {
        *self.current.write().await = value;
    }
}

#[cfg(test)]
pub(crate) fn test_enrollment_authority(
    initial: Option<EnrollmentAuthorizationFence>,
) -> (TestEnrollmentAuthority, EnrollmentAuthorityHandle) {
    let (sender, receiver) = watch::channel(EnrollmentAuthorityState::Ready(Arc::new(
        EnrollmentProjection::default(),
    )));
    let (commands, mut command_receiver) = mpsc::channel(32);
    let current = Arc::new(tokio::sync::RwLock::new(initial));
    let task_current = current.clone();
    tokio::spawn(async move {
        while let Some(command) = command_receiver.recv().await {
            let current = task_current.read().await.clone();
            match command {
                EnrollmentAuthorityCommand::FreshMemberAuthorization { member_did, ack } => {
                    let _ = ack.send(Ok(current.filter(|value| value.member_did == member_did)));
                }
                EnrollmentAuthorityCommand::FreshAuthorization {
                    member_did,
                    member_peer,
                    ack,
                } => {
                    let _ = ack.send(Ok(current.filter(|value| {
                        value.member_did == member_did && value.member_peer == member_peer
                    })));
                }
                EnrollmentAuthorityCommand::FreshPeerAuthorization { member_peer, ack } => {
                    let _ = ack.send(Ok(current.filter(|value| value.member_peer == member_peer)));
                }
                EnrollmentAuthorityCommand::FreshMemberAdmission { member_did, ack } => {
                    let _ = ack.send(Ok(
                        current.is_some_and(|value| value.member_did == member_did)
                    ));
                }
            }
        }
        drop(sender);
    });
    (
        TestEnrollmentAuthority { current },
        EnrollmentAuthorityHandle { receiver, commands },
    )
}

impl EnrollmentAuthorityOwner {
    fn publish_ready(&self, projection: EnrollmentProjection) {
        let next = EnrollmentAuthorityState::Ready(Arc::new(projection));
        self.sender.send_if_modified(|current| {
            if *current == next {
                return false;
            }
            *current = next;
            true
        });
    }

    fn publish_failed(&self, error: &anyhow::Error) {
        let next = EnrollmentAuthorityState::Failed(Arc::from(format!("{error:#}")));
        self.sender.send_if_modified(|current| {
            if *current == next {
                return false;
            }
            *current = next;
            true
        });
    }
}

/// Run the sole durable enrollment projection/effect loop.
///
/// Consumers only read [`EnrollmentAuthorityHandle`]. This loop alone queries
/// the full authority set, publishes its immutable projection, owns
/// `source="enrollment"` data-plane rows, and retries exact terminal delivery.
pub async fn run_enrollment_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn AgentIdentity>,
    mut owner: EnrollmentAuthorityOwner,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlEnrollmentStore::new(node.clone(), identity.clone());
    let pairing_admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let mut delivered = BTreeSet::new();
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_enrollment(
        &node,
        &identity,
        &store,
        &pairing_admin,
        &owner,
        &mut delivered,
    )
    .await;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                sweep_enrollment(&node, &identity, &store, &pairing_admin, &owner, &mut delivered).await;
            }
            command = owner.commands.recv() => {
                let Some(command) = command else {
                    continue;
                };
                handle_authority_command(&store, &owner, command).await;
            }
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("enrollment update subscription closed; continuing periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "enrollment update subscription dropped messages");
                }
                sweep_enrollment(&node, &identity, &store, &pairing_admin, &owner, &mut delivered).await;
            }
        }
    }
}

async fn handle_authority_command(
    store: &impl EnrollmentProjectionReader,
    owner: &EnrollmentAuthorityOwner,
    command: EnrollmentAuthorityCommand,
) {
    match command {
        EnrollmentAuthorityCommand::FreshAuthorization {
            member_did,
            member_peer,
            ack,
        } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    owner.publish_ready(projection.clone());
                    exact_authorization_fence(&projection, &member_did, &member_peer)
                }
                Err(error) => {
                    owner.publish_failed(&error);
                    Err(error.context("reload enrollment authority for authorization fence"))
                }
            };
            let _ = ack.send(result);
        }
        EnrollmentAuthorityCommand::FreshMemberAdmission { member_did, ack } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    owner.publish_ready(projection.clone());
                    if projection.conflict.is_some() {
                        Err(anyhow::anyhow!("enrollment root authority is conflicted"))
                    } else {
                        Ok(projection
                            .active
                            .iter()
                            .any(|active| active.request.candidate_did == member_did))
                    }
                }
                Err(error) => {
                    owner.publish_failed(&error);
                    Err(error.context("reload enrollment authority for peer admission"))
                }
            };
            let _ = ack.send(result);
        }
        EnrollmentAuthorityCommand::FreshPeerAuthorization { member_peer, ack } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    owner.publish_ready(projection.clone());
                    exact_peer_authorization_fence(&projection, &member_peer)
                }
                Err(error) => {
                    owner.publish_failed(&error);
                    Err(error.context("reload enrollment authority for transport fence"))
                }
            };
            let _ = ack.send(result);
        }
        EnrollmentAuthorityCommand::FreshMemberAuthorization { member_did, ack } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    owner.publish_ready(projection.clone());
                    exact_member_authorization_fence(&projection, &member_did)
                }
                Err(error) => {
                    owner.publish_failed(&error);
                    Err(error.context("reload enrollment authority for request admission"))
                }
            };
            let _ = ack.send(result);
        }
    }
}

fn exact_authorization_fence(
    projection: &EnrollmentProjection,
    member_did: &str,
    member_peer: &str,
) -> Result<Option<EnrollmentAuthorizationFence>> {
    anyhow::ensure!(
        projection.conflict.is_none(),
        "enrollment root authority is conflicted"
    );
    let matches = projection
        .active
        .iter()
        .filter(|active| {
            active.request.candidate_did == member_did
                && active.request.candidate_peer == member_peer
        })
        .collect::<Vec<_>>();
    let [active] = matches.as_slice() else {
        anyhow::ensure!(
            matches.is_empty(),
            "enrollment authority has multiple active generations for member"
        );
        return Ok(None);
    };
    Ok(Some(EnrollmentAuthorizationFence {
        network_id: active.request.network_id.clone(),
        request_id: active.request.request_id.clone(),
        admin_did: active.request.admin_did.clone(),
        member_did: active.request.candidate_did.clone(),
        member_peer: active.request.candidate_peer.clone(),
        member_ticket: active.request.candidate_ticket.clone(),
        owner_agent: active.request.owner_agent.clone(),
        request_digest: active.request.request_digest.clone(),
        authorization_sequence: active.revision.sequence,
        authorization_expires_at: active.revision.authorization_expires_at.clone(),
    }))
}

fn exact_member_authorization_fence(
    projection: &EnrollmentProjection,
    member_did: &str,
) -> Result<Option<EnrollmentAuthorizationFence>> {
    anyhow::ensure!(
        projection.conflict.is_none(),
        "enrollment root authority is conflicted"
    );
    let matches = projection
        .active
        .iter()
        .filter(|active| active.request.candidate_did == member_did)
        .collect::<Vec<_>>();
    let [active] = matches.as_slice() else {
        anyhow::ensure!(
            matches.is_empty(),
            "enrollment authority has multiple active generations for member DID"
        );
        return Ok(None);
    };
    exact_authorization_fence(
        projection,
        &active.request.candidate_did,
        &active.request.candidate_peer,
    )
}

fn exact_peer_authorization_fence(
    projection: &EnrollmentProjection,
    member_peer: &str,
) -> Result<Option<EnrollmentAuthorizationFence>> {
    anyhow::ensure!(
        projection.conflict.is_none(),
        "enrollment root authority is conflicted"
    );
    let matches = projection
        .active
        .iter()
        .filter(|active| active.request.candidate_peer == member_peer)
        .collect::<Vec<_>>();
    let [active] = matches.as_slice() else {
        anyhow::ensure!(
            matches.is_empty(),
            "enrollment authority has multiple active generations for transport peer"
        );
        return Ok(None);
    };
    exact_authorization_fence(
        projection,
        &active.request.candidate_did,
        &active.request.candidate_peer,
    )
}

async fn sweep_enrollment(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn AgentIdentity>,
    store: &GraphqlEnrollmentStore,
    pairing_admin: &EmbeddedRemoteP2pAdmin,
    owner: &EnrollmentAuthorityOwner,
    delivered: &mut BTreeSet<String>,
) {
    let projection = match store.load_projection().await {
        Ok(projection) => projection,
        Err(error) => {
            owner.publish_failed(&error);
            tracing::warn!(error = %error, "enrollment projection read failed; authority remains fail closed");
            return;
        }
    };
    owner.publish_ready(projection.clone());

    if let Err(error) = reconcile_data_plane(node, identity.did(), store, &projection).await {
        owner.publish_failed(&error);
        tracing::warn!(error = %error, "enrollment data-plane reconciliation failed; will retry");
    }
    if let Err(error) =
        record_applied_route_receipts(node, identity, store, pairing_admin, &projection).await
    {
        tracing::warn!(error = %error, "enrollment route receipt publication failed; will retry");
    }
    retry_terminal_delivery(store, &projection, delivered).await;
}

async fn record_applied_route_receipts(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn AgentIdentity>,
    store: &GraphqlEnrollmentStore,
    pairing_admin: &EmbeddedRemoteP2pAdmin,
    projection: &EnrollmentProjection,
) -> Result<()> {
    for active in &projection.active {
        if active.route_receipt_doc_id.is_some() {
            continue;
        }
        let peer_id = &active.request.candidate_peer;
        let pairing_store = GraphqlPairingStateStore::for_enrollment_materialization(
            node.clone(),
            identity.clone(),
            super::EnrollmentEndpointEntry {
                desired_id: active.request.candidate_peer.clone(),
                peer_id: active.request.candidate_peer.clone(),
                agent_did: active.request.candidate_did.clone(),
                address: active.request.candidate_ticket.clone(),
                request_digest: active.request.request_digest.clone(),
                authorization_sequence: active.revision.sequence,
                authorization_expires_at: active.revision.authorization_expires_at.clone(),
            },
        );
        let Some(desired) = pairing_store.load_desired(peer_id).await? else {
            continue;
        };
        let applied = pairing_store.load_applied(peer_id).await?;
        if !applied.duplicate_doc_ids.is_empty()
            || !desired_route_is_applied(&desired, &applied.state)
            || !observe_owned_pairing_live_matches(pairing_admin, &desired, &applied.state).await?
        {
            continue;
        }
        store.record_applied_route(active, Utc::now()).await?;
    }
    Ok(())
}

async fn retry_terminal_delivery(
    store: &GraphqlEnrollmentStore,
    projection: &EnrollmentProjection,
    delivered: &mut BTreeSet<String>,
) {
    let retained = projection
        .active
        .iter()
        .filter_map(active_delivery_id)
        .chain(
            projection
                .denied
                .iter()
                .map(|denied| denied.decision_doc_id.clone()),
        )
        .chain(
            projection
                .revoked
                .iter()
                .map(|revoked| format!("{}:{}", revoked.decision_doc_id, revoked.revision_doc_id)),
        )
        .collect::<BTreeSet<_>>();
    delivered.retain(|delivery_id| retained.contains(delivery_id));
    for active in &projection.active {
        let Some(receipt_doc_id) = active.route_receipt_doc_id.as_deref() else {
            continue;
        };
        let delivery_id = active_delivery_id(active).expect("receipt was checked above");
        if delivered.contains(&delivery_id) {
            continue;
        }
        if !store
            .deliver_terminal(
                &active.request,
                &active.decision_doc_id,
                Some(&active.revision_doc_id),
                Some(receipt_doc_id),
            )
            .await
        {
            delivered.insert(delivery_id);
        }
    }
    for denied in &projection.denied {
        if delivered.contains(&denied.decision_doc_id) {
            continue;
        }
        if !store
            .deliver_terminal(&denied.request, &denied.decision_doc_id, None, None)
            .await
        {
            delivered.insert(denied.decision_doc_id.clone());
        }
    }
    for revoked in &projection.revoked {
        let delivery_id = format!("{}:{}", revoked.decision_doc_id, revoked.revision_doc_id);
        if delivered.contains(&delivery_id) {
            continue;
        }
        if !store
            .deliver_terminal(
                &revoked.request,
                &revoked.decision_doc_id,
                Some(&revoked.revision_doc_id),
                None,
            )
            .await
        {
            delivered.insert(delivery_id);
        }
    }
}

fn active_delivery_id(active: &super::enrollment_store::ActiveEnrollment) -> Option<String> {
    active.route_receipt_doc_id.as_ref().map(|receipt_doc_id| {
        format!(
            "{}:{}:{}",
            active.decision_doc_id, active.revision_doc_id, receipt_doc_id
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnrollmentRoute {
    peer_id: String,
    member_did: String,
    address: String,
    request_digest: String,
    authorization_sequence: u64,
    authorization_expires_at: String,
}

fn desired_routes(projection: &EnrollmentProjection) -> BTreeMap<String, EnrollmentRoute> {
    desired_routes_at(projection, Utc::now())
}

fn desired_routes_at(
    projection: &EnrollmentProjection,
    now: chrono::DateTime<Utc>,
) -> BTreeMap<String, EnrollmentRoute> {
    projection
        .active
        .iter()
        .filter(|active| {
            gents_protocol::enrollment::authorization_lease_is_fresh_at(
                &active.revision.authorization_expires_at,
                now,
            )
        })
        .map(|active| {
            let route = EnrollmentRoute {
                peer_id: active.request.candidate_peer.clone(),
                member_did: active.request.candidate_did.clone(),
                address: active.request.candidate_ticket.clone(),
                request_digest: active.request.request_digest.clone(),
                authorization_sequence: active.revision.sequence,
                authorization_expires_at: active.revision.authorization_expires_at.clone(),
            };
            (route.peer_id.clone(), route)
        })
        .collect()
}

async fn reconcile_data_plane(
    node: &EmbeddedNode,
    local_did: &str,
    store: &GraphqlEnrollmentStore,
    projection: &EnrollmentProjection,
) -> Result<()> {
    let response = node
        .execute(
            r#"{
                PeerPairingDesired {
                    peer_id agent_did template source collections replicator_addresses
                    enrollment_request_digest enrollment_authorization_sequence
                    enrollment_authorization_expires_at
                }
            }"#,
        )
        .await;
    ensure_no_errors(&response, "query enrollment base-route ownership")?;
    let existing = rows::<EnrollmentRouteRow>(&response, "PeerPairingDesired")?;
    let desired = desired_routes(projection);
    let client = resolve_template(CLIENT_TEMPLATE).context("client template is missing")?;
    let client_collections = client
        .collections
        .iter()
        .map(|collection| collection.to_string())
        .collect::<BTreeSet<_>>();

    let foreign_peers = existing
        .iter()
        .filter(|row| row.source.as_deref() != Some(SOURCE_ENROLLMENT))
        .map(|row| row.peer_id.clone())
        .collect::<BTreeSet<_>>();
    let enrollment_rows = existing
        .into_iter()
        .filter(|row| row.source.as_deref() == Some(SOURCE_ENROLLMENT))
        .map(|row| (row.peer_id.clone(), row))
        .collect::<BTreeMap<_, _>>();

    for (peer_id, route) in &desired {
        if foreign_peers.contains(peer_id) {
            tracing::warn!(
                peer_id,
                "enrollment route is blocked by another base-route owner"
            );
            continue;
        }
        // A durable revoke or superseding generation can commit after the
        // sweep projection was loaded. Reload through the authority store at
        // the write boundary and compare the complete signed generation; a
        // time-only check cannot fence those transitions.
        let fresh_projection = match store.load_projection().await {
            Ok(projection) => projection,
            Err(error) => {
                // Authority read failure is fail closed. Remove every row this
                // owner can identify so a stale materialization cannot keep
                // granting route admission while the projection is unknown.
                for stale_peer_id in enrollment_rows.keys() {
                    delete_enrollment_route(node, stale_peer_id).await?;
                }
                return Err(error.context("reload enrollment authority before route publication"));
            }
        };
        let fresh =
            exact_authorization_fence(&fresh_projection, &route.member_did, &route.peer_id)?;
        if !fresh
            .as_ref()
            .is_some_and(|fence| enrollment_route_matches_fence(route, fence))
        {
            if enrollment_rows.contains_key(peer_id) {
                delete_enrollment_route(node, peer_id).await?;
            }
            continue;
        }
        let current = enrollment_rows.get(peer_id);
        if current
            .is_some_and(|row| row.matches(local_did, route, CLIENT_TEMPLATE, &client_collections))
        {
            continue;
        }
        let mutation = upsert_enrollment_route_mutation(
            route,
            local_did,
            &client_collections,
            &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        );
        crate::graphql::graphql_mutation_with_transaction_retry(
            node,
            &mutation,
            "upsert enrollment base route",
        )
        .await?;
    }
    for peer_id in enrollment_rows.keys() {
        if desired.contains_key(peer_id) {
            continue;
        }
        let mutation = delete_enrollment_route_mutation(peer_id);
        crate::graphql::graphql_mutation_with_transaction_retry(
            node,
            &mutation,
            "retract enrollment base route",
        )
        .await?;
    }
    Ok(())
}

fn enrollment_route_matches_fence(
    route: &EnrollmentRoute,
    fence: &EnrollmentAuthorizationFence,
) -> bool {
    route.peer_id == fence.member_peer
        && route.member_did == fence.member_did
        && route.address == fence.member_ticket
        && route.request_digest == fence.request_digest
        && route.authorization_sequence == fence.authorization_sequence
        && route.authorization_expires_at == fence.authorization_expires_at
}

async fn delete_enrollment_route(node: &EmbeddedNode, peer_id: &str) -> Result<()> {
    let mutation = delete_enrollment_route_mutation(peer_id);
    crate::graphql::graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "retract enrollment base route at authority fence",
    )
    .await
    .map(|_| ())
}

fn upsert_enrollment_route_mutation(
    route: &EnrollmentRoute,
    local_did: &str,
    collections: &BTreeSet<String>,
    now: &str,
) -> String {
    let escape = crate::graphql::escape_graphql_string;
    let peer_id = escape(&route.peer_id);
    let local_did = escape(local_did);
    let address = graphql_string_list_literal([route.address.as_str()]);
    let collections = graphql_string_list_literal(collections.iter().map(String::as_str));
    let now = escape(now);
    let request_digest = escape(&route.request_digest);
    let authorization_expires_at = escape(&route.authorization_expires_at);
    format!(
        r#"mutation {{
            upsert_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "enrollment" }} }},
                add: {{
                    peer_id: "{peer_id}", agent_did: "{local_did}", template: "client",
                    source: "enrollment", collections: {collections},
                    enrollment_request_digest: "{request_digest}",
                    enrollment_authorization_sequence: {},
                    enrollment_authorization_expires_at: "{authorization_expires_at}",
                    replicator_addresses: {address}, created_at: "{now}", updated_at: "{now}"
                }},
                update: {{
                    agent_did: "{local_did}", template: "client", source: "enrollment",
                    enrollment_request_digest: "{request_digest}",
                    enrollment_authorization_sequence: {},
                    enrollment_authorization_expires_at: "{authorization_expires_at}",
                    collections: {collections}, replicator_addresses: {address}, updated_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#,
        route.authorization_sequence, route.authorization_sequence,
    )
}

fn delete_enrollment_route_mutation(peer_id: &str) -> String {
    let peer_id = crate::graphql::escape_graphql_string(peer_id);
    format!(
        r#"mutation {{
            delete_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "enrollment" }} }}
            ) {{ _docID }}
        }}"#
    )
}

#[derive(Debug, Deserialize)]
struct EnrollmentRouteRow {
    peer_id: String,
    agent_did: Option<String>,
    template: Option<String>,
    source: Option<String>,
    collections: Option<Vec<String>>,
    replicator_addresses: Option<Vec<String>>,
    enrollment_request_digest: Option<String>,
    enrollment_authorization_sequence: Option<i64>,
    enrollment_authorization_expires_at: Option<String>,
}

impl EnrollmentRouteRow {
    fn matches(
        &self,
        local_did: &str,
        route: &EnrollmentRoute,
        template: &str,
        collections: &BTreeSet<String>,
    ) -> bool {
        self.agent_did.as_deref() == Some(local_did)
            && self.template.as_deref() == Some(template)
            && self.source.as_deref() == Some(SOURCE_ENROLLMENT)
            && self.enrollment_request_digest.as_deref() == Some(&route.request_digest)
            && self.enrollment_authorization_sequence == Some(route.authorization_sequence as i64)
            && self.enrollment_authorization_expires_at.as_deref()
                == Some(&route.authorization_expires_at)
            && self
                .collections
                .as_ref()
                .map(|values| values.iter().cloned().collect::<BTreeSet<_>>())
                .as_ref()
                == Some(collections)
            && self.replicator_addresses.as_deref() == Some(&[route.address.clone()])
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use gents_protocol::enrollment::{
        AuthorizationRevisionKind, AuthorizationRevisionRecord, EnrollmentDecisionKind,
        EnrollmentDecisionRecord, EnrollmentRequestRecord, ENROLLMENT_PROTOCOL_VERSION,
    };
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, AgentRequestCreate};
    use gents_protocol::request_lifecycle::RequestLifecycleState;

    use super::*;
    use crate::agent::p2p_reconcile::enrollment_store::{ActiveEnrollment, RevokedEnrollment};
    use crate::identity::KeyIdentity;
    use crate::request_admission::AgentRequestAdmissionVerifier;

    enum ScriptedProjection {
        Projection(EnrollmentProjection),
        Error(&'static str),
    }

    struct ScriptedProjectionReader {
        steps: Mutex<VecDeque<ScriptedProjection>>,
    }

    #[async_trait]
    impl EnrollmentProjectionReader for ScriptedProjectionReader {
        async fn load_authority_projection(&self) -> Result<EnrollmentProjection> {
            match self
                .steps
                .lock()
                .expect("scripted projection lock")
                .pop_front()
                .expect("scripted projection step")
            {
                ScriptedProjection::Projection(projection) => Ok(projection),
                ScriptedProjection::Error(message) => anyhow::bail!(message),
            }
        }
    }

    fn active_enrollment(sequence: u64, expires_at: &str) -> ActiveEnrollment {
        let request_id = format!("enrollment-request-{sequence}");
        let request_digest = format!("enrollment-digest-{sequence}");
        let request = EnrollmentRequestRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            request_id: request_id.clone(),
            request_digest: request_digest.clone(),
            offer_id: format!("offer-{sequence}"),
            offer_token: format!("token-{sequence}"),
            challenge: format!("challenge-{sequence}"),
            network_id: "network-1".into(),
            admin_did: "did:key:admin".into(),
            server_peer: "server-peer".into(),
            candidate_did: "did:key:member-placeholder".into(),
            candidate_peer: "member-peer".into(),
            candidate_ticket: "member-ticket".into(),
            owner_agent: "did:key:owner-placeholder".into(),
            profile: "client".into(),
            client_nonce: format!("nonce-{sequence}"),
            issued_at: "2030-01-01T00:00:00Z".into(),
            expires_at: "2031-01-01T00:00:00Z".into(),
            candidate_sig: vec![1; 64],
        };
        let decision = EnrollmentDecisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            decision_id: format!("decision-{sequence}"),
            request_id: request_id.clone(),
            request_digest: request_digest.clone(),
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            candidate_did: request.candidate_did.clone(),
            candidate_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            decision: EnrollmentDecisionKind::Approved,
            authorization_sequence: sequence,
            authorization_expires_at: expires_at.into(),
            decided_at: "2030-01-01T00:00:01Z".into(),
            signer_did: request.admin_did.clone(),
            admin_sig: vec![2; 64],
        };
        let revision = AuthorizationRevisionRecord {
            protocol_version: ENROLLMENT_PROTOCOL_VERSION,
            revision_id: format!("revision-{sequence}"),
            request_id,
            request_digest,
            network_id: request.network_id.clone(),
            admin_did: request.admin_did.clone(),
            member_did: request.candidate_did.clone(),
            member_peer: request.candidate_peer.clone(),
            owner_agent: request.owner_agent.clone(),
            sequence,
            authorization_expires_at: expires_at.into(),
            kind: AuthorizationRevisionKind::Active,
            issued_at: "2030-01-01T00:00:01Z".into(),
            signer_did: request.admin_did.clone(),
            admin_sig: vec![3; 64],
        };
        ActiveEnrollment {
            request_doc_id: format!("request-doc-{sequence}"),
            decision_doc_id: format!("decision-doc-{sequence}"),
            revision_doc_id: format!("revision-doc-{sequence}"),
            route_receipt_doc_id: None,
            request,
            decision,
            revision,
            route_receipt: None,
        }
    }

    fn projection_with_active(active: ActiveEnrollment) -> EnrollmentProjection {
        EnrollmentProjection {
            network_id: Some(active.request.network_id.clone()),
            active: vec![active],
            ..EnrollmentProjection::default()
        }
    }

    fn projection_with_revoked(active: &ActiveEnrollment) -> EnrollmentProjection {
        let mut revision = active.revision.clone();
        revision.kind = AuthorizationRevisionKind::Revoked;
        EnrollmentProjection {
            network_id: Some(active.request.network_id.clone()),
            revoked: vec![RevokedEnrollment {
                request_doc_id: active.request_doc_id.clone(),
                decision_doc_id: active.decision_doc_id.clone(),
                revision_doc_id: format!("revoked-{}", active.revision.sequence + 1),
                request: active.request.clone(),
                decision: active.decision.clone(),
                revision,
            }],
            ..EnrollmentProjection::default()
        }
    }

    async fn create_enrollment_request(
        node: Arc<EmbeddedNode>,
        target: &dyn AgentIdentity,
        member: &dyn AgentIdentity,
        active: &ActiveEnrollment,
        label: &str,
    ) -> crate::watcher::AgentRequest {
        let mut create = AgentRequestCreate::base(
            format!("agent-request-{label}"),
            target.did(),
            member.did(),
            "behavior-1",
            format!("session-{label}"),
            format!("content-{label}"),
            "interactive",
            "2030-01-01T00:00:02Z",
            AgentRequestAdmissionRecord::enrollment(
                member.did(),
                &active.request.request_id,
                &active.request.request_digest,
                &active.request.admin_did,
                active.revision.sequence,
                &active.revision.authorization_expires_at,
            ),
        );
        crate::sign_agent_request_create(member, &mut create)
            .await
            .expect("sign enrollment AgentRequest");
        let response = node.execute(&create.graphql_mutation().unwrap()).await;
        assert!(
            !response.has_errors(),
            "create enrollment AgentRequest: {:?}",
            response.errors
        );
        let doc_id = response
            .data
            .as_ref()
            .and_then(|data| {
                data.get("create_AgentRequest")
                    .or_else(|| data.get("add_AgentRequest"))
            })
            .and_then(|value| {
                value.get("_docID").or_else(|| {
                    value
                        .as_array()
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("_docID"))
                })
            })
            .and_then(serde_json::Value::as_str)
            .expect("created AgentRequest doc id");
        crate::request_admission::load_request_for_admission_test(node.as_ref(), doc_id)
            .await
            .expect("load queued AgentRequest")
    }

    #[test]
    fn enrollment_route_mutations_are_owned_bounded_and_nonempty() {
        let route = EnrollmentRoute {
            peer_id: "peer-\"\\".to_string(),
            member_did: "did:key:member".to_string(),
            address: "ticket-1".to_string(),
            request_digest: "digest-1".to_string(),
            authorization_sequence: 1,
            authorization_expires_at: "2026-09-29T12:00:00Z".to_string(),
        };
        let collections = BTreeSet::from(["AgentRequest".to_string()]);
        let upsert = upsert_enrollment_route_mutation(
            &route,
            "did:key:server",
            &collections,
            "2026-08-29T12:00:00Z",
        );
        assert!(upsert.contains("source: \"enrollment\""));
        assert!(upsert.contains("template: \"client\""));
        assert!(upsert.contains("enrollment_request_digest: \"digest-1\""));
        assert!(upsert.contains("enrollment_authorization_sequence: 1"));
        assert!(upsert.contains("enrollment_authorization_expires_at: \"2026-09-29T12:00:00Z\""));
        assert!(upsert.contains("peer-\\\"\\\\"));
        assert!(!upsert.contains("[]"));

        let delete = delete_enrollment_route_mutation(&route.peer_id);
        assert!(delete.contains("source: { _eq: \"enrollment\" }"));
        assert!(delete.contains("peer-\\\"\\\\"));
    }

    #[test]
    fn authority_handle_is_fail_closed_until_owner_publishes() {
        let (owner, handle) = enrollment_authority_channel();
        assert!(handle.current().is_err());
        owner.publish_ready(EnrollmentProjection::default());
        assert_eq!(*handle.current().unwrap(), EnrollmentProjection::default());
    }

    #[tokio::test]
    async fn final_claim_fence_reloads_revocation_expiry_supersession_and_read_failure() {
        let target_dir = tempfile::tempdir().unwrap();
        let member_dir = tempfile::tempdir().unwrap();
        let target: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(target_dir.path().join("target.key"), None).unwrap(),
        );
        let member: Arc<dyn AgentIdentity> = Arc::new(
            KeyIdentity::load_or_create(member_dir.path().join("member.key"), None).unwrap(),
        );
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .unwrap();

        let mut generation_one = active_enrollment(1, "2030-01-01T00:10:00Z");
        generation_one.request.candidate_did = member.did().to_string();
        generation_one.request.owner_agent = target.did().to_string();
        generation_one.decision.candidate_did = member.did().to_string();
        generation_one.decision.owner_agent = target.did().to_string();
        generation_one.revision.member_did = member.did().to_string();
        generation_one.revision.owner_agent = target.did().to_string();
        let mut expired = generation_one.clone();
        expired.request.request_id = "enrollment-request-expired".into();
        expired.request.request_digest = "enrollment-digest-expired".into();
        expired.decision.request_id = expired.request.request_id.clone();
        expired.decision.request_digest = expired.request.request_digest.clone();
        expired.decision.authorization_expires_at = "2030-01-01T00:05:00Z".into();
        expired.revision.request_id = expired.request.request_id.clone();
        expired.revision.request_digest = expired.request.request_digest.clone();
        expired.revision.authorization_expires_at = "2030-01-01T00:05:00Z".into();
        let mut generation_two = active_enrollment(2, "2030-01-01T00:20:00Z");
        generation_two.request.candidate_did = member.did().to_string();
        generation_two.request.owner_agent = target.did().to_string();
        generation_two.decision.candidate_did = member.did().to_string();
        generation_two.decision.owner_agent = target.did().to_string();
        generation_two.revision.member_did = member.did().to_string();
        generation_two.revision.owner_agent = target.did().to_string();

        let revoked_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            &generation_one,
            "revoked",
        )
        .await;
        let expired_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            &expired,
            "expired",
        )
        .await;
        let superseded_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            &generation_one,
            "superseded",
        )
        .await;
        let replacement_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            &generation_two,
            "replacement",
        )
        .await;

        let script = ScriptedProjectionReader {
            steps: Mutex::new(VecDeque::from([
                ScriptedProjection::Projection(projection_with_active(generation_one.clone())),
                ScriptedProjection::Projection(projection_with_revoked(&generation_one)),
                ScriptedProjection::Projection(projection_with_active(expired.clone())),
                ScriptedProjection::Projection(projection_with_active(expired)),
                ScriptedProjection::Projection(projection_with_active(generation_one.clone())),
                ScriptedProjection::Projection(projection_with_active(generation_two.clone())),
                ScriptedProjection::Projection(projection_with_active(generation_two.clone())),
                ScriptedProjection::Error("injected enrollment projection read failure"),
            ])),
        };
        let (mut owner, handle) = enrollment_authority_channel();
        let command_task = tokio::spawn(async move {
            for _ in 0..8 {
                let command = owner.commands.recv().await.expect("authority command");
                handle_authority_command(&script, &owner, command).await;
            }
        });
        let verifier =
            AgentRequestAdmissionVerifier::new(node.clone(), target.clone(), handle.clone());

        assert!(handle
            .fresh_member_authorization(member.did())
            .await
            .unwrap()
            .is_some());
        let revoked_request_id = revoked_request.request_id.clone();
        let rejected = crate::agent::daemon::verify_request_at_claim_boundary(
            &verifier,
            node.clone(),
            "behavior-1",
            revoked_request,
        )
        .await;
        assert!(rejected.is_none());
        #[derive(Deserialize)]
        struct RejectedRow {
            lifecycle_state: RequestLifecycleState,
            claimed_at: Option<String>,
            failure_reason: Option<String>,
        }
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    lifecycle_state claimed_at failure_reason
                }} }}"#,
                crate::graphql::escape_graphql_string(&revoked_request_id),
            ))
            .await;
        assert!(
            !response.has_errors(),
            "query rejected request: {:?}",
            response.errors
        );
        let rejected_row: RejectedRow = crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .expect("rejected request row");
        assert_eq!(rejected_row.lifecycle_state, RequestLifecycleState::Failed);
        assert!(rejected_row.claimed_at.is_none());
        assert!(rejected_row
            .failure_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("request admission denied")));

        assert!(handle
            .fresh_member_authorization(member.did())
            .await
            .unwrap()
            .is_some());
        let expiry_error = verifier
            .verify_fresh_at(
                &expired_request,
                "behavior-1",
                "2030-01-01T00:05:00Z".parse().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(expiry_error.is_denied());
        assert!(expiry_error.to_string().contains("lease expired"));

        let first_fence = handle
            .fresh_member_authorization(member.did())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_fence.authorization_sequence, 1);
        let superseded_error = verifier
            .verify_fresh_at(
                &superseded_request,
                "behavior-1",
                "2030-01-01T00:06:00Z".parse().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(superseded_error.is_denied());
        assert!(superseded_error
            .to_string()
            .contains("stale or mixed enrollment generation"));

        let verified = verifier
            .verify_fresh_at(
                &replacement_request,
                "behavior-1",
                "2030-01-01T00:06:00Z".parse().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(verified.request_id, replacement_request.request_id);

        let read_error = verifier
            .verify_fresh_at(
                &replacement_request,
                "behavior-1",
                "2030-01-01T00:06:00Z".parse().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(!read_error.is_denied());
        assert!(format!("{read_error:#}").contains("injected enrollment projection read failure"));
        command_task.await.unwrap();

        let replacement_request_id = replacement_request.request_id.clone();
        assert!(crate::agent::daemon::verify_request_at_claim_boundary(
            &verifier,
            node.clone(),
            "behavior-1",
            replacement_request,
        )
        .await
        .is_none());
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    lifecycle_state claimed_at failure_reason
                }} }}"#,
                crate::graphql::escape_graphql_string(&replacement_request_id),
            ))
            .await;
        let pending_row: RejectedRow = crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .expect("temporarily unavailable request row");
        assert_eq!(pending_row.lifecycle_state, RequestLifecycleState::Pending);
        assert!(pending_row.claimed_at.is_none());
        assert!(pending_row
            .failure_reason
            .as_deref()
            .is_none_or(str::is_empty));
    }
}
