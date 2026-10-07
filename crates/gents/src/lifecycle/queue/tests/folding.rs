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
    let mut pending: Vec<u64> = Vec::new();
    let mut active: Option<RequestLifecycle> = None;
    let mut claims = Vec::new();
    let writer = DefraStreamWriter::new(db.node.clone(), db.agent_did(), Duration::ZERO);
    for event in &case.inputs {
        match event {
            LeanFoldQueueInput::Enqueue { entry } => {
                let doc = enqueue(&db, &session_id, entry, bound.len()).await;
                bound.insert(entry.request_id, doc);
                pending.push(entry.request_id);
            }
            LeanFoldQueueInput::Claim { admitted } => {
                assert!(active.is_none(), "{}: claim while active", case.name);
                let head = pending.remove(0);
                let request =
                    crate::request_binding::load_agent_request_by_doc_id(&db.node, &bound[&head])
                        .await
                        .unwrap()
                        .unwrap();
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
                    "{}: head {head} must claim",
                    case.name
                );
                let rows = fold_rows(&db.node, &session_id).await;
                let mut folded = rows
                    .iter()
                    .filter(|row| {
                        row.superseded_by_request_doc_id.as_deref() == Some(bound[&head].as_str())
                    })
                    .inspect(|row| {
                        assert_eq!(row.lifecycle_state, RequestLifecycleState::Superseded);
                        assert_eq!(row.failure_reason.as_deref(), Some(FOLDED_REASON));
                    })
                    .map(modeled_id)
                    .collect::<Vec<_>>();
                // The fold supersedes in queue order; the durable owner
                // replays that order for execution.
                let loaded = load_folded_inputs(&db.node, lifecycle.request())
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|input| input.request_id.parse::<u64>().unwrap())
                    .collect::<Vec<_>>();
                folded.sort_by_key(|id| pending.iter().position(|pending| pending == id));
                assert_eq!(loaded, folded, "{}: folded input order", case.name);
                pending.retain(|id| !folded.contains(id));
                claims.push((head, folded));
                active = Some(lifecycle);
            }
            LeanFoldQueueInput::Finish => {
                let mut lifecycle = active.take().expect("finish requires an active claim");
                lifecycle.begin_owned_execution(&writer).await.unwrap();
                lifecycle
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
    let mut native_pending = rows
        .iter()
        .filter(|row| row.lifecycle_state == RequestLifecycleState::Pending)
        .map(modeled_id)
        .collect::<Vec<_>>();
    native_pending.sort_unstable();
    let mut expected_pending = case.expected.pending.clone();
    expected_pending.sort_unstable();
    assert_eq!(
        native_pending, expected_pending,
        "{}: pending requests",
        case.name
    );
    let expected_claims = case
        .expected
        .claims
        .iter()
        .map(|claim| (claim.head, claim.folded.clone()))
        .collect::<Vec<_>>();
    assert_eq!(claims, expected_claims, "{}: per-claim folds", case.name);
}
