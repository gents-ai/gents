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
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::identity::AgentIdentity;

use super::graphql_helpers::{ensure_no_errors, rows};
use super::session_hydration::{
    decide_hydration, AppliedPairingRoute, HydrationCatalog, HydrationDocument, HydrationRequest,
    HydrationVerdict, SessionOwner, VerifiedActiveMembership, HYDRATION_COLLECTIONS,
};
use super::templates::{conjunctive_string_eq, decode_pairing_filters};

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
    async fn load_catalog(&self, request: &HydrationRequest) -> Result<HydrationCatalog>;
    async fn mark_served(&self, request_key: &str, served_doc_count: usize) -> Result<()>;
    async fn mark_rejected(&self, request_key: &str, detail: &str) -> Result<()>;
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
    identity: Arc<dyn AgentIdentity>,
    cancel: CancellationToken,
) -> Result<()> {
    let store = GraphqlHydrationStore {
        node: node.clone(),
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
        let active =
            super::network::GraphqlNetworkStore::new(self.node.clone(), self.identity.clone())
                .load_verified_active_memberships()
                .await
                .context("load verified active hydration memberships")?;
        let session_id = escape_graphql_string(&request.session_id);
        let peer_id = escape_graphql_string(&request.peer_id);
        let query = format!(
            r#"{{
                PeerPairingDesired(filter: {{ peer_id: {{ _eq: "{peer_id}" }} }}) {{
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
        );
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

        Ok(HydrationCatalog {
            applied_pairing_routes,
            selected_network_id: active.network_id.clone(),
            verified_active_memberships: active
                .member_dids
                .into_iter()
                .map(|member_did| VerifiedActiveMembership {
                    network_id: active.network_id.clone(),
                    member_did,
                })
                .collect(),
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
    use crate::agent::p2p_reconcile::templates::{combine_filters, equality_filter};

    struct MemoryStore {
        pending: Vec<HydrationRequestRow>,
        catalog: HydrationCatalog,
        served: std::sync::Mutex<Vec<(String, usize)>>,
        rejected: std::sync::Mutex<Vec<(String, String)>>,
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
            served: std::sync::Mutex::new(Vec::new()),
            rejected: std::sync::Mutex::new(Vec::new()),
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
