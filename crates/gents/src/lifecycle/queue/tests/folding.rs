use super::*;
use crate::lean_vocab_test::{LeanFoldQueueCase, LeanFoldQueueEntry, LeanFoldQueueInput};
use crate::lifecycle::RequestTerminalOutcome;
use serde_json::json;
use std::collections::HashMap;

const FOREIGN_REQUESTER: &str = "did:test:fold-foreign-requester";
const OTHER_CWD: &str = "/tmp/gents-fold-other-context";

async fn insert_row(node: &EmbeddedNode, input: serde_json::Value) -> String {
    crate::config_client::ConfigAccess::transact_local(node, None, "test.fold.insert", |txn| {
        let input = input.clone();
        Box::pin(async move {
            let result = txn
                .execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({ "input": input }),
                )
                .await?;
            gents_protocol::graphql::extract_mutation_doc_id(&result, "AgentRequest")
        })
    })
    .await
    .unwrap()
}

/// Modeled requester 1 is the agent's own principal; 2 is another signer.
fn requester_did(db: &TestDb, requester: Option<u64>) -> Option<String> {
    match requester {
        Some(1) => Some(db.agent_did().to_owned()),
        Some(2) => Some(FOREIGN_REQUESTER.to_owned()),
        None => None,
        Some(other) => panic!("no native principal for modeled requester {other}"),
    }
}

/// The modeled turn context is the request's execution settings; context 1
/// differs from the head only in its working directory.
fn generated_input(entry: &LeanFoldQueueEntry) -> RequestInput {
    let source: QueueSource = serde_json::from_value(json!(entry.source)).unwrap();
    let policy: QueuePolicy = serde_json::from_value(json!(entry.policy)).unwrap();
    let queue = match (source, entry.queued_after) {
        (QueueSource::User, None) => None,
        (source, queued_after) => Some(RequestQueue {
            source,
            policy,
            key: None,
            queued_after_request_id: queued_after.map(|id| id.to_string()),
            interrupted_request_id: None,
            background_completion_wake_version: None,
        }),
    };
    RequestInput {
        cwd: match entry.turn_context {
            0 => None,
            1 => Some(OTHER_CWD.to_owned()),
            other => panic!("no native settings for modeled turn context {other}"),
        },
        queue,
        ..RequestInput::default()
    }
}

async fn enqueue(
    db: &TestDb,
    session_id: &str,
    entry: &LeanFoldQueueEntry,
    arrival: usize,
) -> String {
    insert_row(
        &db.node,
        json!({
            "request_id": entry.request_id.to_string(),
            "purpose": "normal",
            "agent_did": db.agent_did(),
            "requester_did": requester_did(db, entry.requester_id),
            "behavior_id": TEST_BEHAVIOR_ID,
            "session_id": session_id,
            "content": format!("message {}", entry.request_id),
            "input": generated_input(entry),
            "lifecycle_state": "pending",
            "execution_origin": entry.execution_origin,
            "created_at": format!("2026-09-01T00:00:{arrival:02}Z"),
            "subagent_depth": 0,
            "retry_count": 0,
            "max_retries": 3,
        }),
    )
    .await
}

#[derive(Debug, Deserialize)]
struct FoldRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    lifecycle_state: RequestLifecycleState,
    #[serde(default)]
    superseded_by_request_doc_id: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
}

async fn fold_rows(node: &EmbeddedNode, session_id: &str) -> Vec<FoldRow> {
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
                _docID request_id lifecycle_state superseded_by_request_doc_id failure_reason
            }} }}"#,
            escape_graphql_string(session_id)
        ),
        "fold rows",
    )
    .await
    .unwrap();
    crate::graphql::rows(&response, "AgentRequest").unwrap()
}

fn modeled_id(row: &FoldRow) -> u64 {
    row.request_id.parse().unwrap()
}

/// The native active turn: its lifecycle, the run its claim selected, and
/// how many of those it has published.
struct ActiveTurn {
    lifecycle: RequestLifecycle,
    selected: Vec<u64>,
    consumed: usize,
    begun: bool,
}

impl ActiveTurn {
    async fn begin(&mut self, writer: &DefraStreamWriter) {
        if !self.begun {
            self.lifecycle.begin_owned_execution(writer).await.unwrap();
            self.begun = true;
        }
    }
}

async fn claim_head(
    db: &TestDb,
    bound: &HashMap<u64, String>,
    head: u64,
    admitted: &[u64],
) -> ActiveTurn {
    let request = crate::request_binding::load_agent_request_by_doc_id(&db.node, &bound[&head])
        .await
        .unwrap()
        .unwrap();
    claim_request(db, bound, request, admitted).await
}

async fn claim_request(
    db: &TestDb,
    bound: &HashMap<u64, String>,
    request: crate::watcher::AgentRequest,
    admitted: &[u64],
) -> ActiveTurn {
    let head = request.request_id.clone();
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        db.node.clone(),
        TEST_BEHAVIOR_ID,
        db.agent_did(),
        request,
        60,
    );
    lifecycle.set_fold_admitted(admitted.iter().map(|id| bound[id].clone()).collect());
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        ClaimOutcome::Claimed,
        "head {head} must claim"
    );
    let selected = lifecycle
        .folded_selection()
        .iter()
        .map(|folded| {
            assert_eq!(
                bound[&folded.request_id.parse::<u64>().unwrap()],
                folded.request_doc_id
            );
            folded.request_id.parse::<u64>().unwrap()
        })
        .collect();
    ActiveTurn {
        lifecycle,
        selected,
        consumed: 0,
        begun: false,
    }
}

#[tokio::test]
async fn generated_fold_queue_cases_bind_to_native_claims() {
    let cases = crate::lean_vocab_test::lean_fold_queue_cases();
    assert!(!cases.is_empty());
    for case in cases {
        drive(case).await;
    }
}

async fn drive(case: &LeanFoldQueueCase) {
    assert_eq!(case.agent_id, 1, "fixture maps modeled agent 1 to its DID");
    let db = test_db(&case.name).await;
    let session_id = case.session_id.to_string();
    let mut bound: HashMap<u64, String> = HashMap::new();
    // Native queue order: admission order of rows still pending.
    let mut arrivals: Vec<u64> = Vec::new();
    let mut active: Option<ActiveTurn> = None;
    let mut claims = Vec::new();
    let writer = DefraStreamWriter::new(db.node.clone(), db.agent_did(), Duration::ZERO);
    for event in &case.inputs {
        match event {
            LeanFoldQueueInput::Enqueue { entry } => {
                let doc = enqueue(&db, &session_id, entry, bound.len()).await;
                bound.insert(entry.request_id, doc);
                arrivals.push(entry.request_id);
            }
            LeanFoldQueueInput::Claim { admitted } => {
                assert!(active.is_none(), "{}: claim while active", case.name);
                let rows = fold_rows(&db.node, &session_id).await;
                let head = *arrivals
                    .iter()
                    .find(|id| {
                        rows.iter().any(|row| {
                            modeled_id(row) == **id
                                && row.lifecycle_state == RequestLifecycleState::Pending
                        })
                    })
                    .expect("a pending head");
                let turn = claim_head(&db, &bound, head, admitted).await;
                claims.push((head, turn.selected.clone()));
                active = Some(turn);
            }
            LeanFoldQueueInput::Consume => {
                let turn = active.as_mut().expect("consume requires an active turn");
                turn.begin(&writer).await;
                let folded = turn.selected[turn.consumed];
                writer
                    .publish_authored_message(
                        &turn.lifecycle,
                        &folded_input_key(&bound[&folded]),
                        &gents_protocol::message::Message::user(format!("message {folded}")),
                    )
                    .await
                    .unwrap();
                turn.consumed += 1;
            }
            LeanFoldQueueInput::Finish => {
                let mut turn = active.take().expect("finish requires an active turn");
                turn.begin(&writer).await;
                turn.lifecycle
                    .terminalize_owned(
                        RequestTerminalOutcome::Completed,
                        gents_protocol::output::TerminalOutput::NoMessage,
                        None,
                    )
                    .await
                    .unwrap();
            }
        }
    }

    let rows = fold_rows(&db.node, &session_id).await;
    let by_id: HashMap<u64, &FoldRow> = rows.iter().map(|row| (modeled_id(row), row)).collect();
    assert_eq!(
        by_id.len(),
        bound.len(),
        "{}: exact row inventory",
        case.name
    );
    for (id, doc) in &bound {
        assert_eq!(&by_id[id].doc_id, doc, "{}: physical identity", case.name);
    }
    let native_active = rows
        .iter()
        .filter(|row| {
            matches!(
                row.lifecycle_state,
                RequestLifecycleState::Claimed | RequestLifecycleState::Processing
            )
        })
        .map(modeled_id)
        .collect::<Vec<_>>();
    assert_eq!(
        native_active,
        case.expected.active.into_iter().collect::<Vec<_>>(),
        "{}: active request",
        case.name
    );
    let native_folding = active
        .as_ref()
        .map(|turn| turn.selected[turn.consumed..].to_vec())
        .unwrap_or_default();
    assert_eq!(
        native_folding, case.expected.folding,
        "{}: selected run",
        case.name
    );
    let native_pending = arrivals
        .iter()
        .copied()
        .filter(|id| by_id[id].lifecycle_state == RequestLifecycleState::Pending)
        .filter(|id| !native_folding.contains(id))
        .collect::<Vec<_>>();
    assert_eq!(
        native_pending, case.expected.pending,
        "{}: pending queue",
        case.name
    );
    let mut native_terminal = rows
        .iter()
        .filter(|row| row.lifecycle_state.is_terminal())
        .map(modeled_id)
        .collect::<Vec<_>>();
    native_terminal.sort_unstable();
    assert_eq!(
        native_terminal, case.expected.terminal,
        "{}: terminal requests",
        case.name
    );
    for row in rows
        .iter()
        .filter(|row| row.lifecycle_state == RequestLifecycleState::Superseded)
    {
        assert_eq!(
            row.failure_reason.as_deref(),
            Some(FOLDED_REASON),
            "{}",
            case.name
        );
        let head = claims
            .iter()
            .find(|(_, selected)| selected.contains(&modeled_id(row)))
            .map(|(head, _)| *head)
            .expect("a superseded message was selected by a claim");
        assert_eq!(
            row.superseded_by_request_doc_id.as_deref(),
            Some(bound[&head].as_str()),
            "{}: supersession points at the answering request",
            case.name
        );
    }
    let expected_claims = case
        .expected
        .claims
        .iter()
        .map(|claim| (claim.head, claim.folded.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        claims, expected_claims,
        "{}: per-claim selections",
        case.name
    );
}

#[tokio::test]
async fn generated_handover_fold_claims_bind_to_native_claims() {
    let cases = crate::lean_vocab_test::lean_handover_fold_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let db = test_db(&case.name).await;
        let session_id = format!("handover-{}", case.name);
        let mut bound = HashMap::new();
        for (arrival, entry) in case.pending.iter().enumerate() {
            bound.insert(
                entry.request_id,
                enqueue(&db, &session_id, entry, arrival).await,
            );
        }
        let head = case.pending[0].request_id;
        let turn = claim_head(&db, &bound, head, &case.admitted).await;
        assert_eq!(Some(head), case.expected.active, "{}: active", case.name);
        assert_eq!(
            turn.selected, case.expected.folding,
            "{}: selection",
            case.name
        );
        let rows = fold_rows(&db.node, &session_id).await;
        let pending = case
            .pending
            .iter()
            .map(|entry| entry.request_id)
            .filter(|id| {
                rows.iter().any(|row| {
                    modeled_id(row) == *id && row.lifecycle_state == RequestLifecycleState::Pending
                })
            })
            .filter(|id| !turn.selected.contains(id))
            .collect::<Vec<_>>();
        assert_eq!(pending, case.expected.pending, "{}: pending", case.name);
    }
}

async fn authored_keys(node: &EmbeddedNode, request_doc_id: &str) -> Vec<String> {
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}, order: {{ sequence: ASC }}) {{ message_key }} }}"#,
            escape_graphql_string(request_doc_id)
        ),
        "authored keys",
    )
    .await
    .unwrap();
    response.data.unwrap()["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["message_key"].as_str().unwrap().to_owned())
        .collect()
}

/// Lean `reuseAuthored`: a crash between or after the turn's input
/// publications, then a reclaim under a new generation, republishes that input
/// by reusing every accepted entry and consuming only what is still queued,
/// leaving exactly one entry per key.
#[tokio::test]
async fn reclaimed_turn_reuses_published_input_once_per_key() {
    for published_before_crash in 0..=3 {
        let name = format!("reclaim-after-{published_before_crash}");
        let db = test_db(&name).await;
        let session_id = name.clone();
        let mut bound = HashMap::new();
        for (arrival, (id, queued_after)) in [(101, None), (102, Some(101)), (103, Some(101))]
            .into_iter()
            .enumerate()
        {
            let entry = LeanFoldQueueEntry {
                request_id: id,
                execution_origin: "interactive".into(),
                source: "user".into(),
                policy: "append".into(),
                queued_after,
                requester_id: Some(1),
                turn_context: 0,
            };
            bound.insert(id, enqueue(&db, &session_id, &entry, arrival).await);
        }
        let writer = DefraStreamWriter::new(db.node.clone(), db.agent_did(), Duration::ZERO);
        let inputs = |turn: &ActiveTurn, consumed: &[u64]| {
            std::iter::once(("prompt".to_owned(), "message 101".to_owned()))
                .chain(
                    consumed
                        .iter()
                        .chain(turn.selected.iter())
                        .map(|id| (folded_input_key(&bound[id]), format!("message {id}"))),
                )
                .collect::<Vec<_>>()
        };

        let mut first = claim_head(&db, &bound, 101, &[102, 103]).await;
        assert_eq!(first.selected, [102, 103]);
        first.begin(&writer).await;
        for (key, content) in inputs(&first, &[]).into_iter().take(published_before_crash) {
            writer
                .publish_authored_message(
                    &first.lifecycle,
                    &key,
                    &gents_protocol::message::Message::user(content),
                )
                .await
                .unwrap();
        }
        let first_generation = first.lifecycle.execution_generation().unwrap().to_owned();
        let reclaimed_request = first.lifecycle.request().clone();
        drop(first);

        // The lease is lost and the same physical request is claimed again.
        crate::config_client::ConfigAccess::write_local_response(
            &db.node,
            "test.fold_reclaim",
            &format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "pending" }}) {{ _docID }} }}"#,
                escape_graphql_string(&bound[&101])
            ),
        )
        .await
        .unwrap();
        let consumed = load_consumed_folded_inputs(
            &db.node,
            &crate::request_binding::load_agent_request_by_doc_id(&db.node, &bound[&101])
                .await
                .unwrap()
                .unwrap(),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|input| input.request_id.parse::<u64>().unwrap())
        .collect::<Vec<_>>();
        let still_queued = [102, 103]
            .into_iter()
            .filter(|id| !consumed.contains(id))
            .collect::<Vec<_>>();
        let mut second = claim_request(&db, &bound, reclaimed_request, &still_queued).await;
        assert_ne!(
            second.lifecycle.execution_generation().unwrap(),
            first_generation
        );
        assert_eq!(
            second.selected, still_queued,
            "{name}: reclaim selects what is queued"
        );
        second.begin(&writer).await;
        let expected = inputs(&second, &consumed);
        for (key, content) in &expected {
            writer
                .publish_authored_message(
                    &second.lifecycle,
                    key,
                    &gents_protocol::message::Message::user(content.clone()),
                )
                .await
                .unwrap();
        }
        let keys = authored_keys(&db.node, &bound[&101]).await;
        assert_eq!(
            keys,
            expected
                .iter()
                .map(
                    |(key, _)| crate::session::canonical_rows::authored_message_key(
                        &bound[&101],
                        key
                    )
                )
                .collect::<Vec<_>>(),
            "{name}: exactly one entry per key, in order"
        );
        let rows = fold_rows(&db.node, &session_id).await;
        for id in [102, 103] {
            let row = rows.iter().find(|row| modeled_id(row) == id).unwrap();
            assert_eq!(
                row.lifecycle_state,
                RequestLifecycleState::Superseded,
                "{name}"
            );
            assert_eq!(
                row.superseded_by_request_doc_id.as_deref(),
                Some(bound[&101].as_str())
            );
        }
    }
}

/// A turn that fails before it publishes its selected messages leaves them
/// queued: the next claim answers them.
#[tokio::test]
async fn early_failure_leaves_selected_messages_queued_for_the_next_turn() {
    let db = test_db("fold-early-failure").await;
    let session_id = "fold-early-failure".to_owned();
    let mut bound = HashMap::new();
    for (arrival, (id, queued_after)) in [(101, None), (102, Some(101)), (103, Some(101))]
        .into_iter()
        .enumerate()
    {
        let entry = LeanFoldQueueEntry {
            request_id: id,
            execution_origin: "interactive".into(),
            source: "user".into(),
            policy: "append".into(),
            queued_after,
            requester_id: Some(1),
            turn_context: 0,
        };
        bound.insert(id, enqueue(&db, &session_id, &entry, arrival).await);
    }
    let mut first = claim_head(&db, &bound, 101, &[102, 103]).await;
    assert_eq!(first.selected, [102, 103]);
    // Workspace resolution or prompt preparation fails before publication.
    first
        .lifecycle
        .terminalize_owned(
            RequestTerminalOutcome::Failed,
            gents_protocol::output::TerminalOutput::NoMessage,
            Some("workspace unavailable"),
        )
        .await
        .unwrap();
    let rows = fold_rows(&db.node, &session_id).await;
    for id in [102, 103] {
        let row = rows.iter().find(|row| modeled_id(row) == id).unwrap();
        assert_eq!(row.lifecycle_state, RequestLifecycleState::Pending);
        assert!(
            crate::lifecycle::folded_into(&gents_protocol::row::AgentRequestRow {
                request_id: row.request_id.clone(),
                lifecycle_state: Some(row.lifecycle_state),
                failure_reason: row.failure_reason.clone(),
                ..Default::default()
            })
            .is_none()
        );
    }
    let next = claim_head(&db, &bound, 102, &[103]).await;
    assert_eq!(
        next.selected,
        [103],
        "the next turn answers the queued messages"
    );
}
