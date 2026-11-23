use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::config_client::{ConfigAccess, IdempotentTransactionRetry};

use gents_lean_vocab::families::contracts_session_advance as advance;

type Document = advance::DocumentWire<RequestLifecycleState>;
type Row = advance::RequestWire<RequestLifecycleState>;

/// Model identifiers and seconds as the stored strings, and the physical
/// request documents back to the model's document numbers.
struct Names {
    base: chrono::DateTime<chrono::FixedOffset>,
    docs: BTreeMap<u64, String>,
}

impl Names {
    fn new() -> Self {
        Self {
            base: chrono::DateTime::parse_from_rfc3339("2030-01-01T00:00:00Z").unwrap(),
            docs: BTreeMap::new(),
        }
    }

    fn node(n: u64) -> String {
        format!("did:test:node-{n}")
    }

    fn session(n: u64) -> String {
        format!("session-{n}")
    }

    fn requester(n: u64) -> String {
        format!("did:test:requester-{n}")
    }

    fn agent(n: u64) -> String {
        format!("agent-{n}")
    }

    fn request_id(n: u64) -> String {
        format!("{n:08}")
    }

    fn at(&self, seconds: u64) -> String {
        (self.base + chrono::Duration::seconds(i64::try_from(seconds).unwrap())).to_rfc3339()
    }

    fn seconds(&self, value: &str) -> u64 {
        u64::try_from(
            (chrono::DateTime::parse_from_rfc3339(value).unwrap() - self.base).num_seconds(),
        )
        .unwrap()
    }

    /// A model document no row was created for names a physical identity no
    /// stored request carries.
    fn doc(&mut self, n: u64) -> String {
        self.docs
            .entry(n)
            .or_insert_with(|| format!("bae-{}", uuid::Uuid::from_u128(u128::from(n))))
            .clone()
    }

    fn model_doc(&self, physical: &str) -> u64 {
        *self
            .docs
            .iter()
            .find(|(_, doc)| doc.as_str() == physical)
            .expect("observation names an unknown request")
            .0
    }

    fn observation(
        &mut self,
        wire: &advance::ObservationWire<RequestLifecycleState>,
    ) -> gents_protocol::session::SessionObservation {
        gents_protocol::session::SessionObservation {
            last_activity_at: self.at(wire.activity),
            preview: wire.preview.clone(),
            reply_preview: None,
            latest_request: wire.latest.as_ref().map(|latest| {
                gents_protocol::session::SessionRequestObservation {
                    request_doc_id: self.doc(latest.doc_id),
                    request_id: Self::request_id(latest.request_id),
                    lifecycle_state: latest.state,
                }
            }),
        }
    }

    fn modeled(
        &self,
        observation: &gents_protocol::session::SessionObservation,
    ) -> advance::ObservationWire<RequestLifecycleState> {
        assert_eq!(observation.reply_preview, None);
        advance::ObservationWire {
            activity: self.seconds(&observation.last_activity_at),
            preview: observation.preview.clone(),
            latest: observation
                .latest_request
                .as_ref()
                .map(|latest| advance::LatestWire {
                    doc_id: self.model_doc(&latest.request_doc_id),
                    request_id: latest.request_id.parse().expect("numeric request id"),
                    state: latest.lifecycle_state,
                }),
        }
    }
}

/// Stores the modeled session document and request rows; returns the stored
/// session before the operation.
async fn seed(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    names: &mut Names,
    before: &Document,
    rows: &[Row],
) -> anyhow::Result<gents_protocol::session::SessionOwnerRow> {
    let (node, session) = (
        Names::node(before.scope.node),
        Names::session(before.scope.session),
    );
    let requester = before.scope.requester.map(Names::requester);
    crate::session::ensure_session_in_txn(
        txn,
        crate::session::NewAgentSession {
            session_id: &session,
            node_did: &node,
            agent_did: &Names::agent(before.agent),
            requester_did: requester.as_deref(),
            title: None,
            provenance: None,
            created_at: &names.at(before.created_at),
        },
    )
    .await?;
    for row in rows {
        let response = txn
            .execute_with_variables(
                "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                &serde_json::json!({"input": {
                    "request_id": Names::request_id(row.request_id), "purpose": row.purpose,
                    "node_did": Names::node(row.scope.node),
                    "session_id": Names::session(row.scope.session),
                    "requester_did": row.scope.requester.map(Names::requester),
                    "agent_did": Names::agent(row.agent), "content": "prompt",
                    "created_at": names.at(row.created_at),
                    "terminalized_at": row.terminalized_at.map(|at| names.at(at)),
                    "lifecycle_state": row.state}}),
            )
            .await?;
        let doc = gents_store::graphql::created_doc_id(&response, "AgentRequest")?;
        anyhow::ensure!(
            names.docs.insert(row.doc_id, doc).is_none(),
            "two rows share a model document"
        );
    }
    let stored = load_agent_session_row_in_txn(txn, &node, &session, requester.as_deref())
        .await?
        .context("seeded session is missing")?;
    if let Some(observation) = &before.observation {
        patch_session_in_txn(
            txn,
            &stored.doc_id,
            serde_json::json!({ "observation": names.observation(observation) }),
        )
        .await?;
    }
    load_agent_session_row_in_txn(txn, &node, &session, requester.as_deref())
        .await?
        .context("seeded session is missing")
}

/// The modeled document after the operation: only the observation may change,
/// and the stored one is read back into the model's wire.
async fn modeled_after(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    names: &Names,
    before: &Document,
    stored_before: &gents_protocol::session::SessionOwnerRow,
) -> anyhow::Result<Document> {
    let stored = load_agent_session_row_in_txn(
        txn,
        &stored_before.session.node_did,
        &stored_before.session.session_id,
        stored_before.session.requester_did.as_deref(),
    )
    .await?
    .context("session disappeared")?
    .session;
    let mut unchanged = stored.clone();
    unchanged.observation = stored_before.session.observation.clone();
    assert_eq!(unchanged, stored_before.session);
    let mut after = before.clone();
    after.observation = stored.observation.as_ref().map(|o| names.modeled(o));
    Ok(after)
}

#[tokio::test]
async fn generated_session_advances_drive_the_claim_observation() {
    for case in
        gents_lean_vocab::families::contracts_session_advance::cases::<RequestLifecycleState>()
    {
        let node_db = gents_store::test_support::TestStore::start(
            "generated-session-advances-drive-the-cla",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = node_db.node.clone();
        let input = case.input.clone();
        let after = ConfigAccess::transact_local_idempotent(
            &node,
            None,
            IdempotentTransactionRetry::Standard,
            "test.session.generated_advance",
            |txn| {
                let input = input.clone();
                Box::pin(async move {
                    let mut names = Names::new();
                    let stored = seed(txn, &mut names, &input.before, &input.rows).await?;
                    let request = &input.request;
                    let physical = names.doc(request.doc_id);
                    let incoming = load_scoped_request_facts_in_txn(txn, &stored.session, false)
                        .await?
                        .into_iter()
                        .find(|fact| {
                            fact.observed.request_doc_id == physical
                                && fact.observed.request_id == Names::request_id(request.request_id)
                        })
                        .context("claimed request missing from its exact session scope")?;
                    advance_session_request_observation_in_txn(
                        txn,
                        &incoming,
                        &input.normalized_preview,
                        &names.at(input.now),
                    )
                    .await?;
                    modeled_after(txn, &names, &input.before, &stored).await
                })
            },
        )
        .await
        .unwrap();
        case.check(advance::DocumentAfter { after });
        node.shutdown().await.expect("shut down test node");
    }
}

#[tokio::test]
async fn generated_session_refreshes_drive_the_request_observation() {
    for case in
        gents_lean_vocab::families::contracts_session_refresh::cases::<RequestLifecycleState>()
    {
        let node_db = gents_store::test_support::TestStore::start(
            "generated-session-refreshes-drive-the-re",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = node_db.node.clone();
        let input = case.input.clone();
        let before: Document =
            serde_json::from_value(serde_json::to_value(&input.before).unwrap()).unwrap();
        let rows: Vec<Row> =
            serde_json::from_value(serde_json::to_value(&input.rows).unwrap()).unwrap();
        let after = ConfigAccess::transact_local_idempotent(
            &node,
            None,
            IdempotentTransactionRetry::Standard,
            "test.session.generated_refresh",
            |txn| {
                let (input, before, rows) = (input.clone(), before.clone(), rows.clone());
                Box::pin(async move {
                    let mut names = Names::new();
                    let stored = seed(txn, &mut names, &before, &rows).await?;
                    let event = &input.event;
                    let physical = names.doc(event.doc_id);
                    refresh_session_request_observation_in_txn(
                        txn,
                        &stored.session.node_did,
                        stored.session.requester_did.as_deref(),
                        &stored.session.session_id,
                        SessionRequestRefresh {
                            request_doc_id: &physical,
                            request_id: &Names::request_id(event.request_id),
                            reply_preview: None,
                        },
                        &names.at(input.now),
                    )
                    .await?;
                    modeled_after(txn, &names, &before, &stored).await
                })
            },
        )
        .await
        .unwrap();
        case.check(serde_json::from_value(serde_json::json!({ "after": after })).unwrap());
        node.shutdown().await.expect("shut down test node");
    }
}
