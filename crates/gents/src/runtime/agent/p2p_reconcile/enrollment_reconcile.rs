//! Single I/O owner for authenticated enrollment authority and its effects.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use defra_node::Node as EmbeddedNode;
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::store::identity::NodeIdentity;

use super::enrollment_store::{EnrollmentProjection, GraphqlEnrollmentStore};
use super::graphql_helpers::rows;
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
    pub owner_node: String,
    pub profile: String,
    pub request_digest: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
}

enum EnrollmentAuthorityCommand {
    Authorization {
        member_did: String,
        member_peer: String,
        ack: oneshot::Sender<Result<Option<EnrollmentAuthorizationFence>>>,
    },
    MemberAdmission {
        member_did: String,
        ack: oneshot::Sender<Result<bool>>,
    },
    PeerAuthorization {
        member_peer: String,
        ack: oneshot::Sender<Result<Option<EnrollmentAuthorizationFence>>>,
    },
    MemberAuthorization {
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
        owner_node: &str,
    ) -> Result<bool>;
}

impl EnrollmentAuthorityHandle {
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

    /// Reload the document set and return the exact current authorization
    /// generation for one member.
    pub async fn fresh_authorization(
        &self,
        member_did: &str,
        member_peer: &str,
    ) -> Result<Option<EnrollmentAuthorizationFence>> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(EnrollmentAuthorityCommand::Authorization {
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

    /// A long-running effect remains bound to its exact signed authorization.
    /// Watch observations also carry data-plane failures and can lag a fresh
    /// command, so a change wakes a signed-authority reload rather than revoking
    /// by itself. Expiry bounds even a reload whose owner stops responding.
    pub async fn authorization_lost(&self, expected: &EnrollmentAuthorizationFence) -> Result<()> {
        let mut changes = self.receiver.clone();
        let expires = chrono::DateTime::parse_from_rfc3339(&expected.authorization_expires_at)
            .context("enrollment authorization has an invalid expiry")?;
        let remaining = expires
            .signed_duration_since(Utc::now())
            .to_std()
            .unwrap_or_default();
        let expiry = tokio::time::sleep(remaining);
        tokio::pin!(expiry);
        loop {
            let current = tokio::select! {
                current = self.fresh_authorization(&expected.member_did, &expected.member_peer) => current?,
                _ = &mut expiry => return Ok(()),
            };
            if current.as_ref() != Some(expected) {
                return Ok(());
            }
            tokio::select! {
                result = changes.changed() => result.context("enrollment authority owner stopped")?,
                _ = &mut expiry => return Ok(()),
            }
        }
    }

    /// Resolve one transport peer to its exact current generation; transport
    /// reconciliation has the peer identity before a trusted member DID, so
    /// the DID derives from the signed authority projection, not a desired row.
    pub async fn fresh_peer_authorization(
        &self,
        member_peer: &str,
    ) -> Result<Option<EnrollmentAuthorizationFence>> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(EnrollmentAuthorityCommand::PeerAuthorization {
                member_peer: member_peer.to_string(),
                ack,
            })
            .await
            .context("enrollment authority owner stopped")?;
        result
            .await
            .context("enrollment authority owner dropped fresh peer authorization response")?
    }

    /// Resolve an exact current generation for a signed request principal;
    /// duplicate active identities fail closed.
    pub async fn fresh_member_authorization(
        &self,
        member_did: &str,
    ) -> Result<Option<EnrollmentAuthorizationFence>> {
        let (ack, result) = oneshot::channel();
        self.commands
            .send(EnrollmentAuthorityCommand::MemberAuthorization {
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
            .send(EnrollmentAuthorityCommand::MemberAdmission {
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
        owner_node: &str,
    ) -> Result<bool> {
        Ok(self
            .fresh_member_authorization(member_did)
            .await?
            .is_some_and(|authorization| authorization.owner_node == owner_node))
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
                EnrollmentAuthorityCommand::MemberAuthorization { member_did, ack } => {
                    let _ = ack.send(Ok(current.filter(|value| value.member_did == member_did)));
                }
                EnrollmentAuthorityCommand::Authorization {
                    member_did,
                    member_peer,
                    ack,
                } => {
                    let _ = ack.send(Ok(current.filter(|value| {
                        value.member_did == member_did && value.member_peer == member_peer
                    })));
                }
                EnrollmentAuthorityCommand::PeerAuthorization { member_peer, ack } => {
                    let _ = ack.send(Ok(current.filter(|value| value.member_peer == member_peer)));
                }
                EnrollmentAuthorityCommand::MemberAdmission { member_did, ack } => {
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

fn publish_ready(
    sender: &watch::Sender<EnrollmentAuthorityState>,
    projection: EnrollmentProjection,
) {
    let next = EnrollmentAuthorityState::Ready(Arc::new(projection));
    sender.send_if_modified(|current| {
        if *current == next {
            return false;
        }
        *current = next;
        true
    });
}

fn publish_failed(sender: &watch::Sender<EnrollmentAuthorityState>, error: &anyhow::Error) {
    let next = EnrollmentAuthorityState::Failed(Arc::from(format!("{error:#}")));
    sender.send_if_modified(|current| {
        if *current == next {
            return false;
        }
        *current = next;
        true
    });
}

/// Authority rows plus the applied pairing witness that a route receipt
/// records.
const ENROLLMENT_SWEEP_COLLECTIONS: [&str; 6] = [
    "Network",
    "NetworkEnrollmentRequest",
    "NetworkEnrollmentDecision",
    "NetworkAuthorizationRevision",
    "NetworkEnrollmentRouteReceipt",
    "PeerPairingApplied",
];

/// Run the sole durable enrollment projection/effect loop. This loop publishes
/// its immutable projection and owns `source="enrollment"` data-plane rows.
pub async fn run_enrollment_reconciler(
    node: Arc<EmbeddedNode>,
    identity: Arc<dyn NodeIdentity>,
    mut owner: EnrollmentAuthorityOwner,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlEnrollmentStore::new(node.clone(), identity.clone());
    let pairing_admin = EmbeddedRemoteP2pAdmin::new(node.clone());
    let mut delivered = BTreeSet::new();
    let mut renewal_notices = super::enrollment_renewal::RenewalNotices::default();
    let mut subscription = crate::subscribe_collections(&node, ENROLLMENT_SWEEP_COLLECTIONS)?;
    let mut subscription_open = true;
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let sweep = sweep_enrollment(
            &node,
            &identity,
            &store,
            &pairing_admin,
            &owner.sender,
            &mut delivered,
            &mut renewal_notices,
        );
        if !serve_authority_during_sweep(sweep, &store, &owner.sender, &mut owner.commands, &cancel)
            .await
        {
            return Ok(());
        }
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                _ = interval.tick() => break,
                command = owner.commands.recv() => {
                    let Some(command) = command else { return Ok(()); };
                    handle_authority_command(&store, &owner.sender, command).await;
                }
                batch = subscription.recv(), if subscription_open => {
                    if batch.is_none() {
                        tracing::warn!("enrollment change subscription closed; continuing periodic sweeps");
                        subscription_open = false;
                        continue;
                    }
                    break;
                }
            }
        }
    }
}

/// Network effects can require transport admission through this same authority.
/// Keep fresh database-backed checks responsive while the sweep awaits those effects.
async fn serve_authority_during_sweep(
    sweep: impl std::future::Future<Output = ()>,
    store: &impl EnrollmentProjectionReader,
    sender: &watch::Sender<EnrollmentAuthorityState>,
    commands: &mut mpsc::Receiver<EnrollmentAuthorityCommand>,
    cancel: &CancellationToken,
) -> bool {
    tokio::pin!(sweep);
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return false,
            _ = &mut sweep => return true,
            command = commands.recv() => {
                let Some(command) = command else { return false; };
                handle_authority_command(store, sender, command).await;
            }
        }
    }
}

async fn handle_authority_command(
    store: &impl EnrollmentProjectionReader,
    sender: &watch::Sender<EnrollmentAuthorityState>,
    command: EnrollmentAuthorityCommand,
) {
    match command {
        EnrollmentAuthorityCommand::Authorization {
            member_did,
            member_peer,
            ack,
        } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    publish_ready(sender, projection.clone());
                    exact_authorization_fence(&projection, &member_did, &member_peer)
                }
                Err(error) => {
                    publish_failed(sender, &error);
                    Err(error.context("reload enrollment authority for authorization fence"))
                }
            };
            let _ = ack.send(result);
        }
        EnrollmentAuthorityCommand::MemberAdmission { member_did, ack } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    publish_ready(sender, projection.clone());
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
                    publish_failed(sender, &error);
                    Err(error.context("reload enrollment authority for peer admission"))
                }
            };
            let _ = ack.send(result);
        }
        EnrollmentAuthorityCommand::PeerAuthorization { member_peer, ack } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    publish_ready(sender, projection.clone());
                    exact_peer_authorization_fence(&projection, &member_peer)
                }
                Err(error) => {
                    publish_failed(sender, &error);
                    Err(error.context("reload enrollment authority for transport fence"))
                }
            };
            let _ = ack.send(result);
        }
        EnrollmentAuthorityCommand::MemberAuthorization { member_did, ack } => {
            let result = match store.load_authority_projection().await {
                Ok(projection) => {
                    publish_ready(sender, projection.clone());
                    exact_member_authorization_fence(&projection, &member_did)
                }
                Err(error) => {
                    publish_failed(sender, &error);
                    Err(error.context("reload enrollment authority for request admission"))
                }
            };
            let _ = ack.send(result);
        }
    }
}

pub(crate) fn exact_authorization_fence(
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
        owner_node: active.request.owner_node_did.clone(),
        profile: active.request.profile.clone(),
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
    identity: &Arc<dyn NodeIdentity>,
    store: &GraphqlEnrollmentStore,
    pairing_admin: &EmbeddedRemoteP2pAdmin,
    sender: &watch::Sender<EnrollmentAuthorityState>,
    delivered: &mut BTreeSet<String>,
    renewal_notices: &mut super::enrollment_renewal::RenewalNotices,
) {
    let projection = match store.load_projection().await {
        Ok(projection) => projection,
        Err(error) => {
            publish_failed(sender, &error);
            tracing::warn!(error = %error, "enrollment projection read failed; authority remains fail closed");
            return;
        }
    };
    publish_ready(sender, projection.clone());
    for notice in renewal_notices.observe(&projection, Utc::now()) {
        super::enrollment_renewal::log_renewal_notice(&notice);
    }

    if let Err(error) = reconcile_data_plane(node, identity.did(), store, &projection).await {
        publish_failed(sender, &error);
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
    identity: &Arc<dyn NodeIdentity>,
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
                node_did: active.request.candidate_did.clone(),
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
        if !applied.enrollment.binds(
            &active.request.request_digest,
            active.revision.sequence,
            &active.revision.authorization_expires_at,
        ) || !applied.duplicate_doc_ids.is_empty()
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
            active.request.profile == CLIENT_TEMPLATE
                && gents_protocol::enrollment::authorization_lease_is_fresh_at(
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
    node: &std::sync::Arc<EmbeddedNode>,
    local_did: &str,
    store: &GraphqlEnrollmentStore,
    projection: &EnrollmentProjection,
) -> Result<()> {
    anyhow::ensure!(
        node.node_identity_did() == Some(local_did),
        "enrollment reconciler has another runtime identity"
    );
    let runtime = identity::Did::new(local_did)?;
    let access = crate::store::config_client::ConfigAccess::local_as(node.clone(), runtime.clone());
    let (response, owned) = access
        .transact_readonly("p2p.query_enrollment_routes", |txn| {
            let runtime = &runtime;
            Box::pin(async move {
                let response = txn.execute_local_response(
                    r#"{
                PeerPairingDesired {
                    _docID peer_id node_did actor_did template source collections replicator_addresses
                    enrollment_request_digest enrollment_authorization_sequence
                    enrollment_authorization_expires_at
                }
            }"#,
                )
                .await?;
                let ids = rows::<serde_json::Value>(&response, "PeerPairingDesired")?.into_iter()
                    .map(|row| row["_docID"].as_str().map(str::to_owned).context("desired route has no ID"))
                    .collect::<Result<Vec<_>>>()?;
                let owners = if ids.is_empty() { Vec::new() } else { txn.document_owners("PeerPairingDesired", &ids).await? };
                anyhow::ensure!(owners.len() == ids.len(), "desired route ownership is incomplete");
                let owned = owners.into_iter().filter(|row| row.visible && row.owner.as_ref() == Some(runtime))
                    .map(|row| row.doc_id).collect::<BTreeSet<_>>();
                Ok::<_, anyhow::Error>((response, owned))
            })
        })
        .await?;
    let existing = rows::<EnrollmentRouteRow>(&response, "PeerPairingDesired")?;
    let desired = desired_routes(projection);
    let client = resolve_template(CLIENT_TEMPLATE).context("client template is missing")?;
    let client_collections = client
        .collections
        .iter()
        .map(|collection| collection.to_string())
        .collect::<BTreeSet<_>>();

    let mut seen = BTreeSet::new();
    let duplicate_peers = existing
        .iter()
        .filter_map(|row| (!seen.insert(row.peer_id.clone())).then_some(row.peer_id.clone()))
        .collect::<BTreeSet<_>>();

    let foreign_peers = existing
        .iter()
        .filter(|row| {
            row.source.as_deref() != Some(SOURCE_ENROLLMENT)
                || row.actor_did.as_deref() != Some(local_did)
                || !owned.contains(&row.doc_id)
        })
        .map(|row| row.peer_id.clone())
        .collect::<BTreeSet<_>>();
    let enrollment_rows = existing
        .into_iter()
        .filter(|row| {
            row.source.as_deref() == Some(SOURCE_ENROLLMENT)
                && row.actor_did.as_deref() == Some(local_did)
                && owned.contains(&row.doc_id)
        })
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
        // A durable revoke or superseding generation can commit after the sweep
        // projection was loaded; reload through the authority store at the
        // write boundary and compare the complete signed generation.
        let fresh_projection = match store.load_projection().await {
            Ok(projection) => projection,
            Err(error) => {
                // Fail closed: remove every row this owner can identify so a
                // stale materialization cannot keep granting route admission.
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
        if !duplicate_peers.contains(peer_id)
            && current.is_some_and(|row| {
                row.matches(local_did, route, CLIENT_TEMPLATE, &client_collections)
            })
        {
            continue;
        }
        crate::store::config_client::ConfigAccess::transact_local_write(
            node,
            Some(runtime.clone()),
            "p2p.upsert_enrollment_route",
            |txn| {
                let client_collections = &client_collections;
                Box::pin(async move {
                    let fresh = store.load_projection_in_txn(txn).await?;
                    let fence =
                        exact_authorization_fence(&fresh, &route.member_did, &route.peer_id)?;
                    anyhow::ensure!(
                        fence
                            .as_ref()
                            .is_some_and(|fence| enrollment_route_matches_fence(route, fence)),
                        "enrollment route generation changed before materialization"
                    );
                    super::materialize_enrollment_desired_in_txn(
                        txn,
                        local_did,
                        &super::EnrollmentEndpointEntry {
                            desired_id: route.peer_id.clone(),
                            peer_id: route.peer_id.clone(),
                            node_did: route.member_did.clone(),
                            address: route.address.clone(),
                            request_digest: route.request_digest.clone(),
                            authorization_sequence: route.authorization_sequence,
                            authorization_expires_at: route.authorization_expires_at.clone(),
                        },
                        client_collections,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            },
        )
        .await?;
    }
    for peer_id in enrollment_rows.keys() {
        if desired.contains_key(peer_id) {
            continue;
        }
        delete_enrollment_route(node, peer_id).await?;
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

async fn delete_enrollment_route(node: &std::sync::Arc<EmbeddedNode>, peer_id: &str) -> Result<()> {
    let runtime = identity::Did::new(
        node.node_identity_did()
            .context("enrollment runtime identity missing")?,
    )?;
    let access = crate::store::config_client::ConfigAccess::local_as(node.clone(), runtime.clone());
    let mutation = delete_enrollment_route_mutation(peer_id, runtime.as_str());
    let peer = crate::store::graphql::escape_graphql_string(peer_id);
    let node_did = crate::store::graphql::escape_graphql_string(runtime.as_str());
    access.transact_write("p2p.retract_enrollment_route_fenced", |txn| {
        let runtime = &runtime;
        let mutation = &mutation;
        let peer = &peer;
        let node_did = &node_did;
        Box::pin(async move {
            txn.validate_runtime_actor(runtime)?;
            crate::store::graphql::fence_arrivals(txn, "PeerPairingDesired").await?;
            let response = txn.execute(&format!(r#"{{ PeerPairingDesired(filter: {{peer_id: {{_eq: "{peer}"}}, node_did: {{_eq: "{node_did}"}}, actor_did: {{_eq: "{node_did}"}}, source: {{_eq: "enrollment"}}}}) {{_docID}} }}"#)).await?;
            let ids = gents_protocol::graphql::graphql_rows_from_response(&response, "PeerPairingDesired")
                .into_iter().map(|row| row["_docID"].as_str().map(str::to_owned).context("desired route has no ID"))
                .collect::<Result<Vec<_>>>()?;
            if !ids.is_empty() {
                let owners = txn.document_owners("PeerPairingDesired", &ids).await?;
                anyhow::ensure!(owners.len() == ids.len() && owners.iter().all(|row| row.visible && row.owner.as_ref() == Some(runtime)), "desired route has another native owner");
            }
            txn.execute(mutation).await?;
            Ok::<_, anyhow::Error>(())
        })
    }).await
}

fn delete_enrollment_route_mutation(peer_id: &str, node_did: &str) -> String {
    let peer_id = crate::store::graphql::escape_graphql_string(peer_id);
    let node_did = crate::store::graphql::escape_graphql_string(node_did);
    format!(
        r#"mutation {{
            delete_PeerPairingDesired(
                filter: {{ peer_id: {{ _eq: "{peer_id}" }}, node_did: {{_eq: "{node_did}"}}, actor_did: {{_eq: "{node_did}"}}, source: {{ _eq: "enrollment" }} }}
            ) {{ _docID }}
        }}"#
    )
}

#[derive(Debug, Deserialize)]
struct EnrollmentRouteRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    peer_id: String,
    node_did: Option<String>,
    actor_did: Option<String>,
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
        self.node_did.as_deref() == Some(local_did)
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
            && self.replicator_addresses.as_deref() == Some(std::slice::from_ref(&route.address))
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
    use crate::request::admission::AgentRequestAdmissionVerifier;
    use crate::runtime::agent::p2p_reconcile::enrollment_store::{
        ActiveEnrollment, RevokedEnrollment,
    };
    use crate::store::identity::KeyIdentity;

    #[tokio::test]
    async fn signed_enrollment_repairs_p2p_merged_production_unique_index_twins() {
        use crate::runtime::agent::p2p_reconcile::embedded_impl::push_documents_to_peer;
        use crate::runtime::agent::p2p_reconcile::embedded_impl::tests::fixtures::{
            authorize_enrollment_peer, runtime_http_test_node, wait_for_active_peer,
            wait_for_peer_info,
        };
        use crate::runtime::agent::p2p_reconcile::{PairingFilters, RemoteP2pAdmin};
        use crate::store::config_client::ConfigAccess;
        use crate::store::graphql::escape_graphql_string;
        use crate::store::identity::NodeIdentity;
        use defra_p2p_adapter::P2pDocumentRequest;

        let keys = tempfile::tempdir().unwrap();
        let admin: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(keys.path().join("admin.key"), None).unwrap());
        let member: Arc<dyn NodeIdentity> =
            Arc::new(KeyIdentity::load_or_create(keys.path().join("member.key"), None).unwrap());
        let (receiver_test, _) = runtime_http_test_node(admin.as_ref()).await;
        let (sender_test, _) = runtime_http_test_node(member.as_ref()).await;
        let receiver = &receiver_test.node;
        let sender = &sender_test.node;
        for node in [receiver, sender] {
            let schema = node.get_collection("PeerPairingDesired").unwrap().unwrap();
            assert!(
                schema.indexes.iter().any(|index| {
                    index.unique && index.fields.len() == 1 && index.fields[0].name == "peer_id"
                }),
                "the fixture must retain the production unique index"
            );
        }
        let sender_admin = EmbeddedRemoteP2pAdmin::new(sender.clone());
        let receiver_admin = EmbeddedRemoteP2pAdmin::new(receiver.clone());
        let sender_addresses = wait_for_peer_info(&sender_admin).await;
        let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
        sender_admin.connect(&receiver_addresses).await.unwrap();
        wait_for_active_peer(&sender_admin).await;
        wait_for_active_peer(&receiver_admin).await;
        let peer = sender.p2p().unwrap().local_peer_id().await.unwrap();
        let ticket = sender
            .p2p()
            .unwrap()
            .shareable_address()
            .await
            .unwrap()
            .unwrap();
        authorize_enrollment_peer(
            receiver.clone(),
            admin.clone(),
            member.clone(),
            &peer,
            &ticket,
        )
        .await;
        let store = GraphqlEnrollmentStore::new(receiver.clone(), admin.clone());
        let projection = store.load_projection().await.unwrap();
        let signed = exact_authorization_fence(&projection, member.did(), &peer)
            .unwrap()
            .expect("current signed enrollment");
        assert_eq!(signed.authorization_sequence, 1);

        // Both local writes obey uniqueness. Only a remote merge may retain twins.
        let mut ids = Vec::new();
        for (node, digest) in [
            (receiver, "previous-request-a"),
            (sender, "previous-request-b"),
        ] {
            let input = serde_json::json!({
                "peer_id": peer, "node_did": admin.did(), "source": "enrollment",
                "template": "client", "collections": ["AgentRequest"], "profiles": null,
                "replicator_addresses": ["previous-address"],
                "enrollment_request_digest": digest, "enrollment_authorization_sequence": 1,
                "enrollment_authorization_expires_at": signed.authorization_expires_at,
            });
            let response = ConfigAccess::transact_local_write(
                node, None, "test.desired.seed_unique_replica", |txn| {
                    let input = input.clone();
                    Box::pin(async move {
                        txn.execute_with_variables(
                            "mutation($input: PeerPairingDesiredMutationInputArg!) { create_PeerPairingDesired(input: $input) { _docID } }",
                            &serde_json::json!({"input": input}),
                        ).await
                    })
                },
            ).await.unwrap();
            ids.push(
                crate::store::graphql::created_doc_id(&response, "PeerPairingDesired").unwrap(),
            );
        }
        assert_ne!(ids[0], ids[1]);
        let mut updates = crate::subscribe_collections(receiver, ["PeerPairingDesired"]).unwrap();
        receiver_admin
            .add_replicator(
                &sender_addresses,
                &["PeerPairingDesired".into()],
                &PairingFilters::default(),
            )
            .await
            .unwrap();
        let receiver_peer = receiver.p2p().unwrap().local_peer_id().await.unwrap();
        push_documents_to_peer(
            sender,
            &receiver_peer,
            vec![P2pDocumentRequest {
                collection: "PeerPairingDesired".into(),
                doc_id: ids[1].clone(),
            }],
        )
        .await
        .unwrap();
        let access = ConfigAccess::local(receiver.clone());
        const ROWS: &str = "{ PeerPairingDesired { _docID peer_id node_did source template collections profiles replicator_addresses enrollment_request_digest enrollment_authorization_sequence enrollment_authorization_expires_at created_at updated_at } }";
        // Delivery ACK only admits a head; observe both fully merged documents.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let response = access.execute(ROWS).await.unwrap();
                let rows = response["data"]["PeerPairingDesired"].as_array().unwrap();
                if ids
                    .iter()
                    .all(|id| rows.iter().any(|row| row["_docID"] == *id))
                {
                    break;
                }
                updates
                    .recv()
                    .await
                    .expect("desired subscription stays open");
            }
        })
        .await
        .expect("both genuinely remote-merged twins become visible");
        sender.shutdown().await.expect("node shutdown");
        let before = access.execute(ROWS).await.unwrap();
        let before_rows = before["data"]["PeerPairingDesired"].as_array().unwrap();
        assert_eq!(before_rows.len(), 2);
        ids.sort();
        let parked = before_rows
            .iter()
            .find(|row| row["_docID"] == ids[1])
            .unwrap()
            .clone();
        assert_ne!(
            before_rows[0]["enrollment_request_digest"],
            before_rows[1]["enrollment_request_digest"]
        );
        assert!(before_rows
            .iter()
            .all(|row| row["enrollment_authorization_sequence"] == 1));

        let old = ConfigAccess::write_local(receiver, "test.desired.old_filter_upsert", &format!(
            "mutation {{ upsert_PeerPairingDesired(filter: {{peer_id: {{_eq: \"{}\"}}}}, add: {{peer_id: \"{}\", collections: null, replicator_addresses: null}}, update: {{template: \"client\"}}) {{ _docID }} }}",
            escape_graphql_string(&peer), escape_graphql_string(&peer),
        )).await;
        assert!(
            old.is_err(),
            "real indexed twins must reproduce the old upsert failure; got {old:?}"
        );
        assert!(format!("{:#}", old.unwrap_err()).contains("multiple matching documents"));
        assert_eq!(
            access.execute(ROWS).await.unwrap(),
            before,
            "failed upsert is atomic"
        );

        // Exercise the production caller: signed projection is reloaded and fenced
        // inside the same transaction as canonical witness materialization.
        reconcile_data_plane(receiver, admin.did(), &store, &projection)
            .await
            .unwrap();
        let after = access.execute(ROWS).await.unwrap();
        let after_rows = after["data"]["PeerPairingDesired"].as_array().unwrap();
        assert_eq!(after_rows.len(), 2);
        assert_eq!(
            after_rows
                .iter()
                .find(|row| row["_docID"] == ids[1])
                .unwrap(),
            &parked
        );
        let canonical = after_rows
            .iter()
            .find(|row| row["_docID"] == ids[0])
            .unwrap();
        assert_eq!(canonical["node_did"], admin.did());
        assert_eq!(
            canonical["enrollment_request_digest"],
            signed.request_digest
        );
        assert_eq!(
            canonical["enrollment_authorization_sequence"],
            signed.authorization_sequence
        );
        assert_eq!(
            canonical["enrollment_authorization_expires_at"],
            signed.authorization_expires_at
        );
        assert_eq!(
            canonical["replicator_addresses"],
            serde_json::json!([ticket])
        );
        let indexed = access
            .execute(&format!(
                "{{ PeerPairingDesired(filter: {{peer_id: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                escape_graphql_string(&peer),
            ))
            .await
            .unwrap();
        assert_eq!(
            indexed["data"]["PeerPairingDesired"],
            serde_json::json!([{ "_docID": ids[0] }])
        );
        reconcile_data_plane(receiver, admin.did(), &store, &projection)
            .await
            .unwrap();
        assert_eq!(
            access.execute(ROWS).await.unwrap(),
            after,
            "a repeated sweep preserves both witnesses"
        );
        receiver.shutdown().await.expect("node shutdown");
    }

    enum ScriptedProjection {
        Projection(Box<EnrollmentProjection>),
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
                ScriptedProjection::Projection(projection) => Ok(*projection),
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
            owner_node_did: "did:key:owner-placeholder".into(),
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
            owner_node_did: request.owner_node_did.clone(),
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
            owner_node_did: request.owner_node_did.clone(),
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

    #[test]
    fn client_route_requires_a_fresh_client_profile() {
        let now = "2030-01-01T00:00:00Z".parse().unwrap();
        let client = active_enrollment(1, "2031-01-01T00:00:00Z");
        let projection = projection_with_active(client.clone());
        assert!(desired_routes_at(&projection, now).contains_key(&client.request.candidate_peer));

        for profile in ["pack:example/game", "peer", ""] {
            let mut other = client.clone();
            other.request.profile = profile.into();
            assert!(desired_routes_at(&projection_with_active(other), now).is_empty());
        }
        let expired = active_enrollment(2, "2029-01-01T00:00:00Z");
        assert!(desired_routes_at(&projection_with_active(expired), now).is_empty());
    }

    struct CurrentProjectionReader(Arc<Mutex<EnrollmentProjection>>);

    #[async_trait]
    impl EnrollmentProjectionReader for CurrentProjectionReader {
        async fn load_authority_projection(&self) -> Result<EnrollmentProjection> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    #[tokio::test]
    async fn delivery_authorization_survives_transport_failure_and_stale_observations() {
        let active = active_enrollment(1, "2099-01-01T00:00:00Z");
        let projection = projection_with_active(active.clone());
        let expected = exact_authorization_fence(
            &projection,
            &active.request.candidate_did,
            &active.request.candidate_peer,
        )
        .unwrap()
        .unwrap();
        let loaded = Arc::new(Mutex::new(projection.clone()));
        let reader = CurrentProjectionReader(loaded.clone());
        let (mut owner, handle) = enrollment_authority_channel();
        let sender = owner.sender.clone();
        publish_ready(&sender, projection.clone());
        let task = tokio::spawn(async move {
            while let Some(command) = owner.commands.recv().await {
                handle_authority_command(&reader, &owner.sender, command).await;
            }
        });
        let watch = handle.authorization_lost(&expected);
        tokio::pin!(watch);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut watch)
                .await
                .is_err()
        );
        publish_failed(&sender, &anyhow::anyhow!("unrelated route write failed"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut watch)
                .await
                .is_err(),
            "a data-plane failure does not revoke successfully loaded authority"
        );
        let mut unrelated = projection;
        let mut other = active_enrollment(9, "2099-01-01T00:00:00Z");
        other.request.candidate_did = "did:key:another-member".into();
        other.request.candidate_peer = "another-peer".into();
        unrelated.active.push(other);
        *loaded.lock().unwrap() = unrelated.clone();
        publish_ready(&sender, unrelated);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut watch)
                .await
                .is_err()
        );
        // A sweep can publish an old snapshot after a fresh command has reloaded.
        publish_ready(&sender, EnrollmentProjection::default());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut watch)
                .await
                .is_err(),
            "a stale observation must be reloaded before cancelling"
        );
        *loaded.lock().unwrap() =
            projection_with_active(active_enrollment(2, "2099-01-01T00:00:00Z"));
        publish_ready(&sender, loaded.lock().unwrap().clone());
        tokio::time::timeout(std::time::Duration::from_secs(1), watch)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn delivery_authorization_expires_even_when_reload_is_stalled() {
        let active = active_enrollment(
            1,
            &(Utc::now() + chrono::Duration::seconds(60)).to_rfc3339(),
        );
        let expected = exact_authorization_fence(
            &projection_with_active(active.clone()),
            &active.request.candidate_did,
            &active.request.candidate_peer,
        )
        .unwrap()
        .unwrap();
        let (_owner, handle) = enrollment_authority_channel();
        tokio::time::timeout(
            std::time::Duration::from_secs(61),
            handle.authorization_lost(&expected),
        )
        .await
        .unwrap()
        .unwrap();
    }

    #[tokio::test]
    async fn delivery_authorization_fails_when_owner_stops() {
        let active = active_enrollment(1, "2099-01-01T00:00:00Z");
        let expected = exact_authorization_fence(
            &projection_with_active(active.clone()),
            &active.request.candidate_did,
            &active.request.candidate_peer,
        )
        .unwrap()
        .unwrap();
        let (owner, handle) = enrollment_authority_channel();
        drop(owner);
        assert!(handle.authorization_lost(&expected).await.is_err());
    }

    #[tokio::test]
    async fn stalled_network_sweep_serves_fresh_authority_and_revocation() {
        let active = active_enrollment(1, "2099-01-01T00:00:00Z");
        let member = active.request.candidate_did.clone();
        let script = ScriptedProjectionReader {
            steps: Mutex::new(VecDeque::from([
                ScriptedProjection::Projection(Box::new(projection_with_active(active.clone()))),
                ScriptedProjection::Projection(Box::new(projection_with_revoked(&active))),
                ScriptedProjection::Error("authority unavailable during network replay"),
            ])),
        };
        let (mut owner, handle) = enrollment_authority_channel();
        let (release, held) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            serve_authority_during_sweep(
                async {
                    held.await.unwrap();
                },
                &script,
                &owner.sender,
                &mut owner.commands,
                &CancellationToken::new(),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            assert!(handle
                .fresh_member_authorization(&member)
                .await
                .unwrap()
                .is_some());
            assert!(handle
                .fresh_member_authorization(&member)
                .await
                .unwrap()
                .is_none());
            assert!(handle.fresh_member_authorization(&member).await.is_err());
        })
        .await
        .expect("fresh authority must not wait for network replay");
        assert!(!task.is_finished(), "the network effect is still held");
        release.send(()).unwrap();
        assert!(task.await.unwrap());
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
        target: &dyn NodeIdentity,
        member: &dyn NodeIdentity,
        agent_did: &str,
        active: &ActiveEnrollment,
        label: &str,
    ) -> crate::request::watcher::AgentRequest {
        let mut create = AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            format!("agent-request-{label}"),
            target.did(),
            member.did(),
            agent_did,
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
        crate::request::admission::load_request_for_admission_test(node.as_ref(), doc_id)
            .await
            .expect("load queued AgentRequest")
    }

    #[test]
    fn enrollment_route_delete_remains_scoped_to_its_source() {
        let delete = delete_enrollment_route_mutation("peer-\"\\", "node");
        assert!(delete.contains("source: { _eq: \"enrollment\" }"));
        assert!(delete.contains("peer-\\\"\\\\"));
    }

    #[test]
    fn authority_handle_is_fail_closed_until_owner_publishes() {
        let (owner, handle) = enrollment_authority_channel();
        assert!(handle.current().is_err());
        publish_ready(&owner.sender, EnrollmentProjection::default());
        assert_eq!(*handle.current().unwrap(), EnrollmentProjection::default());
    }

    #[tokio::test]
    async fn final_claim_fence_reloads_revocation_expiry_supersession_and_read_failure() {
        let target_dir = tempfile::tempdir().unwrap();
        let member_dir = tempfile::tempdir().unwrap();
        let target: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(target_dir.path().join("target.key"), None).unwrap(),
        );
        let member: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(member_dir.path().join("member.key"), None).unwrap(),
        );
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(target.did())
                .build()
                .await
                .unwrap(),
        );
        crate::store::schema::ensure_runtime_schemas(node.as_ref())
            .await
            .unwrap();
        let binding = crate::store::identity::provision_agent_identity(target.as_ref()).unwrap();
        crate::test_support::install_test_agent_binding(
            &node,
            target.did(),
            "behavior-1",
            &binding,
        )
        .await;
        let agent =
            crate::store::identity::resolve_agent_identity(target.as_ref(), &binding).unwrap();

        let mut generation_one = active_enrollment(1, "2030-01-01T00:10:00Z");
        generation_one.request.candidate_did = member.did().to_string();
        generation_one.request.owner_node_did = target.did().to_string();
        generation_one.decision.candidate_did = member.did().to_string();
        generation_one.decision.owner_node_did = target.did().to_string();
        generation_one.revision.member_did = member.did().to_string();
        generation_one.revision.owner_node_did = target.did().to_string();
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
        generation_two.request.owner_node_did = target.did().to_string();
        generation_two.decision.candidate_did = member.did().to_string();
        generation_two.decision.owner_node_did = target.did().to_string();
        generation_two.revision.member_did = member.did().to_string();
        generation_two.revision.owner_node_did = target.did().to_string();

        let revoked_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            agent.did(),
            &generation_one,
            "revoked",
        )
        .await;
        let expired_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            agent.did(),
            &expired,
            "expired",
        )
        .await;
        let superseded_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            agent.did(),
            &generation_one,
            "superseded",
        )
        .await;
        let replacement_request = create_enrollment_request(
            node.clone(),
            target.as_ref(),
            member.as_ref(),
            agent.did(),
            &generation_two,
            "replacement",
        )
        .await;

        let script = ScriptedProjectionReader {
            steps: Mutex::new(VecDeque::from([
                ScriptedProjection::Projection(Box::new(projection_with_active(
                    generation_one.clone(),
                ))),
                ScriptedProjection::Projection(Box::new(projection_with_revoked(&generation_one))),
                ScriptedProjection::Projection(Box::new(projection_with_active(expired.clone()))),
                ScriptedProjection::Projection(Box::new(projection_with_active(expired))),
                ScriptedProjection::Projection(Box::new(projection_with_active(
                    generation_one.clone(),
                ))),
                ScriptedProjection::Projection(Box::new(projection_with_active(
                    generation_two.clone(),
                ))),
                ScriptedProjection::Projection(Box::new(projection_with_active(
                    generation_two.clone(),
                ))),
                ScriptedProjection::Error("injected enrollment projection read failure"),
            ])),
        };
        let (mut owner, handle) = enrollment_authority_channel();
        let command_task = tokio::spawn(async move {
            for _ in 0..8 {
                let command = owner.commands.recv().await.expect("authority command");
                handle_authority_command(&script, &owner.sender, command).await;
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
        let rejected = crate::runtime::agent::daemon::verify_request_at_claim_boundary(
            &verifier,
            node.clone(),
            agent.as_ref(),
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
                crate::store::graphql::escape_graphql_string(&revoked_request_id),
            ))
            .await;
        assert!(
            !response.has_errors(),
            "query rejected request: {:?}",
            response.errors
        );
        let rejected_row: RejectedRow = crate::store::graphql::first_row(&response, "AgentRequest")
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
                agent.did(),
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
                agent.did(),
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
                agent.did(),
                "2030-01-01T00:06:00Z".parse().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(verified.request_id, replacement_request.request_id);

        let read_error = verifier
            .verify_fresh_at(
                &replacement_request,
                agent.did(),
                "2030-01-01T00:06:00Z".parse().unwrap(),
            )
            .await
            .unwrap_err();
        assert!(!read_error.is_denied());
        assert!(format!("{read_error:#}").contains("injected enrollment projection read failure"));
        command_task.await.unwrap();

        let replacement_request_id = replacement_request.request_id.clone();
        assert!(
            crate::runtime::agent::daemon::verify_request_at_claim_boundary(
                &verifier,
                node.clone(),
                agent.as_ref(),
                replacement_request,
            )
            .await
            .is_none()
        );
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    lifecycle_state claimed_at failure_reason
                }} }}"#,
                crate::store::graphql::escape_graphql_string(&replacement_request_id),
            ))
            .await;
        let pending_row: RejectedRow = crate::store::graphql::first_row(&response, "AgentRequest")
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
