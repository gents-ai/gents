//! Server sweep for `SessionHydrationRequest`.
//!
//! Loads pending rows, rebuilds the Lean catalog from pairing/membership/
//! session/transcript documents, runs [`super::session_hydration::decide_hydration`],
//! pushes the exact selected set through existing peer-targeted doc-push
//! machinery, then writes a terminal `served`/`rejected` status.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{SecondsFormat, Utc};
use defra_node::{EmbeddedNode, EventName};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;

use super::graphql_helpers::{ensure_no_errors, rows};
use super::session_hydration::{
    decide_hydration, HydrationCatalog, HydrationDocument, HydrationRequest, HydrationVerdict,
    SessionOwner, HYDRATION_COLLECTIONS,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HydrationTickOutcome {
    pub served: BTreeSet<String>,
    pub rejected: BTreeSet<String>,
}

#[async_trait]
pub trait HydrationDelivery: Send + Sync {
    async fn push_documents_to_peer(
        &self,
        peer_id: &str,
        documents: &BTreeSet<HydrationDocument>,
    ) -> Result<()>;
}

#[async_trait]
pub trait HydrationRequestStore: Send + Sync {
    async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>>;
    async fn load_catalog(&self, request: &HydrationRequest) -> Result<HydrationCatalog>;
    async fn mark_served(&self, request_key: &str, served_doc_count: usize) -> Result<()>;
    async fn mark_rejected(&self, request_key: &str, detail: &str) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydrationRequestRow {
    pub request_key: String,
    pub requester_did: String,
    pub agent_did: String,
    pub session_id: String,
}

pub async fn reconcile_hydration_tick(
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
            store.mark_rejected(&row.request_key, detail).await?;
            outcome.rejected.insert(row.request_key.clone());
            return Ok(());
        }
    };

    let catalog = store
        .load_catalog(&request)
        .await
        .context("load hydration catalog")?;

    match decide_hydration(&request, &catalog) {
        HydrationVerdict::Admit(documents) => {
            delivery
                .push_documents_to_peer(&request.peer_id, &documents)
                .await
                .context("push admitted hydration documents")?;
            store
                .mark_served(&request.request_key, documents.len())
                .await
                .context("mark session hydration served")?;
            outcome.served.insert(request.request_key);
        }
        HydrationVerdict::Reject(detail) => {
            store
                .mark_rejected(&request.request_key, detail)
                .await
                .context("mark session hydration rejected")?;
            outcome.rejected.insert(request.request_key);
        }
    }
    Ok(())
}

pub async fn run_session_hydration_reconciler(
    node: Arc<EmbeddedNode>,
    delivery: Arc<dyn HydrationDelivery>,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlHydrationStore { node: node.clone() };
    let mut subscription = node.subscribe(&[EventName::Update]);
    let mut interval = tokio::time::interval(super::intervals::sweep_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    sweep_hydration_requests(&store, delivery.as_ref()).await;
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            _ = interval.tick() => sweep_hydration_requests(&store, delivery.as_ref()).await,
            message = subscription.recv() => {
                if message.is_none() {
                    tracing::warn!("session-hydration reconciler update subscription closed; continuing with periodic sweeps");
                    continue;
                }
                let dropped = subscription.check_and_reset_dropped();
                if dropped > 0 {
                    tracing::warn!(dropped, "session-hydration reconciler update subscription dropped messages");
                }
                sweep_hydration_requests(&store, delivery.as_ref()).await;
            }
        }
    }
}

async fn sweep_hydration_requests(store: &GraphqlHydrationStore, delivery: &dyn HydrationDelivery) {
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

pub struct GraphqlHydrationStore {
    node: Arc<EmbeddedNode>,
}

pub struct EmbeddedHydrationDelivery {
    node: Arc<EmbeddedNode>,
}

impl EmbeddedHydrationDelivery {
    pub fn new(node: Arc<EmbeddedNode>) -> Self {
        Self { node }
    }
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
struct PeerIdRow {
    peer_id: Option<String>,
}

#[derive(Deserialize)]
struct MemberRow {
    member_did: Option<String>,
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

#[derive(Deserialize)]
struct RequestIdRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
    request_id: Option<String>,
    requester_did: Option<String>,
    agent_did: Option<String>,
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct ApprovalRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
    request_id: Option<String>,
    agent_did: Option<String>,
}

#[async_trait]
impl HydrationRequestStore for GraphqlHydrationStore {
    async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>> {
        let query = r#"{
            SessionHydrationRequest(filter: { status: { _eq: "pending" } }) {
                request_key
                requester_did
                agent_did
                session_id
            }
        }"#;
        let response = self.node.execute(query).await;
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

    async fn load_catalog(&self, request: &HydrationRequest) -> Result<HydrationCatalog> {
        let session_id = escape_graphql_string(&request.session_id);
        let query = format!(
            r#"{{
                PeerPairingApplied {{ peer_id }}
                NetworkMembership(filter: {{ status: {{ _eq: "active" }} }}) {{ member_did }}
                AgentSession(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                    session_id requester_did agent_did
                }}
                AgentRequest(filter: {{ session_id: {{ _eq: "{session_id}" }} }}) {{
                    _docID request_id requester_did agent_did session_id
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
                AgentToolApproval {{ _docID request_id agent_did }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query session hydration catalog")?;

        let paired_peer_ids = rows::<PeerIdRow>(&response, "PeerPairingApplied")?
            .into_iter()
            .filter_map(|row| row.peer_id.filter(|value| !value.is_empty()))
            .collect();
        let active_member_dids = rows::<MemberRow>(&response, "NetworkMembership")?
            .into_iter()
            .filter_map(|row| row.member_did.filter(|value| !value.is_empty()))
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

        let request_rows = rows::<RequestIdRow>(&response, "AgentRequest")?;
        let session_request_ids: BTreeSet<String> = request_rows
            .iter()
            .filter(|row| {
                row.session_id.as_deref() == Some(request.session_id.as_str())
                    && row.requester_did.as_deref() == Some(request.requester_did.as_str())
                    && row.agent_did.as_deref() == Some(request.agent_did.as_str())
            })
            .filter_map(|row| row.request_id.clone())
            .collect();

        let mut documents = BTreeSet::new();
        for collection in HYDRATION_COLLECTIONS {
            if *collection == "AgentToolApproval" {
                continue;
            }
            let field = if *collection == "AgentRequest" {
                "AgentRequest"
            } else {
                *collection
            };
            if field == "AgentRequest" {
                for row in &request_rows {
                    if let Some(document) = transcript_document(
                        collection,
                        row.doc_id.clone(),
                        row.requester_did.as_deref(),
                        row.agent_did.as_deref(),
                        row.session_id.as_deref(),
                    ) {
                        documents.insert(document);
                    }
                }
                continue;
            }
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

        for row in rows::<ApprovalRow>(&response, "AgentToolApproval")? {
            let request_id = row.request_id.unwrap_or_default();
            if !session_request_ids.contains(&request_id) {
                continue;
            }
            let Some(doc_id) = row.doc_id.filter(|value| !value.is_empty()) else {
                continue;
            };
            documents.insert(HydrationDocument {
                collection: "AgentToolApproval".into(),
                doc_id,
                requester_did: request.requester_did.clone(),
                agent_did: row.agent_did.unwrap_or_else(|| request.agent_did.clone()),
                session_id: request.session_id.clone(),
            });
        }

        Ok(HydrationCatalog {
            paired_peer_ids,
            active_member_dids,
            sessions,
            documents,
        })
    }

    async fn mark_served(&self, request_key: &str, served_doc_count: usize) -> Result<()> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = mark_served_mutation(request_key, served_doc_count, &now);
        crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "mark SessionHydrationRequest served",
        )
        .await
        .map(|_| ())
    }

    async fn mark_rejected(&self, request_key: &str, detail: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        let mutation = mark_rejected_mutation(request_key, detail, &now);
        crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "mark SessionHydrationRequest rejected",
        )
        .await
        .map(|_| ())
    }
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

fn mark_served_mutation(request_key: &str, served_doc_count: usize, now: &str) -> String {
    let request_key = escape_graphql_string(request_key);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            update_SessionHydrationRequest(
                filter: {{ request_key: {{ _eq: "{request_key}" }} }},
                input: {{
                    status: "served",
                    status_detail: "",
                    served_doc_count: {served_doc_count},
                    processed_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

fn mark_rejected_mutation(request_key: &str, detail: &str, now: &str) -> String {
    let request_key = escape_graphql_string(request_key);
    let detail = escape_graphql_string(detail);
    let now = escape_graphql_string(now);
    format!(
        r#"mutation {{
            update_SessionHydrationRequest(
                filter: {{ request_key: {{ _eq: "{request_key}" }} }},
                input: {{
                    status: "rejected",
                    status_detail: "{detail}",
                    served_doc_count: 0,
                    processed_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::p2p_reconcile::session_hydration::HYDRATION_COLLECTIONS;

    struct MemoryStore {
        pending: Vec<HydrationRequestRow>,
        catalog: HydrationCatalog,
        served: std::sync::Mutex<Vec<(String, usize)>>,
        rejected: std::sync::Mutex<Vec<(String, String)>>,
    }

    struct RecordingDelivery {
        pushed: std::sync::Mutex<Vec<(String, BTreeSet<HydrationDocument>)>>,
    }

    #[async_trait]
    impl HydrationRequestStore for MemoryStore {
        async fn load_pending_requests(&self) -> Result<Vec<HydrationRequestRow>> {
            Ok(self.pending.clone())
        }
        async fn load_catalog(&self, _request: &HydrationRequest) -> Result<HydrationCatalog> {
            Ok(self.catalog.clone())
        }
        async fn mark_served(&self, request_key: &str, served_doc_count: usize) -> Result<()> {
            self.served
                .lock()
                .expect("served lock")
                .push((request_key.to_string(), served_doc_count));
            Ok(())
        }
        async fn mark_rejected(&self, request_key: &str, detail: &str) -> Result<()> {
            self.rejected
                .lock()
                .expect("rejected lock")
                .push((request_key.to_string(), detail.to_string()));
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

    #[tokio::test]
    async fn admitted_request_pushes_exact_set_then_marks_served() {
        let document = HydrationDocument {
            collection: "AgentMessage".into(),
            doc_id: "owned".into(),
            requester_did: "did:key:requester-1".into(),
            agent_did: "did:key:agent-1".into(),
            session_id: "session-1".into(),
        };
        let store = MemoryStore {
            pending: vec![HydrationRequestRow {
                request_key: "peer-1:session-1".into(),
                requester_did: "did:key:requester-1".into(),
                agent_did: "did:key:agent-1".into(),
                session_id: "session-1".into(),
            }],
            catalog: HydrationCatalog {
                paired_peer_ids: BTreeSet::from(["peer-1".into()]),
                active_member_dids: BTreeSet::from(["did:key:requester-1".into()]),
                sessions: BTreeSet::from([SessionOwner {
                    session_id: "session-1".into(),
                    requester_did: "did:key:requester-1".into(),
                    agent_did: "did:key:agent-1".into(),
                }]),
                documents: BTreeSet::from([document.clone()]),
            },
            served: std::sync::Mutex::new(Vec::new()),
            rejected: std::sync::Mutex::new(Vec::new()),
        };
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
        assert!(HYDRATION_COLLECTIONS.contains(&"AgentToolApproval"));
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
