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
use defra_node::{EmbeddedNode, EventName};
use gents_protocol::session_hydration::{
    canonical_manifest_json, SessionHydrationDocumentKey, SessionHydrationReceipt,
    SESSION_HYDRATION_RECEIPT_VERSION,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::enrollment_reconcile::{EnrollmentAuthorityHandle, EnrollmentAuthorizationFence};
use super::graphql_helpers::{ensure_no_errors, rows};
use super::session_hydration::{
    decide_hydration, AppliedPairingRoute, HydrationCatalog, HydrationDocument, HydrationRequest,
    HydrationVerdict, SessionOwner, VerifiedActiveMembership, HYDRATION_COLLECTIONS,
};
use super::templates::{conjunctive_string_eq, decode_pairing_filters};
use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

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

    match decide_hydration(&request, &loaded.catalog) {
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
            delivery
                .push_documents_to_peer(&request.peer_id, &documents)
                .await
                .context("push admitted hydration documents")?;
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
            store
                .mark_served(row, &documents)
                .await
                .context("mark session hydration served")?;
            outcome.served.insert(request.request_key);
        }
        HydrationVerdict::Reject(detail) => {
            store
                .mark_rejected(row, detail)
                .await
                .context("mark session hydration rejected")?;
            outcome.rejected.insert(request.request_key);
        }
    }
    Ok(())
}

pub async fn run_session_hydration_reconciler(
    node: Arc<EmbeddedNode>,
    enrollment: EnrollmentAuthorityHandle,
    identity: Arc<dyn AgentIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlHydrationStore {
        node: node.clone(),
        enrollment,
        identity,
    };
    let delivery: Arc<dyn HydrationDelivery> =
        Arc::new(EmbeddedHydrationDelivery { node: node.clone() });
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

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
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("session-hydration reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "session-hydration reconciler update subscription dropped messages");
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
        let query = hydration_catalog_query(&session_id, &peer_id);
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

        let mut documents = BTreeSet::new();
        for collection in HYDRATION_COLLECTIONS {
            for row in rows::<TranscriptRow>(&response, collection)? {
                if let Some(document) = transcript_document(
                    collection,
                    row.doc_id,
                    row.requester_did.as_deref(),
                    row.agent_did.as_deref(),
                    row.session_id.as_deref(),
                ) {
                    documents.insert(document);
                }
            }
        }

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
        let response = crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "mark SessionHydrationRequest served",
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
        let response = crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "mark SessionHydrationRequest rejected",
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

fn hydration_catalog_query(session_id: &str, peer_id: &str) -> String {
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
            AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                _docID requester_did agent_did session_id
            }}
            AgentResponse(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                _docID requester_did agent_did session_id
            }}
            AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                _docID requester_did agent_did session_id
            }}
            AgentToolCall(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                _docID requester_did agent_did session_id
            }}
            AgentToolResult(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                _docID requester_did agent_did session_id
            }}
            CompactionEntry(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
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

fn transcript_document(
    collection: &str,
    doc_id: Option<String>,
    requester_did: Option<&str>,
    agent_did: Option<&str>,
    session_id: Option<&str>,
) -> Option<HydrationDocument> {
    Some(HydrationDocument {
        collection: collection.to_string(),
        doc_id: doc_id.filter(|value| !value.is_empty())?,
        requester_did: requester_did.unwrap_or_default().to_string(),
        agent_did: agent_did.unwrap_or_default().to_string(),
        session_id: session_id.unwrap_or_default().to_string(),
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
            self.served
                .lock()
                .expect("served lock")
                .push((request.request_key.clone(), documents.len()));
            Ok(())
        }
        async fn mark_rejected(&self, request: &HydrationRequestRow, detail: &str) -> Result<()> {
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
            collection: "AgentMessage".into(),
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
            },
            authorization_current: std::sync::atomic::AtomicBool::new(true),
            authorization_check: None,
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
        let query = hydration_catalog_query("session-1", "peer-1");
        let response = node.execute(&query).await;
        ensure_no_errors(&response, "real hydration catalog query").unwrap();
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
