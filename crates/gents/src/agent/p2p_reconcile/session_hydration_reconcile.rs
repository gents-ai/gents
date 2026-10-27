use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::session_hydration::{
    canonical_manifest_json, SessionHydrationDocumentKey, SessionHydrationReceipt,
    SESSION_HYDRATION_RECEIPT_VERSION,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::engine::EnrollmentMaterialization;
use super::enrollment_reconcile::{EnrollmentAuthorityHandle, EnrollmentAuthorizationFence};
use super::graphql_helpers::rows;
use super::session_hydration::{
    apply_hydration_delivery, decide_hydration, hydration_collection_name, AppliedPairingRoute,
    HydrationApplyOutcome, HydrationCatalog, HydrationDeliveryResult, HydrationDocument,
    HydrationRequest, HydrationTerminalWriteResult, HydrationVerdict, SessionOwner,
    VerifiedActiveMembership,
};
use super::session_hydration_closure::{
    build_canonical_selection, CanonicalClosureInput, CanonicalPublicationOrigin, ScopedDocument,
    ToolEffectRow,
};
use super::templates::{conjunctive_string_eq, decode_pairing_filters};
use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::identity::NodeIdentity;
use crate::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};

mod file_images;
mod inventory;
mod inventory_order;
mod inventory_requests;
pub(crate) use inventory::refusal_reason as file_inventory_refusal_reason;
pub use inventory::{FileInventoryPageError, FileInventorySnapshot};
mod selection;
use selection::load_hydration_selection;

#[derive(Clone, Copy)]
enum DeliveryTarget<'a> {
    Transcript(&'a BTreeSet<HydrationDocument>),
    FileImage(&'a str),
    FileInventory(&'a str),
}

#[cfg(test)]
impl DeliveryTarget<'_> {
    fn transcript(self) -> BTreeSet<HydrationDocument> {
        match self {
            Self::Transcript(documents) => documents.clone(),
            Self::FileImage(_) | Self::FileInventory(_) => {
                panic!("transcript delivery received file images")
            }
        }
    }
}

const HYDRATION_DELIVERY_MAX_ATTEMPTS: usize = 3;
const HYDRATION_DELIVERY_RETRY_BASE: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct HydrationTickOutcome {
    pub served: BTreeSet<String>,
    pub rejected: BTreeSet<String>,
}

#[async_trait]
trait HydrationDelivery: Send + Sync {
    async fn push_documents_to_peer(
        &self,
        peer_id: &str,
        documents: DeliveryTarget<'_>,
    ) -> Result<()>;
}

#[async_trait]
trait HydrationRequestStore: Send + Sync {
    async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>>;
    async fn load_admission(&self, request: &HydrationRequest) -> Result<LoadedHydrationAdmission>;
    async fn load_selection(&self, request: &HydrationRequest) -> Result<HydrationSelection>;
    async fn authorization_is_current(
        &self,
        request: &HydrationRequest,
        fence: &EnrollmentAuthorizationFence,
    ) -> Result<bool>;
    async fn mark_served(
        &self,
        request: &HydrationRequestRow,
        documents: &BTreeSet<HydrationDocument>,
    ) -> Result<()>;
    async fn mark_rejected(&self, request: &HydrationRequestRow, detail: &str) -> Result<()>;
}

#[derive(Debug, Clone)]
struct LoadedHydrationAdmission {
    catalog: HydrationCatalog,
    authorization: Option<EnrollmentAuthorizationFence>,
}

#[derive(Debug, Clone, Default)]
struct HydrationSelection {
    documents: BTreeSet<HydrationDocument>,
    authorized_reference_closure: BTreeSet<SessionHydrationDocumentKey>,
    publications: Vec<CanonicalPublicationOrigin>,
    headers: Vec<crate::session::canonical_rows::TranscriptMessageRow>,
    output_segments: Vec<crate::session::canonical_rows::OutputSegmentRow>,
}

struct AdmittedHydration {
    row: HydrationRequestRow,
    request: HydrationRequest,
    admission: LoadedHydrationAdmission,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HydrationRequestRow {
    pub request_key: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

async fn reconcile_hydration_tick(
    store: &dyn HydrationRequestStore,
    delivery: &dyn HydrationDelivery,
) -> Result<HydrationTickOutcome> {
    let pending = store
        .load_pending_requests()
        .await
        .context("load pending session hydration requests")?;

    let mut outcome = HydrationTickOutcome::default();
    let mut first_error: Option<anyhow::Error> = None;
    let mut record_error = |request_key: &str, error: anyhow::Error| {
        tracing::warn!(
            request_key = %request_key,
            error = %error,
            "session hydration reconcile failed; continuing sweep"
        );
        if first_error.is_none() {
            first_error = Some(error.context(format!("reconcile session hydration {request_key}")));
        }
    };
    let mut admitted = Vec::new();
    for row in pending {
        match admit_or_reject(store, row.clone(), &mut outcome).await {
            Ok(Some(request)) => admitted.push(request),
            Ok(None) => {}
            Err(error) => record_error(&row.request_key, error),
        }
    }
    for request in admitted {
        let request_key = request.row.request_key.clone();
        if let Err(error) = serve_admitted_request(store, delivery, request, &mut outcome).await {
            record_error(&request_key, error);
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(outcome)
}

async fn admit_or_reject(
    store: &dyn HydrationRequestStore,
    row: HydrationRequestRow,
    outcome: &mut HydrationTickOutcome,
) -> Result<Option<AdmittedHydration>> {
    let request = match HydrationRequest::from_row(
        row.request_key.clone(),
        row.requester_did.clone(),
        row.node_did.clone(),
        row.session_id.clone(),
    ) {
        Ok(request) => request,
        Err(detail) => {
            store.mark_rejected(&row, detail).await?;
            outcome.rejected.insert(row.request_key.clone());
            return Ok(None);
        }
    };
    let admission = store
        .load_admission(&request)
        .await
        .context("load hydration admission")?;
    match decide_hydration(&request, &admission.catalog) {
        HydrationVerdict::Admit(_) => Ok(Some(AdmittedHydration {
            row,
            request,
            admission,
        })),
        HydrationVerdict::Reject(detail) => {
            commit_rejection(store, &row, detail, HydrationDeliveryResult::Confirmed).await?;
            tracing::info!(
                request_key = %row.request_key,
                session_id = %row.session_id,
                requester_did = %row.requester_did,
                detail,
                "session hydration request rejected at admission"
            );
            outcome.rejected.insert(row.request_key);
            Ok(None)
        }
    }
}

async fn commit_rejection(
    store: &dyn HydrationRequestStore,
    row: &HydrationRequestRow,
    detail: &'static str,
    delivery_result: HydrationDeliveryResult,
) -> Result<()> {
    let terminal_write = store.mark_rejected(row, detail).await;
    let modeled = apply_hydration_delivery(
        HydrationVerdict::Reject(detail),
        delivery_result,
        if terminal_write.is_ok() {
            HydrationTerminalWriteResult::Committed
        } else {
            HydrationTerminalWriteResult::Failed
        },
    );
    match (terminal_write, modeled) {
        (Ok(()), HydrationApplyOutcome::Rejected { .. }) => Ok(()),
        (Err(error), HydrationApplyOutcome::PendingAfterTerminalWriteFailure { .. }) => {
            Err(error).context("mark session hydration rejected")
        }
        _ => unreachable!("hydration model diverged from rejected receipt commit"),
    }
}

async fn serve_admitted_request(
    store: &dyn HydrationRequestStore,
    delivery: &dyn HydrationDelivery,
    admitted: AdmittedHydration,
    outcome: &mut HydrationTickOutcome,
) -> Result<()> {
    let AdmittedHydration {
        row,
        request,
        admission,
    } = admitted;
    let authorization = admission
        .authorization
        .context("admitted hydration request has no authenticated enrollment fence")?;
    let selection = store
        .load_selection(&request)
        .await
        .context("load hydration selection")?;
    let catalog = HydrationCatalog {
        documents: selection.documents,
        authorized_reference_closure: selection.authorized_reference_closure,
        ..admission.catalog
    };
    let documents = match decide_hydration(&request, &catalog) {
        HydrationVerdict::Admit(documents) => documents,
        HydrationVerdict::Reject(_) => {
            unreachable!("hydration admission does not depend on the selected documents")
        }
    };
    if !store
        .authorization_is_current(&request, &authorization)
        .await
        .context("revalidate hydration authorization generation")?
    {
        let detail = "authenticated enrollment authorization changed before hydration delivery";
        store.mark_rejected(&row, detail).await?;
        outcome.rejected.insert(request.request_key);
        return Ok(());
    }
    let delivery_result =
        deliver_with_bounded_retry(delivery, &request, DeliveryTarget::Transcript(&documents))
            .await;
    if delivery_result == HydrationDeliveryResult::Indeterminate {
        let modeled = apply_hydration_delivery(
            HydrationVerdict::Admit(documents),
            HydrationDeliveryResult::Indeterminate,
            HydrationTerminalWriteResult::NotAttempted,
        );
        debug_assert!(matches!(
            modeled,
            HydrationApplyOutcome::PendingAfterIndeterminateDelivery { .. }
        ));
        return Ok(());
    }
    if !store
        .authorization_is_current(&request, &authorization)
        .await
        .context("revalidate hydration authorization at terminal commit")?
    {
        let detail = "authenticated enrollment authorization changed before hydration commit";
        store.mark_rejected(&row, detail).await?;
        outcome.rejected.insert(request.request_key);
        return Ok(());
    }
    let terminal_write = store.mark_served(&row, &documents).await;
    let modeled = apply_hydration_delivery(
        HydrationVerdict::Admit(documents),
        HydrationDeliveryResult::Confirmed,
        if terminal_write.is_ok() {
            HydrationTerminalWriteResult::Committed
        } else {
            HydrationTerminalWriteResult::Failed
        },
    );
    match (terminal_write, modeled) {
        (Ok(()), HydrationApplyOutcome::Served(_)) => {
            outcome.served.insert(request.request_key);
            Ok(())
        }
        (Err(error), HydrationApplyOutcome::PendingAfterTerminalWriteFailure { .. }) => {
            Err(error).context("mark session hydration served")
        }
        _ => unreachable!("hydration model diverged from served receipt commit"),
    }
}

async fn deliver_with_bounded_retry(
    delivery: &dyn HydrationDelivery,
    request: &HydrationRequest,
    documents: DeliveryTarget<'_>,
) -> HydrationDeliveryResult {
    for attempt in 1..=HYDRATION_DELIVERY_MAX_ATTEMPTS {
        match delivery
            .push_documents_to_peer(&request.peer_id, documents)
            .await
        {
            Ok(()) => return HydrationDeliveryResult::Confirmed,
            Err(error) => {
                tracing::warn!(
                    request_key = %request.request_key,
                    attempt,
                    max_attempts = HYDRATION_DELIVERY_MAX_ATTEMPTS,
                    %error,
                    "session hydration document delivery attempt failed"
                );
                if attempt < HYDRATION_DELIVERY_MAX_ATTEMPTS {
                    tokio::time::sleep(HYDRATION_DELIVERY_RETRY_BASE * attempt as u32).await;
                }
            }
        }
    }
    HydrationDeliveryResult::Indeterminate
}

pub async fn run_session_hydration_reconciler(
    node: Arc<EmbeddedNode>,
    enrollment: EnrollmentAuthorityHandle,
    identity: Arc<dyn NodeIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlHydrationStore {
        node: node.clone(),
        enrollment,
        identity,
    };
    let delivery: Arc<dyn HydrationDelivery> =
        Arc::new(EmbeddedHydrationDelivery { node: node.clone() });
    let mut subscription = crate::subscribe_collections(
        &node,
        [
            "SessionHydrationRequest",
            "FileImageHydrationRequest",
            "FileInventoryRequest",
        ],
    )?;
    let sweep_interval = super::intervals::sweep_interval();
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + sweep_interval, sweep_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut subscription_open = true;

    if !sweep_hydration_requests_until_cancelled(
        &cancel,
        sweep_hydration_requests(&store, delivery.as_ref()),
    )
    .await
    {
        return Ok(());
    }
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                if !sweep_hydration_requests_until_cancelled(&cancel, sweep_hydration_requests(&store, delivery.as_ref())).await {
                    return Ok(());
                }
            },
            batch = subscription.recv(), if subscription_open => {
                let Some(batch) = batch else {
                    tracing::warn!("session-hydration reconciler update subscription closed; continuing with periodic sweeps");
                    subscription_open = false;
                    continue;
                };
                if batch.resync_required {
                    tracing::warn!(
                        updates = batch.updates,
                        "session-hydration document-change capacity exceeded; performing authoritative sweep"
                    );
                }
                if !sweep_hydration_requests_until_cancelled(&cancel, sweep_hydration_requests(&store, delivery.as_ref())).await {
                    return Ok(());
                }
            }
        }
    }
}

async fn sweep_hydration_requests_until_cancelled(
    cancel: &CancellationToken,
    sweep: impl std::future::Future<Output = ()>,
) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = sweep => true,
    }
}

async fn sweep_hydration_requests(store: &GraphqlHydrationStore, delivery: &dyn HydrationDelivery) {
    sweep_transcript_requests(store, delivery).await;
    if let Err(error) = inventory_requests::reconcile_inventory_requests(store, delivery).await {
        tracing::warn!(%error, "file inventory sweep failed");
    }
    if let Err(error) = file_images::reconcile_file_images(store, delivery).await {
        tracing::warn!(%error, "file-image hydration sweep failed");
    }
}

async fn sweep_transcript_requests(
    store: &dyn HydrationRequestStore,
    delivery: &dyn HydrationDelivery,
) {
    match reconcile_hydration_tick(store, delivery).await {
        Ok(outcome) => {
            if !outcome.served.is_empty() || !outcome.rejected.is_empty() {
                tracing::info!(
                    served = ?outcome.served,
                    rejected = ?outcome.rejected,
                    "reconciled session hydration requests"
                );
            }
        }
        Err(error) => {
            tracing::warn!(error = %error, "session-hydration reconcile sweep failed")
        }
    }
}

struct GraphqlHydrationStore {
    node: Arc<EmbeddedNode>,
    enrollment: EnrollmentAuthorityHandle,
    identity: Arc<dyn NodeIdentity>,
}

/// Read-only source operation. The caller must still persist and deliver its
/// correlated receipt through the owned transport; no image readers are added.
pub async fn sign_file_inventory_page(
    node: Arc<EmbeddedNode>,
    enrollment: EnrollmentAuthorityHandle,
    identity: Arc<dyn NodeIdentity>,
    demand: &gents_protocol::file_inventory::FileInventoryPageDemand,
) -> Result<gents_protocol::file_inventory::FileInventoryPageReceipt> {
    let request_key = demand.derive_request_key()?;
    let admission = &demand.admission;
    let store = GraphqlHydrationStore {
        node,
        enrollment,
        identity,
    };
    let loaded = store.inventory_admission(demand).await?;
    anyhow::ensure!(
        matches!(
            decide_hydration(&admission.request, &loaded.catalog),
            HydrationVerdict::Admit(_)
        ),
        "file inventory session is not authorized"
    );
    let fence = loaded
        .authorization
        .context("file inventory has no authenticated enrollment fence")?;
    let snapshot = FileInventorySnapshot::load(&store.node, admission).await?;
    let page = snapshot.page(demand.cursor.as_ref())?;
    crate::file_inventory::sign_current_page(
        store.identity.as_ref(),
        &request_key,
        page,
        || async {
            let current = store.inventory_admission(demand).await?;
            Ok(current.authorization.as_ref() == Some(&fence)
                && matches!(
                    decide_hydration(&admission.request, &current.catalog),
                    HydrationVerdict::Admit(_)
                ))
        },
    )
    .await
}

struct EmbeddedHydrationDelivery {
    node: Arc<EmbeddedNode>,
}

#[async_trait]
impl HydrationDelivery for EmbeddedHydrationDelivery {
    async fn push_documents_to_peer(
        &self,
        peer_id: &str,
        target: DeliveryTarget<'_>,
    ) -> Result<()> {
        let documents = match target {
            DeliveryTarget::Transcript(documents) => documents
                .iter()
                .map(|document| defra_p2p_adapter::P2pDocumentRequest {
                    collection: hydration_collection_name(document.collection).to_owned(),
                    doc_id: document.doc_id.clone(),
                })
                .collect(),
            DeliveryTarget::FileInventory(document) => {
                vec![defra_p2p_adapter::P2pDocumentRequest {
                    collection: gents_protocol::schemas::FILE_INVENTORY_PAGE_NAME.to_owned(),
                    doc_id: document.to_owned(),
                }]
            }
            DeliveryTarget::FileImage(document) => vec![defra_p2p_adapter::P2pDocumentRequest {
                collection: gents_protocol::schemas::TOOL_FILE_CHANGE_NAME.to_owned(),
                doc_id: document.to_owned(),
            }],
        };
        super::embedded_impl::push_documents_to_peer(&self.node, peer_id, documents).await?;
        let explicit = match target {
            DeliveryTarget::FileImage(document) => {
                Some((gents_protocol::schemas::TOOL_FILE_CHANGE_NAME, document))
            }
            DeliveryTarget::FileInventory(document) => {
                Some((gents_protocol::schemas::FILE_INVENTORY_PAGE_NAME, document))
            }
            DeliveryTarget::Transcript(_) => None,
        };
        if let Some((collection, document)) = explicit {
            let acknowledged = super::embedded_impl::EmbeddedRemoteP2pAdmin::new(self.node.clone())
                .document_heads_acknowledged(peer_id, collection, &[document.to_owned()])
                .await?;
            anyhow::ensure!(
                acknowledged == [true],
                "selected file image has not been acknowledged by the peer"
            );
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct PendingRow {
    request_key: Option<String>,
    requester_did: Option<String>,
    node_did: Option<String>,
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct DesiredPairingRow {
    peer_id: Option<String>,
    node_did: Option<String>,
    #[serde(flatten)]
    enrollment: EnrollmentMaterialization,
}

impl DesiredPairingRow {
    fn materializes(&self, authorization: &EnrollmentAuthorizationFence) -> bool {
        self.enrollment.binds(
            &authorization.request_digest,
            authorization.authorization_sequence,
            &authorization.authorization_expires_at,
        )
    }
}

#[derive(Deserialize)]
struct AppliedPairingRow {
    peer_id: Option<String>,
    replicator_filter: Option<String>,
    #[serde(flatten)]
    enrollment: EnrollmentMaterialization,
}

#[derive(Deserialize)]
struct SessionRow {
    session_id: Option<String>,
    requester_did: Option<String>,
    node_did: Option<String>,
}

#[derive(Deserialize)]
struct TranscriptRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
    requester_did: Option<String>,
    node_did: Option<String>,
    session_id: Option<String>,
}

#[async_trait]
impl HydrationRequestStore for GraphqlHydrationStore {
    async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>> {
        let node_did = escape_graphql_string(self.identity.did());
        let query = format!(
            r#"{{
            SessionHydrationRequest(filter: {{ status: {{ _eq: "pending" }}, node_did: {{ _eq: "{node_did}" }} }}) {{
                request_key
                requester_did
                node_did
                session_id
            }}
        }}"#
        );
        let response = graphql_with_transaction_retry(
            &self.node,
            &query,
            "query SessionHydrationRequest pending rows",
        )
        .await?;
        Ok(rows::<PendingRow>(&response, "SessionHydrationRequest")?
            .into_iter()
            .filter_map(|row| {
                Some(HydrationRequestRow {
                    request_key: row.request_key.filter(|value| !value.is_empty())?,
                    requester_did: row.requester_did.unwrap_or_default(),
                    node_did: row.node_did.unwrap_or_default(),
                    session_id: row.session_id.unwrap_or_default(),
                })
            })
            .collect())
    }

    async fn load_admission(&self, request: &HydrationRequest) -> Result<LoadedHydrationAdmission> {
        self.load_admission_for_collection(request, "SessionHydrationRequest")
            .await
    }

    async fn load_selection(&self, request: &HydrationRequest) -> Result<HydrationSelection> {
        load_hydration_selection(&self.node, request).await
    }

    async fn authorization_is_current(
        &self,
        request: &HydrationRequest,
        fence: &EnrollmentAuthorizationFence,
    ) -> Result<bool> {
        Ok(self
            .enrollment
            .fresh_authorization(&request.requester_did, &request.peer_id)
            .await?
            .as_ref()
            == Some(fence))
    }

    async fn mark_served(
        &self,
        request: &HydrationRequestRow,
        documents: &BTreeSet<HydrationDocument>,
    ) -> Result<()> {
        let manifest = documents
            .iter()
            .map(|document| SessionHydrationDocumentKey {
                collection: document.collection,
                doc_id: document.doc_id.clone(),
            })
            .collect::<Vec<_>>();
        let receipt = self.signed_receipt(request, "served", "", manifest).await?;
        let mutation = terminal_mutation(request, &receipt)?;
        let response = crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "p2p.mark_hydration_served",
            &mutation,
        )
        .await?;
        anyhow::ensure!(
            rows::<serde_json::Value>(&response, "update_SessionHydrationRequest")?.len() == 1,
            "session hydration served commit lost its pending-row compare-and-set"
        );
        Ok(())
    }

    async fn mark_rejected(&self, request: &HydrationRequestRow, detail: &str) -> Result<()> {
        let receipt = self
            .signed_receipt(request, "rejected", detail, Vec::new())
            .await?;
        let mutation = terminal_mutation(request, &receipt)?;
        let response = crate::config_client::ConfigAccess::write_local_response(
            &self.node,
            "p2p.mark_hydration_rejected",
            &mutation,
        )
        .await?;
        anyhow::ensure!(
            rows::<serde_json::Value>(&response, "update_SessionHydrationRequest")?.len() == 1,
            "session hydration rejected commit lost its pending-row compare-and-set"
        );
        Ok(())
    }
}

impl GraphqlHydrationStore {
    async fn load_admission_for_collection(
        &self,
        request: &HydrationRequest,
        collection: &str,
    ) -> Result<LoadedHydrationAdmission> {
        let authorization = self
            .enrollment
            .fresh_authorization(&request.requester_did, &request.peer_id)
            .await
            .context("load fresh authenticated enrollment authority for hydration")?;
        let session_id = escape_graphql_string(&request.session_id);
        let peer_id = escape_graphql_string(&request.peer_id);
        let query = hydration_admission_query(&session_id, &peer_id);
        let response =
            graphql_with_transaction_retry(&self.node, &query, "query session hydration admission")
                .await?;

        let desired_agents = rows::<DesiredPairingRow>(&response, "PeerPairingDesired")?
            .into_iter()
            .filter(|row| {
                authorization
                    .as_ref()
                    .is_some_and(|authorization| row.materializes(authorization))
            })
            .filter_map(|row| {
                Some((
                    row.peer_id.filter(|value| !value.is_empty())?,
                    row.node_did.filter(|value| !value.is_empty())?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let applied_rows = rows::<AppliedPairingRow>(&response, "PeerPairingApplied")?;
        let applied_count = applied_rows.len();
        let applied_pairing_routes: BTreeSet<_> = applied_rows
            .into_iter()
            .filter(|row| {
                authorization.as_ref().is_some_and(|authorization| {
                    row.enrollment.binds(
                        &authorization.request_digest,
                        authorization.authorization_sequence,
                        &authorization.authorization_expires_at,
                    )
                })
            })
            .filter_map(|row| applied_pairing_route(row, &desired_agents, collection))
            .collect();
        if authorization.is_some() && applied_pairing_routes.is_empty() {
            tracing::debug!(
                request_key = %request.request_key,
                collection,
                sequence = ?authorization.as_ref().map(|value| value.authorization_sequence),
                current_materializations = desired_agents.len(),
                applied_count,
                "hydration route has no current materialization"
            );
        }
        let sessions = rows::<SessionRow>(&response, "AgentSession")?
            .into_iter()
            .filter_map(|row| {
                Some(SessionOwner {
                    session_id: row.session_id.filter(|value| !value.is_empty())?,
                    requester_did: row.requester_did.unwrap_or_default(),
                    node_did: row.node_did.unwrap_or_default(),
                })
            })
            .collect();
        let (selected_network_id, verified_active_memberships) = match authorization.as_ref() {
            Some(authorization) => (
                authorization.network_id.clone(),
                BTreeSet::from([VerifiedActiveMembership {
                    network_id: authorization.network_id.clone(),
                    member_did: authorization.member_did.clone(),
                }]),
            ),
            None => (String::new(), BTreeSet::new()),
        };
        Ok(LoadedHydrationAdmission {
            catalog: HydrationCatalog {
                applied_pairing_routes,
                selected_network_id,
                verified_active_memberships,
                sessions,
                ..HydrationCatalog::default()
            },
            authorization,
        })
    }

    async fn signed_receipt(
        &self,
        request: &HydrationRequestRow,
        status: &str,
        status_detail: &str,
        served_manifest: Vec<SessionHydrationDocumentKey>,
    ) -> Result<SessionHydrationReceipt> {
        let mut receipt = SessionHydrationReceipt {
            version: SESSION_HYDRATION_RECEIPT_VERSION,
            request_key: request.request_key.clone(),
            requester_did: request.requester_did.clone(),
            node_did: request.node_did.clone(),
            session_id: request.session_id.clone(),
            status: status.to_string(),
            status_detail: status_detail.to_string(),
            served_manifest,
            processed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            signer_did: self.identity.did().to_string(),
            signature: Vec::new(),
        };
        anyhow::ensure!(
            receipt.node_did == receipt.signer_did,
            "hydration reconciler cannot sign for another agent"
        );
        receipt.signature = self.identity.sign(&receipt.signing_payload()?).await?;
        receipt.validate_shape()?;
        Ok(receipt)
    }
}

fn hydration_admission_query(session_id: &str, peer_id: &str) -> String {
    format!(
        r#"{{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "enrollment" }} }}) {{
                peer_id node_did enrollment_request_digest
                enrollment_authorization_sequence enrollment_authorization_expires_at
            }}
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                peer_id replicator_filter enrollment_request_digest
                enrollment_authorization_sequence enrollment_authorization_expires_at
            }}
            AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                session_id requester_did node_did
            }}
        }}"#
    )
}

fn applied_pairing_route(
    row: AppliedPairingRow,
    desired_agents: &BTreeMap<String, String>,
    collection: &str,
) -> Option<AppliedPairingRoute> {
    let peer_id = row.peer_id.filter(|value| !value.is_empty())?;
    let desired_agent = desired_agents.get(&peer_id)?;
    let filters = decode_pairing_filters(row.replicator_filter.as_deref()?).ok()?;
    let request_filter = filters.get(collection)?;
    let requester_did = conjunctive_string_eq(request_filter, "requester_did")?;
    let applied_agent = conjunctive_string_eq(request_filter, "node_did")?;
    if applied_agent != desired_agent {
        return None;
    }
    Some(AppliedPairingRoute {
        peer_id,
        requester_did: requester_did.to_string(),
        node_did: applied_agent.to_string(),
    })
}

fn terminal_mutation(
    request: &HydrationRequestRow,
    receipt: &SessionHydrationReceipt,
) -> Result<String> {
    let request_key = escape_graphql_string(&request.request_key);
    let requester_did = escape_graphql_string(&request.requester_did);
    let node_did = escape_graphql_string(&request.node_did);
    let session_id = escape_graphql_string(&request.session_id);
    let status = escape_graphql_string(&receipt.status);
    let detail = escape_graphql_string(&receipt.status_detail);
    let manifest = escape_graphql_string(&canonical_manifest_json(&receipt.served_manifest)?);
    let processed_at = escape_graphql_string(&receipt.processed_at);
    let signer_did = escape_graphql_string(&receipt.signer_did);
    let signature = escape_graphql_string(&bs58::encode(&receipt.signature).into_string());
    let count = receipt.served_manifest.len();
    Ok(format!(
        r#"mutation {{
            update_SessionHydrationRequest(
                filter: {{
                    request_key: {{ _eq: "{request_key}" }},
                    requester_did: {{ _eq: "{requester_did}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    session_id: {{ _eq: "{session_id}" }},
                    status: {{ _eq: "pending" }}
                }},
                input: {{
                    status: "{status}",
                    status_detail: "{detail}",
                    served_doc_count: {count},
                    served_manifest_json: "{manifest}",
                    processed_at: "{processed_at}",
                    outcome_signer_did: "{signer_did}",
                    outcome_signature: "{signature}"
                }}
            ) {{ _docID }}
        }}"#
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("session_hydration_reconcile/admission_tests.rs");
    use crate::agent::p2p_reconcile::templates::{combine_filters, equality_filter};
    use crate::graphql::ensure_no_errors;

    struct MemoryStore {
        pending: Vec<HydrationRequestRow>,
        catalog: HydrationCatalog,
        authorization_current: std::sync::atomic::AtomicBool,
        authorization_check: Option<Arc<AuthorizationCheckBarrier>>,
        terminal_write_failures_remaining: std::sync::atomic::AtomicUsize,
        selection_loads: std::sync::atomic::AtomicUsize,
        served: std::sync::Mutex<Vec<(String, usize)>>,
        rejected: std::sync::Mutex<Vec<(String, String)>>,
    }

    struct AuthorizationCheckBarrier {
        started: tokio::sync::Notify,
        released: tokio::sync::Notify,
        checks: std::sync::atomic::AtomicUsize,
    }

    struct RecordingDelivery {
        pushed: std::sync::Mutex<Vec<(String, BTreeSet<HydrationDocument>)>>,
    }

    struct FailOnceDelivery {
        attempts: std::sync::atomic::AtomicUsize,
        pushed: std::sync::Mutex<Vec<(String, BTreeSet<HydrationDocument>)>>,
    }

    struct FailingDelivery {
        attempts: std::sync::atomic::AtomicUsize,
        pushed: std::sync::Mutex<Vec<(String, BTreeSet<HydrationDocument>)>>,
    }

    struct BlockingDelivery {
        started: tokio::sync::Notify,
    }

    #[async_trait]
    impl HydrationRequestStore for MemoryStore {
        async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>> {
            Ok(self.pending.clone())
        }
        async fn load_admission(
            &self,
            _request: &HydrationRequest,
        ) -> Result<LoadedHydrationAdmission> {
            let enrolled = !self.catalog.verified_active_memberships.is_empty();
            Ok(LoadedHydrationAdmission {
                catalog: HydrationCatalog {
                    documents: BTreeSet::new(),
                    authorized_reference_closure: BTreeSet::new(),
                    ..self.catalog.clone()
                },
                authorization: enrolled.then(test_authorization_fence),
            })
        }
        async fn load_selection(&self, _request: &HydrationRequest) -> Result<HydrationSelection> {
            self.selection_loads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(HydrationSelection {
                documents: self.catalog.documents.clone(),
                authorized_reference_closure: self.catalog.authorized_reference_closure.clone(),
                publications: Vec::new(),
                headers: Vec::new(),
                output_segments: Vec::new(),
            })
        }
        async fn authorization_is_current(
            &self,
            _request: &HydrationRequest,
            _fence: &EnrollmentAuthorizationFence,
        ) -> Result<bool> {
            if let Some(barrier) = &self.authorization_check {
                if barrier
                    .checks
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    == 0
                {
                    barrier.started.notify_one();
                    barrier.released.notified().await;
                }
            }
            Ok(self
                .authorization_current
                .load(std::sync::atomic::Ordering::SeqCst))
        }
        async fn mark_served(
            &self,
            request: &HydrationRequestRow,
            documents: &BTreeSet<HydrationDocument>,
        ) -> Result<()> {
            if self
                .terminal_write_failures_remaining
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
            {
                anyhow::bail!("injected terminal write failure");
            }
            self.served
                .lock()
                .expect("served lock")
                .push((request.request_key.clone(), documents.len()));
            Ok(())
        }
        async fn mark_rejected(&self, request: &HydrationRequestRow, detail: &str) -> Result<()> {
            if self
                .terminal_write_failures_remaining
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |remaining| remaining.checked_sub(1),
                )
                .is_ok()
            {
                anyhow::bail!("injected terminal write failure");
            }
            self.rejected
                .lock()
                .expect("rejected lock")
                .push((request.request_key.clone(), detail.to_string()));
            Ok(())
        }
    }

    #[async_trait]
    impl HydrationDelivery for RecordingDelivery {
        async fn push_documents_to_peer(
            &self,
            peer_id: &str,
            documents: DeliveryTarget<'_>,
        ) -> Result<()> {
            self.pushed
                .lock()
                .expect("pushed lock")
                .push((peer_id.to_string(), documents.transcript()));
            Ok(())
        }
    }

    #[async_trait]
    impl HydrationDelivery for FailOnceDelivery {
        async fn push_documents_to_peer(
            &self,
            peer_id: &str,
            documents: DeliveryTarget<'_>,
        ) -> Result<()> {
            if self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                anyhow::bail!("temporary transport failure");
            }
            self.pushed
                .lock()
                .expect("pushed lock")
                .push((peer_id.to_string(), documents.transcript()));
            Ok(())
        }
    }

    #[async_trait]
    impl HydrationDelivery for FailingDelivery {
        async fn push_documents_to_peer(
            &self,
            peer_id: &str,
            documents: DeliveryTarget<'_>,
        ) -> Result<()> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.pushed
                .lock()
                .expect("pushed lock")
                .push((peer_id.to_string(), documents.transcript()));
            anyhow::bail!("transport outcome unknown after dispatch")
        }
    }

    #[async_trait]
    impl HydrationDelivery for BlockingDelivery {
        async fn push_documents_to_peer(
            &self,
            _peer_id: &str,
            _documents: DeliveryTarget<'_>,
        ) -> Result<()> {
            self.started.notify_one();
            std::future::pending().await
        }
    }

    fn admitted_store() -> MemoryStore {
        let document = HydrationDocument {
            collection: gents_protocol::session_hydration::SessionHydrationCollection::AgentMessage,
            doc_id: "owned".into(),
            requester_did: "did:key:requester-1".into(),
            node_did: "did:key:agent-1".into(),
            session_id: "session-1".into(),
        };
        MemoryStore {
            pending: vec![HydrationRequestRow {
                request_key: "peer-1:session-1".into(),
                requester_did: "did:key:requester-1".into(),
                node_did: "did:key:agent-1".into(),
                session_id: "session-1".into(),
            }],
            catalog: HydrationCatalog {
                applied_pairing_routes: BTreeSet::from([AppliedPairingRoute {
                    peer_id: "peer-1".into(),
                    requester_did: "did:key:requester-1".into(),
                    node_did: "did:key:agent-1".into(),
                }]),
                selected_network_id: "network-1".into(),
                verified_active_memberships: BTreeSet::from([VerifiedActiveMembership {
                    network_id: "network-1".into(),
                    member_did: "did:key:requester-1".into(),
                }]),
                sessions: BTreeSet::from([SessionOwner {
                    session_id: "session-1".into(),
                    requester_did: "did:key:requester-1".into(),
                    node_did: "did:key:agent-1".into(),
                }]),
                documents: BTreeSet::from([document]),
                authorized_reference_closure: BTreeSet::new(),
            },
            authorization_current: std::sync::atomic::AtomicBool::new(true),
            authorization_check: None,
            terminal_write_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
            selection_loads: std::sync::atomic::AtomicUsize::new(0),
            served: std::sync::Mutex::new(Vec::new()),
            rejected: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn test_authorization_fence() -> EnrollmentAuthorizationFence {
        EnrollmentAuthorizationFence {
            network_id: "network-1".into(),
            request_id: "request-1".into(),
            admin_did: "did:key:admin-1".into(),
            member_did: "did:key:requester-1".into(),
            member_peer: "peer-1".into(),
            member_ticket: "ticket-1".into(),
            owner_node: "did:key:agent-1".into(),
            profile: crate::agent::p2p_reconcile::templates::CLIENT_TEMPLATE.into(),
            request_digest: "digest-1".into(),
            authorization_sequence: 1,
            authorization_expires_at: "2099-09-29T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn admitted_request_pushes_exact_set_then_marks_served() {
        let store = admitted_store();
        let document = store.catalog.documents.first().expect("document").clone();
        let delivery = RecordingDelivery {
            pushed: std::sync::Mutex::new(Vec::new()),
        };
        let outcome = reconcile_hydration_tick(&store, &delivery)
            .await
            .expect("tick");
        assert_eq!(outcome.served, BTreeSet::from(["peer-1:session-1".into()]));
        assert!(outcome.rejected.is_empty());
        let pushed = delivery.pushed.lock().expect("pushed lock").clone();
        assert_eq!(pushed[0].0, "peer-1");
        assert_eq!(pushed[0].1, BTreeSet::from([document]));
        assert_eq!(
            *store.served.lock().expect("served lock"),
            vec![("peer-1:session-1".into(), 1)]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn transient_delivery_failure_retries_within_the_same_sweep() {
        let store = admitted_store();
        let delivery = FailOnceDelivery {
            attempts: std::sync::atomic::AtomicUsize::new(0),
            pushed: std::sync::Mutex::new(Vec::new()),
        };

        let outcome = reconcile_hydration_tick(&store, &delivery)
            .await
            .expect("same sweep retries the transient delivery failure");
        assert_eq!(outcome.served, BTreeSet::from(["peer-1:session-1".into()]));
        assert!(outcome.rejected.is_empty());
        assert_eq!(
            delivery.attempts.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
        assert_eq!(delivery.pushed.lock().expect("pushed lock").len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn indeterminate_delivery_stays_pending_and_retries_idempotently() {
        let store = admitted_store();
        let delivery = FailingDelivery {
            attempts: std::sync::atomic::AtomicUsize::new(0),
            pushed: std::sync::Mutex::new(Vec::new()),
        };

        for sweep in 1..=2 {
            let outcome = reconcile_hydration_tick(&store, &delivery)
                .await
                .expect("ambiguous delivery remains pending for a later sweep");
            assert!(outcome.served.is_empty());
            assert!(outcome.rejected.is_empty());
            assert!(store.served.lock().expect("served lock").is_empty());
            assert!(store.rejected.lock().expect("rejected lock").is_empty());
            assert_eq!(
                delivery.attempts.load(std::sync::atomic::Ordering::SeqCst),
                sweep * HYDRATION_DELIVERY_MAX_ATTEMPTS
            );
        }
        assert_eq!(
            delivery.attempts.load(std::sync::atomic::Ordering::SeqCst),
            2 * HYDRATION_DELIVERY_MAX_ATTEMPTS
        );
        let attempts = delivery.pushed.lock().expect("pushed lock");
        assert_eq!(attempts.len(), 2 * HYDRATION_DELIVERY_MAX_ATTEMPTS);
        assert!(attempts
            .windows(2)
            .all(|attempts| attempts[0] == attempts[1]));
        assert_eq!(
            attempts[0].1, store.catalog.documents,
            "every ambiguous retry remains scoped to the selected content-addressed set"
        );
    }

    #[tokio::test]
    async fn delivered_push_replays_idempotently_after_terminal_write_failure() {
        let mut store = admitted_store();
        store.terminal_write_failures_remaining = std::sync::atomic::AtomicUsize::new(1);
        let delivery = RecordingDelivery {
            pushed: std::sync::Mutex::new(Vec::new()),
        };

        reconcile_hydration_tick(&store, &delivery)
            .await
            .expect_err("first terminal write fails and leaves the request pending");
        assert!(store.served.lock().expect("served lock").is_empty());

        let outcome = reconcile_hydration_tick(&store, &delivery)
            .await
            .expect("a later sweep converges");
        assert_eq!(outcome.served, BTreeSet::from(["peer-1:session-1".into()]));
        let pushes = delivery.pushed.lock().expect("pushed lock");
        assert_eq!(pushes.len(), 2);
        assert_eq!(
            pushes[0], pushes[1],
            "retry sends the same content-addressed document set to the same peer"
        );
        assert_eq!(
            *store.served.lock().expect("served lock"),
            vec![("peer-1:session-1".into(), 1)]
        );
    }

    #[tokio::test]
    async fn revocation_after_catalog_load_blocks_delivery_at_generation_fence() {
        let barrier = Arc::new(AuthorizationCheckBarrier {
            started: tokio::sync::Notify::new(),
            released: tokio::sync::Notify::new(),
            checks: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut store = admitted_store();
        store.authorization_check = Some(barrier.clone());
        let delivery = RecordingDelivery {
            pushed: std::sync::Mutex::new(Vec::new()),
        };
        let tick = reconcile_hydration_tick(&store, &delivery);
        tokio::pin!(tick);
        tokio::select! {
            result = &mut tick => panic!("hydration completed before the authorization fence: {result:?}"),
            _ = barrier.started.notified() => {}
        }
        store
            .authorization_current
            .store(false, std::sync::atomic::Ordering::SeqCst);
        barrier.released.notify_one();
        let outcome = tick.await.unwrap();
        assert!(outcome.served.is_empty());
        assert_eq!(
            outcome.rejected,
            BTreeSet::from(["peer-1:session-1".to_string()])
        );
        assert!(delivery.pushed.lock().unwrap().is_empty());
        assert!(store.served.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn hydration_catalog_filter_is_accepted_by_real_schema() {
        let builder = tokio::task::spawn_blocking(crate::test_support::node_builder)
            .await
            .unwrap();
        let node = Arc::new(builder.build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let admission = hydration_admission_query("session-1", "peer-1");
        let response = node.execute(&admission).await;
        ensure_no_errors(&response, "real hydration admission query").unwrap();
        let did = node.node_identity_did().unwrap().to_owned();
        let request = HydrationRequest {
            request_key: "peer-1:session-1".into(),
            peer_id: "peer-1".into(),
            requester_did: did.clone(),
            node_did: did,
            session_id: "session-1".into(),
        };
        assert!(load_hydration_selection(&node, &request)
            .await
            .unwrap()
            .documents
            .is_empty());
        node.shutdown().await;
    }

    #[tokio::test]
    async fn selection_carries_running_parents_child_audit_records_but_not_forged_children() {
        use crate::tool_call_lifecycle::admission_fixture::{
            published_admission_with_owner, PublishedAdmission, PublishedAdmissionOptions,
        };
        use gents_protocol::output::{OutputSource, PayloadPresentation, PresentedPayload};
        use gents_protocol::session_hydration::SessionHydrationCollection;

        let (fixture, request_owner) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "hydrate-plugin-child".into(),
            real_identity: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let PublishedAdmission {
            node,
            path,
            tool: parent,
            ..
        } = fixture;
        let mut child = parent
            .admit_plugin_effect(1, "fixture_effect", r#"{"value":"input"}"#)
            .await
            .unwrap();
        child.start_running().await.unwrap();
        child
            .complete_raw_with_presentation(
                "effect result",
                "effect result",
                PayloadPresentation::Full,
            )
            .await
            .unwrap();
        let parent_doc = parent.doc_id().unwrap().to_owned();
        let child_doc = child.doc_id().unwrap().to_owned();
        let request_doc = parent.request_doc_id().unwrap().to_owned();
        let session = parent.session_id().to_owned();
        let agent = parent.node_did().to_owned();
        let requester = parent.requester_did().unwrap().to_owned();
        let forged = node
            .execute(&format!(
                r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "forged", tool_call_id: "forged", tool_name: "fixture_effect", request_id: "forged", request_doc_id: "{}", session_id: "{}", node_did: "{}", requester_did: "{}", message_sequence: 0, status: "pending", lifecycle_state: "pending", plugin_parent_tool_call_doc_id: "{}", plugin_effect_ordinal: 2 }}) {{ _docID }} }}"#,
                escape_graphql_string(&request_doc),
                escape_graphql_string(&session),
                escape_graphql_string(&agent),
                escape_graphql_string(&requester),
                escape_graphql_string(&parent_doc),
            ))
            .await;
        ensure_no_errors(&forged, "create forged plugin child").unwrap();
        let forged_doc = crate::graphql::created_doc_id(
            &serde_json::json!({ "data": forged.data }),
            "AgentToolCall",
        )
        .unwrap();

        let request = HydrationRequest::from_row(
            format!("peer-1:{session}"),
            requester.clone(),
            agent.clone(),
            session.clone(),
        )
        .unwrap();
        let messages = graphql_with_transaction_retry(
            &node,
            &format!(
                r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
                escape_graphql_string(&session)
            ),
            "read session messages",
        )
        .await
        .unwrap();
        assert!(rows::<serde_json::Value>(&messages, "AgentMessage")
            .unwrap()
            .iter()
            .map(|row| decode_transcript_message_row(row).unwrap())
            .all(|row| !matches!(
                row.message.publication,
                gents_protocol::output::MessagePublication::ToolDelivery { .. }
            )));
        let selection = load_hydration_selection(&node, &request).await.unwrap();
        let selected = |collection, doc_id: &str| {
            selection
                .documents
                .iter()
                .any(|document| document.collection == collection && document.doc_id == doc_id)
        };
        assert!(selected(
            SessionHydrationCollection::AgentToolCall,
            &parent_doc
        ));
        assert!(selected(
            SessionHydrationCollection::AgentToolCall,
            &child_doc
        ));
        assert!(!selected(
            SessionHydrationCollection::AgentToolCall,
            &forged_doc
        ));

        let response = graphql_with_transaction_retry(
            &node,
            &crate::session::canonical_rows::request_output_segments_query(&request_doc),
            "read child audit segments",
        )
        .await
        .unwrap();
        let segments = rows::<serde_json::Value>(&response, "AgentOutputSegment")
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let arguments = segments
            .iter()
            .filter(|row| {
                row.segment.source
                    == OutputSource::ToolEffectArguments {
                        tool_call_doc_id: child_doc.clone(),
                    }
            })
            .collect::<Vec<_>>();
        assert_eq!(arguments.len(), 1);
        assert!(selected(
            SessionHydrationCollection::AgentOutputSegment,
            &arguments[0].doc_id
        ));
        let output = segments
            .iter()
            .filter(|row| {
                row.segment.source
                    == OutputSource::ToolCall {
                        tool_call_doc_id: child_doc.clone(),
                    }
            })
            .collect::<Vec<_>>();
        assert!(!output.is_empty());
        for row in &output {
            assert!(
                selected(SessionHydrationCollection::AgentOutputSegment, &row.doc_id),
                "child terminal output segment {} must be hydrated",
                row.doc_id
            );
        }
        let terminal = graphql_with_transaction_retry(
            &node,
            &format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ terminal_output }} }}"#,
                escape_graphql_string(&child_doc)
            ),
            "read child terminal",
        )
        .await
        .unwrap();
        let terminal: PresentedPayload = serde_json::from_value(
            rows::<serde_json::Value>(&terminal, "AgentToolCall").unwrap()[0]["terminal_output"]
                .clone(),
        )
        .unwrap();
        assert!(output
            .iter()
            .any(|row| row.doc_id == terminal.output.close_doc_id));

        let effects = crate::config_client::ConfigAccess::transact_local_readonly(
            &node,
            None,
            "test.hydration.effect_children",
            |txn| {
                let parent_doc = parent_doc.clone();
                let requester = requester.clone();
                let agent = agent.clone();
                let request = &request;
                Box::pin(async move {
                    let mut effects = Vec::new();
                    selection::load_effect_children(
                        txn,
                        request,
                        &[ScopedDocument {
                            collection: SessionHydrationCollection::AgentToolCall,
                            doc_id: parent_doc.clone(),
                            requester_did: requester,
                            node_did: agent,
                            session_id: "origin-session".into(),
                        }],
                        &mut effects,
                    )
                    .await?;
                    Ok(effects)
                })
            },
        )
        .await
        .unwrap();
        let found = effects
            .iter()
            .map(|row| row.document.doc_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            found,
            BTreeSet::from([child_doc.as_str(), forged_doc.as_str()])
        );

        drop(request_owner);
        node.shutdown().await;
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn applied_pairing_requires_exact_requester_and_desired_agent() {
        for collection in ["SessionHydrationRequest", "FileImageHydrationRequest"] {
            let filter = combine_filters(
                equality_filter("requester_did", "did:key:requester-1"),
                equality_filter("node_did", "did:key:agent-1"),
            );
            let raw =
                serde_json::to_string(&BTreeMap::from([(collection.to_string(), filter)])).unwrap();
            let row = || AppliedPairingRow {
                peer_id: Some("peer-1".into()),
                replicator_filter: Some(raw.clone()),
                enrollment: Default::default(),
            };
            let desired = BTreeMap::from([("peer-1".to_string(), "did:key:agent-1".to_string())]);
            let route = applied_pairing_route(row(), &desired, collection).expect("exact route");
            assert_eq!(route.requester_did, "did:key:requester-1");
            let wrong_agent =
                BTreeMap::from([("peer-1".to_string(), "did:key:agent-2".to_string())]);
            assert!(applied_pairing_route(row(), &wrong_agent, collection).is_none());
            let other = if collection == "SessionHydrationRequest" {
                "FileImageHydrationRequest"
            } else {
                "SessionHydrationRequest"
            };
            assert!(applied_pairing_route(row(), &desired, other).is_none());
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_delivery() {
        let store = admitted_store();
        let delivery = BlockingDelivery {
            started: tokio::sync::Notify::new(),
        };
        let cancel = CancellationToken::new();
        let sweep = sweep_hydration_requests_until_cancelled(
            &cancel,
            sweep_transcript_requests(&store, &delivery),
        );
        tokio::pin!(sweep);

        tokio::select! {
            result = &mut sweep => panic!("sweep unexpectedly completed: {result}"),
            _ = delivery.started.notified() => {}
        }
        cancel.cancel();
        let completed = tokio::time::timeout(std::time::Duration::from_secs(1), &mut sweep)
            .await
            .expect("cancelled sweep should return promptly");
        assert!(!completed);
        assert!(store.served.lock().expect("served lock").is_empty());
    }

    #[tokio::test]
    async fn unpaired_peer_is_rejected_without_push() {
        let store = MemoryStore {
            pending: vec![HydrationRequestRow {
                request_key: "peer-1:session-1".into(),
                requester_did: "did:key:requester-1".into(),
                node_did: "did:key:agent-1".into(),
                session_id: "session-1".into(),
            }],
            catalog: HydrationCatalog::default(),
            authorization_current: std::sync::atomic::AtomicBool::new(true),
            authorization_check: None,
            terminal_write_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
            selection_loads: std::sync::atomic::AtomicUsize::new(0),
            served: std::sync::Mutex::new(Vec::new()),
            rejected: std::sync::Mutex::new(Vec::new()),
        };
        let delivery = RecordingDelivery {
            pushed: std::sync::Mutex::new(Vec::new()),
        };
        let outcome = reconcile_hydration_tick(&store, &delivery)
            .await
            .expect("tick");
        assert!(outcome.served.is_empty());
        assert_eq!(
            outcome.rejected,
            BTreeSet::from(["peer-1:session-1".into()])
        );
        assert!(delivery.pushed.lock().expect("pushed lock").is_empty());
    }

    fn foreign_session_row() -> HydrationRequestRow {
        HydrationRequestRow {
            request_key: "peer-1:session-2".into(),
            requester_did: "did:key:requester-1".into(),
            node_did: "did:key:agent-1".into(),
            session_id: "session-2".into(),
        }
    }

    #[tokio::test]
    async fn ownership_mismatch_is_rejected_without_reading_the_transcript() {
        let mut store = admitted_store();
        store.pending = vec![foreign_session_row()];
        let delivery = RecordingDelivery {
            pushed: std::sync::Mutex::new(Vec::new()),
        };
        let outcome = reconcile_hydration_tick(&store, &delivery)
            .await
            .expect("tick");
        assert_eq!(
            outcome.rejected,
            BTreeSet::from(["peer-1:session-2".to_string()])
        );
        assert_eq!(
            *store.rejected.lock().unwrap(),
            vec![(
                "peer-1:session-2".to_string(),
                "session ownership does not match request".to_string()
            )]
        );
        assert_eq!(
            store
                .selection_loads
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert!(delivery.pushed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn rejection_commits_before_another_requests_delivery_finishes() {
        let mut store = admitted_store();
        store.pending.push(foreign_session_row());
        let delivery = BlockingDelivery {
            started: tokio::sync::Notify::new(),
        };
        let tick = reconcile_hydration_tick(&store, &delivery);
        tokio::pin!(tick);
        tokio::select! {
            result = &mut tick => panic!("blocked delivery unexpectedly completed: {result:?}"),
            _ = delivery.started.notified() => {}
        }
        assert_eq!(
            *store.rejected.lock().unwrap(),
            vec![(
                "peer-1:session-2".to_string(),
                "session ownership does not match request".to_string()
            )],
            "the refused request is terminal while the admitted delivery is still in flight"
        );
    }

    #[tokio::test]
    async fn requester_without_active_enrollment_is_rejected_not_left_pending() {
        let mut store = admitted_store();
        store.catalog.verified_active_memberships.clear();
        let delivery = RecordingDelivery {
            pushed: std::sync::Mutex::new(Vec::new()),
        };
        let outcome = reconcile_hydration_tick(&store, &delivery)
            .await
            .expect("missing enrollment is a verdict, not a sweep error");
        assert_eq!(
            outcome.rejected,
            BTreeSet::from(["peer-1:session-1".to_string()])
        );
        assert_eq!(
            store.rejected.lock().unwrap()[0].1,
            "requester membership is not verified active in the selected network"
        );
        assert!(delivery.pushed.lock().unwrap().is_empty());
    }
}
