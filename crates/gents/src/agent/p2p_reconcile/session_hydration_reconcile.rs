//! Server sweep for `SessionHydrationRequest`.
//!
//! Loads pending rows, rebuilds the Lean catalog from pairing/membership/
//! session/transcript documents, runs [`super::session_hydration::decide_hydration`],
//! pushes the exact selected set through existing peer-targeted doc-push
//! machinery, then writes a terminal `served`/`rejected` status.

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

use super::enrollment_reconcile::{EnrollmentAuthorityHandle, EnrollmentAuthorizationFence};
use super::graphql_helpers::{ensure_no_errors, rows};
use super::session_hydration::{
    apply_hydration_delivery, decide_hydration, hydration_collection_name, AppliedPairingRoute,
    HydrationApplyOutcome, HydrationCatalog, HydrationDeliveryResult, HydrationDocument,
    HydrationRequest, HydrationTerminalWriteResult, HydrationVerdict, SessionOwner,
    VerifiedActiveMembership,
};
use super::session_hydration_closure::{
    build_canonical_closure, CanonicalClosureInput, ScopedDocument,
};
use super::templates::{conjunctive_string_eq, decode_pairing_filters};
use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;
use crate::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};

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
        documents: &BTreeSet<HydrationDocument>,
    ) -> Result<()>;
}

#[async_trait]
trait HydrationRequestStore: Send + Sync {
    async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>>;
    async fn load_catalog(&self, request: &HydrationRequest) -> Result<LoadedHydrationCatalog>;
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
struct LoadedHydrationCatalog {
    catalog: HydrationCatalog,
    authorization: EnrollmentAuthorizationFence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HydrationRequestRow {
    pub request_key: String,
    pub requester_did: String,
    pub agent_did: String,
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
    for row in pending {
        let request_key = row.request_key.clone();
        if let Err(error) = process_one_request(store, delivery, &row, &mut outcome).await {
            tracing::warn!(
                request_key = %request_key,
                error = %error,
                "session hydration reconcile failed; continuing sweep"
            );
            if first_error.is_none() {
                first_error =
                    Some(error.context(format!("reconcile session hydration {request_key}")));
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    Ok(outcome)
}

async fn process_one_request(
    store: &dyn HydrationRequestStore,
    delivery: &dyn HydrationDelivery,
    row: &HydrationRequestRow,
    outcome: &mut HydrationTickOutcome,
) -> Result<()> {
    let request = match HydrationRequest::from_row(
        row.request_key.clone(),
        row.requester_did.clone(),
        row.agent_did.clone(),
        row.session_id.clone(),
    ) {
        Ok(request) => request,
        Err(detail) => {
            store.mark_rejected(row, detail).await?;
            outcome.rejected.insert(row.request_key.clone());
            return Ok(());
        }
    };

    let loaded = store
        .load_catalog(&request)
        .await
        .context("load hydration catalog")?;

    let (verdict, delivery_result) = match decide_hydration(&request, &loaded.catalog) {
        HydrationVerdict::Admit(documents) => {
            if !store
                .authorization_is_current(&request, &loaded.authorization)
                .await
                .context("revalidate hydration authorization generation")?
            {
                let detail =
                    "authenticated enrollment authorization changed before hydration delivery";
                store.mark_rejected(row, detail).await?;
                outcome.rejected.insert(request.request_key);
                return Ok(());
            }
            let delivery_result = deliver_with_bounded_retry(delivery, &request, &documents).await;
            if delivery_result == HydrationDeliveryResult::Confirmed {
                if !store
                    .authorization_is_current(&request, &loaded.authorization)
                    .await
                    .context("revalidate hydration authorization at terminal commit")?
                {
                    let detail =
                        "authenticated enrollment authorization changed before hydration commit";
                    store.mark_rejected(row, detail).await?;
                    outcome.rejected.insert(request.request_key);
                    return Ok(());
                }
            }
            (HydrationVerdict::Admit(documents), delivery_result)
        }
        HydrationVerdict::Reject(detail) => (
            HydrationVerdict::Reject(detail),
            HydrationDeliveryResult::Confirmed,
        ),
    };

    match (verdict, delivery_result) {
        (HydrationVerdict::Admit(documents), HydrationDeliveryResult::Indeterminate) => {
            let modeled = apply_hydration_delivery(
                HydrationVerdict::Admit(documents),
                HydrationDeliveryResult::Indeterminate,
                HydrationTerminalWriteResult::NotAttempted,
            );
            debug_assert!(matches!(
                modeled,
                HydrationApplyOutcome::PendingAfterIndeterminateDelivery { .. }
            ));
        }
        (HydrationVerdict::Admit(documents), HydrationDeliveryResult::Confirmed) => {
            let terminal_write = store.mark_served(row, &documents).await;
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
                }
                (Err(error), HydrationApplyOutcome::PendingAfterTerminalWriteFailure { .. }) => {
                    return Err(error).context("mark session hydration served");
                }
                _ => unreachable!("hydration model diverged from served receipt commit"),
            }
        }
        (HydrationVerdict::Reject(detail), delivery_result) => {
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
                (Ok(()), HydrationApplyOutcome::Rejected { .. }) => {
                    outcome.rejected.insert(request.request_key);
                }
                (Err(error), HydrationApplyOutcome::PendingAfterTerminalWriteFailure { .. }) => {
                    return Err(error).context("mark session hydration rejected");
                }
                _ => unreachable!("hydration model diverged from rejected receipt commit"),
            }
        }
    }
    Ok(())
}

async fn deliver_with_bounded_retry(
    delivery: &dyn HydrationDelivery,
    request: &HydrationRequest,
    documents: &BTreeSet<HydrationDocument>,
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
    identity: Arc<dyn AgentIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    let hydration_collection_id = node
        .get_collection("SessionHydrationRequest")?
        .context("SessionHydrationRequest schema is not installed")?
        .collection_id;
    let store = GraphqlHydrationStore {
        node: node.clone(),
        enrollment,
        identity,
    };
    let delivery: Arc<dyn HydrationDelivery> =
        Arc::new(EmbeddedHydrationDelivery { node: node.clone() });
    let mut subscription = node.subscribe_document_changes();
    let sweep_interval = super::intervals::sweep_interval();
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + sweep_interval, sweep_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut subscription_open = true;

    if !sweep_hydration_requests_until_cancelled(&store, delivery.as_ref(), &cancel).await {
        return Ok(());
    }
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => {
                if !sweep_hydration_requests_until_cancelled(&store, delivery.as_ref(), &cancel).await {
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
                } else if !batch
                    .changes
                    .iter()
                    .any(|change| change.collection_id == hydration_collection_id)
                {
                    continue;
                }
                if !sweep_hydration_requests_until_cancelled(&store, delivery.as_ref(), &cancel).await {
                    return Ok(());
                }
            }
        }
    }
}

async fn sweep_hydration_requests_until_cancelled(
    store: &dyn HydrationRequestStore,
    delivery: &dyn HydrationDelivery,
    cancel: &CancellationToken,
) -> bool {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = sweep_hydration_requests(store, delivery) => true,
    }
}

async fn sweep_hydration_requests(
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
    identity: Arc<dyn AgentIdentity>,
}

struct EmbeddedHydrationDelivery {
    node: Arc<EmbeddedNode>,
}

#[async_trait]
impl HydrationDelivery for EmbeddedHydrationDelivery {
    async fn push_documents_to_peer(
        &self,
        peer_id: &str,
        documents: &BTreeSet<HydrationDocument>,
    ) -> Result<()> {
        super::embedded_impl::push_documents_to_peer(&self.node, peer_id, documents).await
    }
}

#[derive(Deserialize)]
struct PendingRow {
    request_key: Option<String>,
    requester_did: Option<String>,
    agent_did: Option<String>,
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct DesiredPairingRow {
    peer_id: Option<String>,
    agent_did: Option<String>,
}

#[derive(Deserialize)]
struct AppliedPairingRow {
    peer_id: Option<String>,
    replicator_filter: Option<String>,
}

#[derive(Deserialize)]
struct SessionRow {
    session_id: Option<String>,
    requester_did: Option<String>,
    agent_did: Option<String>,
}

#[derive(Deserialize)]
struct TranscriptRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
    requester_did: Option<String>,
    agent_did: Option<String>,
    session_id: Option<String>,
}

#[async_trait]
impl HydrationRequestStore for GraphqlHydrationStore {
    async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>> {
        let agent_did = escape_graphql_string(self.identity.did());
        let query = format!(
            r#"{{
            SessionHydrationRequest(filter: {{ status: {{ _eq: "pending" }}, agent_did: {{ _eq: "{agent_did}" }} }}) {{
                request_key
                requester_did
                agent_did
                session_id
            }}
        }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query SessionHydrationRequest pending rows")?;
        Ok(rows::<PendingRow>(&response, "SessionHydrationRequest")?
            .into_iter()
            .filter_map(|row| {
                Some(HydrationRequestRow {
                    request_key: row.request_key.filter(|value| !value.is_empty())?,
                    requester_did: row.requester_did.unwrap_or_default(),
                    agent_did: row.agent_did.unwrap_or_default(),
                    session_id: row.session_id.unwrap_or_default(),
                })
            })
            .collect())
    }

    async fn load_catalog(&self, request: &HydrationRequest) -> Result<LoadedHydrationCatalog> {
        let authorization = self
            .enrollment
            .fresh_authorization(&request.requester_did, &request.peer_id)
            .await
            .context("load fresh authenticated enrollment authority for hydration")?
            .context("requester has no active authenticated enrollment")?;
        let network_id = authorization.network_id.clone();
        let session_id = escape_graphql_string(&request.session_id);
        let peer_id = escape_graphql_string(&request.peer_id);
        let agent_did = escape_graphql_string(&request.agent_did);
        let requester_did = escape_graphql_string(&request.requester_did);
        let query = hydration_catalog_query(&session_id, &peer_id, &agent_did, &requester_did);
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query session hydration catalog")?;

        let desired_agents = rows::<DesiredPairingRow>(&response, "PeerPairingDesired")?
            .into_iter()
            .filter_map(|row| {
                Some((
                    row.peer_id.filter(|value| !value.is_empty())?,
                    row.agent_did.filter(|value| !value.is_empty())?,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let applied_pairing_routes = rows::<AppliedPairingRow>(&response, "PeerPairingApplied")?
            .into_iter()
            .filter_map(|row| applied_pairing_route(row, &desired_agents))
            .collect();
        let sessions = rows::<SessionRow>(&response, "AgentSession")?
            .into_iter()
            .filter_map(|row| {
                Some(SessionOwner {
                    session_id: row.session_id.filter(|value| !value.is_empty())?,
                    requester_did: row.requester_did.unwrap_or_default(),
                    agent_did: row.agent_did.unwrap_or_default(),
                })
            })
            .collect();

        let mut root_headers = rows::<serde_json::Value>(&response, "AgentMessage")?
            .iter()
            .map(decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?;
        let mut root_header_ids = root_headers
            .iter()
            .map(|row| row.doc_id.clone())
            .collect::<Vec<_>>();
        let mut terminal_request_ids = BTreeSet::new();
        for request_row in rows::<gents_protocol::row::AgentRequestRow>(&response, "AgentRequest")?
        {
            if request_row.is_terminal() {
                terminal_request_ids.insert(
                    request_row
                        .doc_id
                        .clone()
                        .context("terminal hydration request omitted physical id")?,
                );
                match request_row
                    .terminal_output
                    .context("terminal hydration request omitted terminal_output")?
                {
                    gents_protocol::output::TerminalOutput::Message { message_doc_id } => {
                        let selected = root_headers
                            .iter()
                            .find(|row| row.doc_id == message_doc_id)
                            .context("terminal hydration selection is missing its exact header")?;
                        anyhow::ensure!(
                            selected.message.session_id == request.session_id
                                && selected.message.request_doc_id == request_row.doc_id,
                            "terminal hydration selection does not belong to the exact request"
                        );
                        anyhow::ensure!(
                            selected.message.role
                                == gents_protocol::output::MessageRole::Assistant
                                && matches!(
                                    selected.message.publication,
                                    gents_protocol::output::MessagePublication::RequestExecution {
                                        ..
                                    } | gents_protocol::output::MessagePublication::RequestRecovery {
                                        ..
                                    }
                                ),
                            "terminal hydration selection is not an eligible assistant header"
                        );
                        root_header_ids.push(message_doc_id);
                    }
                    gents_protocol::output::TerminalOutput::NoMessage => {}
                }
            }
        }
        root_header_ids.sort();
        root_header_ids.dedup();
        load_origin_headers(
            self.node.as_ref(),
            &mut root_headers,
            &request.agent_did,
            &request.requester_did,
        )
        .await?;
        let mut output_segments = rows::<serde_json::Value>(&response, "AgentOutputSegment")?
            .iter()
            .map(decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?;
        load_referenced_segments(
            self.node.as_ref(),
            &root_headers,
            &mut output_segments,
            &request.agent_did,
            &request.requester_did,
        )
        .await?;
        let mut bases = Vec::new();
        for collection in [
            gents_protocol::session_hydration::SessionHydrationCollection::AgentRequest,
            gents_protocol::session_hydration::SessionHydrationCollection::AgentToolCall,
            gents_protocol::session_hydration::SessionHydrationCollection::CompactionEntry,
        ] {
            let name = hydration_collection_name(collection);
            bases.extend(
                rows::<TranscriptRow>(&response, name)?
                    .into_iter()
                    .filter_map(|row| {
                        Some(ScopedDocument {
                            collection,
                            doc_id: row.doc_id.filter(|value| !value.is_empty())?,
                            requester_did: row.requester_did.unwrap_or_default(),
                            agent_did: row.agent_did.unwrap_or_default(),
                            session_id: row.session_id.unwrap_or_default(),
                        })
                    }),
            );
        }
        load_referenced_bases(
            self.node.as_ref(),
            &root_headers,
            &output_segments,
            &mut bases,
            &request.agent_did,
            &request.requester_did,
        )
        .await?;
        let mut documents = build_canonical_closure(CanonicalClosureInput {
            root_header_ids: &root_header_ids,
            headers: &root_headers,
            segments: &output_segments,
            base_documents: &bases,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            agent_did: &request.agent_did,
            requester_did: Some(&request.requester_did),
            session_id: &request.session_id,
        })
        .context("build authorized canonical hydration closure")?;
        documents.extend(
            bases
                .iter()
                .filter(|row| row.session_id == request.session_id)
                .cloned()
                .map(|row| HydrationDocument {
                    collection: row.collection,
                    doc_id: row.doc_id,
                    requester_did: row.requester_did,
                    agent_did: row.agent_did,
                    session_id: row.session_id,
                }),
        );
        for request_id in terminal_request_ids {
            let row = bases
                .iter()
                .find(|row| {
                    row.collection
                        == gents_protocol::session_hydration::SessionHydrationCollection::AgentRequest
                        && row.doc_id == request_id
                })
                .context("terminal hydration request missing from authorized base rows")?;
            documents.insert(HydrationDocument {
                collection: row.collection,
                doc_id: row.doc_id.clone(),
                requester_did: row.requester_did.clone(),
                agent_did: row.agent_did.clone(),
                session_id: row.session_id.clone(),
            });
        }
        let authorized_reference_closure = documents
            .iter()
            .filter(|document| document.session_id != request.session_id)
            .map(|document| SessionHydrationDocumentKey {
                collection: document.collection,
                doc_id: document.doc_id.clone(),
            })
            .collect();

        Ok(LoadedHydrationCatalog {
            catalog: HydrationCatalog {
                applied_pairing_routes,
                selected_network_id: network_id.clone(),
                verified_active_memberships: BTreeSet::from([VerifiedActiveMembership {
                    network_id,
                    member_did: authorization.member_did.clone(),
                }]),
                sessions,
                documents,
                authorized_reference_closure,
            },
            authorization,
        })
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
                collection: document.collection.clone(),
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

async fn load_origin_headers(
    node: &EmbeddedNode,
    headers: &mut Vec<crate::session::canonical_rows::TranscriptMessageRow>,
    agent_did: &str,
    requester_did: &str,
) -> Result<()> {
    let mut loaded = headers
        .iter()
        .map(|row| row.doc_id.clone())
        .collect::<BTreeSet<_>>();
    loop {
        let next = headers
            .iter()
            .find_map(|row| match &row.message.publication {
                gents_protocol::output::MessagePublication::Fork {
                    origin_message_doc_id,
                } if !loaded.contains(origin_message_doc_id) => Some(origin_message_doc_id.clone()),
                _ => None,
            });
        let Some(doc_id) = next else {
            return Ok(());
        };
        let id = escape_graphql_string(&doc_id);
        let agent = escape_graphql_string(agent_did);
        let requester = escape_graphql_string(requester_did);
        let response = node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{id}" }}, agent_did: {{ _eq: "{agent}" }}, requester_did: {{ _eq: "{requester}" }} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#
        )).await;
        ensure_no_errors(&response, "query exact authorized hydration origin")?;
        let decoded = rows::<serde_json::Value>(&response, "AgentMessage")?
            .iter()
            .map(decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?;
        anyhow::ensure!(
            decoded.len() == 1,
            "hydration origin is missing, denied, or conflicting"
        );
        loaded.insert(doc_id);
        headers.extend(decoded);
    }
}

async fn load_referenced_segments(
    node: &EmbeddedNode,
    headers: &[crate::session::canonical_rows::TranscriptMessageRow],
    segments: &mut Vec<crate::session::canonical_rows::OutputSegmentRow>,
    agent_did: &str,
    requester_did: &str,
) -> Result<()> {
    let request_ids = headers
        .iter()
        .filter_map(|row| row.message.request_doc_id.clone())
        .chain(headers.iter().flat_map(|row| {
            row.message
                .payload_references()
                .into_iter()
                .filter_map(|reference| {
                    segments
                        .iter()
                        .find(|segment| segment.doc_id == reference.close_doc_id)
                        .map(|segment| segment.segment.request_doc_id.clone())
                })
        }))
        .collect::<BTreeSet<_>>();
    let agent = escape_graphql_string(agent_did);
    let requester = escape_graphql_string(requester_did);
    for request_id in request_ids {
        let request_id = escape_graphql_string(&request_id);
        let response = node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request_id}" }}, agent_did: {{ _eq: "{agent}" }}, requester_did: {{ _eq: "{requester}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
        )).await;
        ensure_no_errors(&response, "query authorized hydration output extent")?;
        for row in rows::<serde_json::Value>(&response, "AgentOutputSegment")?
            .iter()
            .map(decode_output_segment_row)
        {
            let row = row?;
            if !segments.iter().any(|known| known.doc_id == row.doc_id) {
                segments.push(row);
            }
        }
    }
    Ok(())
}

async fn load_referenced_bases(
    node: &EmbeddedNode,
    headers: &[crate::session::canonical_rows::TranscriptMessageRow],
    segments: &[crate::session::canonical_rows::OutputSegmentRow],
    bases: &mut Vec<ScopedDocument>,
    agent_did: &str,
    requester_did: &str,
) -> Result<()> {
    use gents_protocol::output::{MessageBlock, MessagePublication, OutputSource};
    use gents_protocol::session_hydration::SessionHydrationCollection;
    let mut wanted = BTreeSet::new();
    for row in headers {
        if let Some(id) = &row.message.request_doc_id {
            wanted.insert((SessionHydrationCollection::AgentRequest, id.clone()));
        }
        if let MessagePublication::ToolDelivery { tool_call_doc_id } = &row.message.publication {
            wanted.insert((
                SessionHydrationCollection::AgentToolCall,
                tool_call_doc_id.clone(),
            ));
        }
        for block in &row.message.blocks {
            if let MessageBlock::ToolCall {
                tool_call_doc_id, ..
            }
            | MessageBlock::ToolResult {
                tool_call_doc_id, ..
            } = block
            {
                wanted.insert((
                    SessionHydrationCollection::AgentToolCall,
                    tool_call_doc_id.clone(),
                ));
            }
        }
    }
    for row in segments {
        if let OutputSource::ToolCall { tool_call_doc_id } = &row.segment.source {
            wanted.insert((
                SessionHydrationCollection::AgentToolCall,
                tool_call_doc_id.clone(),
            ));
        }
    }
    let agent = escape_graphql_string(agent_did);
    let requester = escape_graphql_string(requester_did);
    for (collection, doc_id) in wanted {
        if bases
            .iter()
            .any(|row| row.collection == collection && row.doc_id == doc_id)
        {
            continue;
        }
        let name = hydration_collection_name(collection);
        let id = escape_graphql_string(&doc_id);
        let response = node.execute(&format!(
            r#"{{ {name}(filter: {{ _docID: {{ _eq: "{id}" }}, agent_did: {{ _eq: "{agent}" }}, requester_did: {{ _eq: "{requester}" }} }}) {{ _docID requester_did agent_did session_id }} }}"#
        )).await;
        ensure_no_errors(&response, "query exact authorized hydration provenance")?;
        let found = rows::<TranscriptRow>(&response, name)?;
        anyhow::ensure!(
            found.len() == 1,
            "hydration provenance is missing, denied, or conflicting"
        );
        let row = found.into_iter().next().expect("length checked");
        bases.push(ScopedDocument {
            collection,
            doc_id: row
                .doc_id
                .context("hydration provenance omitted physical id")?,
            requester_did: row.requester_did.unwrap_or_default(),
            agent_did: row.agent_did.unwrap_or_default(),
            session_id: row.session_id.unwrap_or_default(),
        });
    }
    Ok(())
}

impl GraphqlHydrationStore {
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
            agent_did: request.agent_did.clone(),
            session_id: request.session_id.clone(),
            status: status.to_string(),
            status_detail: status_detail.to_string(),
            served_manifest,
            processed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            signer_did: self.identity.did().to_string(),
            signature: Vec::new(),
        };
        anyhow::ensure!(
            receipt.agent_did == receipt.signer_did,
            "hydration reconciler cannot sign for another agent"
        );
        receipt.signature = self.identity.sign(&receipt.signing_payload()?).await?;
        receipt.validate_shape()?;
        Ok(receipt)
    }
}

fn hydration_catalog_query(
    session_id: &str,
    peer_id: &str,
    agent_did: &str,
    requester_did: &str,
) -> String {
    format!(
        r#"{{
            PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }}, source: {{ _eq: "enrollment" }} }}) {{
                peer_id agent_did
            }}
            PeerPairingApplied(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
                peer_id replicator_filter
            }}
            AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                session_id requester_did agent_did
            }}
            AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{
                _docID request_id requester_did agent_did session_id lifecycle_state terminal_output
            }}
            AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{
                {AGENT_MESSAGE_FIELDS}
            }}
            AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{
                _docID requester_did agent_did session_id
            }}
            AgentOutputSegment(filter: {{ session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{
                {AGENT_OUTPUT_SEGMENT_FIELDS}
            }}
            CompactionEntry(filter: {{ session_id: {{ _eq: "{session_id}" }}, agent_did: {{ _eq: "{agent_did}" }}, requester_did: {{ _eq: "{requester_did}" }} }}) {{
                _docID requester_did agent_did session_id
            }}
        }}"#
    )
}

fn applied_pairing_route(
    row: AppliedPairingRow,
    desired_agents: &BTreeMap<String, String>,
) -> Option<AppliedPairingRoute> {
    let peer_id = row.peer_id.filter(|value| !value.is_empty())?;
    let desired_agent = desired_agents.get(&peer_id)?;
    let filters = decode_pairing_filters(row.replicator_filter.as_deref()?).ok()?;
    let request_filter = filters.get("SessionHydrationRequest")?;
    let requester_did = conjunctive_string_eq(request_filter, "requester_did")?;
    let applied_agent = conjunctive_string_eq(request_filter, "agent_did")?;
    if applied_agent != desired_agent {
        return None;
    }
    Some(AppliedPairingRoute {
        peer_id,
        requester_did: requester_did.to_string(),
        agent_did: applied_agent.to_string(),
    })
}

fn terminal_mutation(
    request: &HydrationRequestRow,
    receipt: &SessionHydrationReceipt,
) -> Result<String> {
    let request_key = escape_graphql_string(&request.request_key);
    let requester_did = escape_graphql_string(&request.requester_did);
    let agent_did = escape_graphql_string(&request.agent_did);
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
                    agent_did: {{ _eq: "{agent_did}" }},
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
    use crate::agent::p2p_reconcile::templates::{combine_filters, equality_filter};

    struct MemoryStore {
        pending: Vec<HydrationRequestRow>,
        catalog: HydrationCatalog,
        authorization_current: std::sync::atomic::AtomicBool,
        authorization_check: Option<Arc<AuthorizationCheckBarrier>>,
        terminal_write_failures_remaining: std::sync::atomic::AtomicUsize,
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
        async fn load_catalog(
            &self,
            _request: &HydrationRequest,
        ) -> Result<LoadedHydrationCatalog> {
            Ok(LoadedHydrationCatalog {
                catalog: self.catalog.clone(),
                authorization: test_authorization_fence(),
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
            documents: &BTreeSet<HydrationDocument>,
        ) -> Result<()> {
            self.pushed
                .lock()
                .expect("pushed lock")
                .push((peer_id.to_string(), documents.clone()));
            Ok(())
        }
    }

    #[async_trait]
    impl HydrationDelivery for FailOnceDelivery {
        async fn push_documents_to_peer(
            &self,
            peer_id: &str,
            documents: &BTreeSet<HydrationDocument>,
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
                .push((peer_id.to_string(), documents.clone()));
            Ok(())
        }
    }

    #[async_trait]
    impl HydrationDelivery for FailingDelivery {
        async fn push_documents_to_peer(
            &self,
            peer_id: &str,
            documents: &BTreeSet<HydrationDocument>,
        ) -> Result<()> {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.pushed
                .lock()
                .expect("pushed lock")
                .push((peer_id.to_string(), documents.clone()));
            anyhow::bail!("transport outcome unknown after dispatch")
        }
    }

    #[async_trait]
    impl HydrationDelivery for BlockingDelivery {
        async fn push_documents_to_peer(
            &self,
            _peer_id: &str,
            _documents: &BTreeSet<HydrationDocument>,
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
            agent_did: "did:key:agent-1".into(),
            session_id: "session-1".into(),
        };
        MemoryStore {
            pending: vec![HydrationRequestRow {
                request_key: "peer-1:session-1".into(),
                requester_did: "did:key:requester-1".into(),
                agent_did: "did:key:agent-1".into(),
                session_id: "session-1".into(),
            }],
            catalog: HydrationCatalog {
                applied_pairing_routes: BTreeSet::from([AppliedPairingRoute {
                    peer_id: "peer-1".into(),
                    requester_did: "did:key:requester-1".into(),
                    agent_did: "did:key:agent-1".into(),
                }]),
                selected_network_id: "network-1".into(),
                verified_active_memberships: BTreeSet::from([VerifiedActiveMembership {
                    network_id: "network-1".into(),
                    member_did: "did:key:requester-1".into(),
                }]),
                sessions: BTreeSet::from([SessionOwner {
                    session_id: "session-1".into(),
                    requester_did: "did:key:requester-1".into(),
                    agent_did: "did:key:agent-1".into(),
                }]),
                documents: BTreeSet::from([document]),
                authorized_reference_closure: BTreeSet::new(),
            },
            authorization_current: std::sync::atomic::AtomicBool::new(true),
            authorization_check: None,
            terminal_write_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
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
            owner_agent: "did:key:agent-1".into(),
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
        // The catalog was admitted under the old generation. Commit the
        // revocation while the fresh owner-command recheck is paused.
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
        let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let query = hydration_catalog_query(
            "session-1",
            "peer-1",
            "did:key:agent-1",
            "did:key:requester-1",
        );
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "real hydration catalog query").unwrap();
        assert!(query.contains("AgentOutputSegment"));
        assert!(!query.contains("AgentResponse"));
        assert!(!query.contains("AgentToolResult"));
    }

    #[test]
    fn applied_pairing_requires_exact_requester_and_desired_agent() {
        let filter = combine_filters(
            equality_filter("requester_did", "did:key:requester-1"),
            equality_filter("agent_did", "did:key:agent-1"),
        );
        let raw = serde_json::to_string(&BTreeMap::from([(
            "SessionHydrationRequest".to_string(),
            filter,
        )]))
        .expect("serialize filter");
        let desired = BTreeMap::from([("peer-1".to_string(), "did:key:agent-1".to_string())]);
        let route = applied_pairing_route(
            AppliedPairingRow {
                peer_id: Some("peer-1".into()),
                replicator_filter: Some(raw.clone()),
            },
            &desired,
        )
        .expect("exact route");
        assert_eq!(route.requester_did, "did:key:requester-1");

        let wrong_agent = BTreeMap::from([("peer-1".to_string(), "did:key:agent-2".to_string())]);
        assert!(applied_pairing_route(
            AppliedPairingRow {
                peer_id: Some("peer-1".into()),
                replicator_filter: Some(raw),
            },
            &wrong_agent,
        )
        .is_none());
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_delivery() {
        let store = admitted_store();
        let delivery = BlockingDelivery {
            started: tokio::sync::Notify::new(),
        };
        let cancel = CancellationToken::new();
        let sweep = sweep_hydration_requests_until_cancelled(&store, &delivery, &cancel);
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
                agent_did: "did:key:agent-1".into(),
                session_id: "session-1".into(),
            }],
            catalog: HydrationCatalog::default(),
            authorization_current: std::sync::atomic::AtomicBool::new(true),
            authorization_check: None,
            terminal_write_failures_remaining: std::sync::atomic::AtomicUsize::new(0),
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
}
