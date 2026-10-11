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

/// Modeled requester 1 is the agent's owning node; 2 is another signer.
fn requester_did(db: &TestDb, requester: Option<u64>) -> Option<String> {
    match requester {
        Some(1) => Some(db.node_did().to_owned()),
        Some(2) => Some(FOREIGN_REQUESTER.to_owned()),
        None => None,
        Some(other) => panic!("no native node for modeled requester {other}"),
    }
}

/// The modeled turn context is the request's execution settings; context 1
/// differs from the head only in its working directory.
pub(super) fn generated_input(entry: &LeanFoldQueueEntry) -> RequestInput {
    let source: QueueSource = serde_json::from_value(json!(entry.source)).unwrap();
    let policy: QueuePolicy = serde_json::from_value(json!(entry.policy)).unwrap();
    let queue = match (source, entry.queued_after) {
        (QueueSource::User, None) if entry.delivery == "queue" => None,
        (source, queued_after) => Some(RequestQueue {
            delivery: serde_json::from_value(json!(entry.delivery)).unwrap(),
            position: None,
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
            other => Some(format!("/tmp/gents-fold-context-{other}")),
        },
        queue,
        ..RequestInput::default()
    }
}

pub(super) async fn enqueue(
    db: &TestDb,
    session_id: &str,
    entry: &LeanFoldQueueEntry,
    arrival: usize,
) -> String {
    enqueue_with_ttl(
        db,
        session_id,
        entry,
        arrival,
        (!entry.fresh).then_some("2000-01-01T00:00:00Z"),
    )
    .await
}

async fn enqueue_with_ttl(
    db: &TestDb,
    session_id: &str,
    entry: &LeanFoldQueueEntry,
    arrival: usize,
    valid_until: Option<&str>,
) -> String {
    insert_row(
        &db.node,
        json!({
            "valid_until": valid_until,
            "request_id": entry.request_id.to_string(),
            "purpose": "normal",
            "node_did": db.node_did(),
            "requester_did": requester_did(db, entry.requester_id),
            "agent_id": TEST_AGENT_ID,
            "session_id": session_id,
            "content": format!("message {}", entry.request_id),
            "input": generated_input(entry),
            "lifecycle_state": "pending",
            "execution_origin": entry.execution_origin,
            "created_at": format!("2026-09-01T00:00:{arrival:02}Z"),
            "request_hop": 0,
            "retry_count": 0,
            "max_retries": 3,
        }),
    )
    .await
}

async fn enqueue_signed(
    db: &TestDb,
    session_id: &str,
    entry: &LeanFoldQueueEntry,
    arrival: usize,
) -> String {
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        entry.request_id.to_string(),
        db.node_did(),
        db.node_did(),
        TEST_AGENT_ID,
        session_id,
        format!("message {}", entry.request_id),
        "interactive",
        format!("2026-09-01T00:00:{arrival:02}Z"),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(db.node_did()),
    );
    create.input = generated_input(entry);
    crate::sign_agent_request_create(db.identity.as_ref(), &mut create)
        .await
        .unwrap();
    let response = crate::config_client::ConfigAccess::write_local_response(
        &db.node,
        "test.fold.signed",
        &create.graphql_mutation().unwrap(),
    )
    .await
    .unwrap();
    extract_single_doc_id(&response, "create_AgentRequest").unwrap()
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
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        db.node.clone(),
        TEST_AGENT_ID,
        db.node_did(),
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
    assert_eq!(case.node_id, 1, "fixture maps modeled node 1 to its DID");
    let db = test_db(&case.name).await;
    let session_id = case.session_id.to_string();
    let mut bound: HashMap<u64, String> = HashMap::new();
    // Native queue order: admission order of rows still pending.
    let mut arrivals: Vec<u64> = Vec::new();
    let mut active: Option<ActiveTurn> = None;
    let mut claims = Vec::new();
    let writer = DefraStreamWriter::new(db.node.clone(), db.node_did(), Duration::ZERO);
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

/// Durable facts a rejected step must leave unchanged.
async fn durable_facts(node: &EmbeddedNode, session_id: &str) -> serde_json::Value {
    let session = escape_graphql_string(session_id);
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}, order: {{ sequence: ASC }}) {{ _docID message_key sequence }}
               AgentOutputSegment(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ _docID }}
               AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ _docID lifecycle_state execution_generation superseded_by_request_doc_id }} }}"#
        ),
        "fold publication durable facts",
    )
    .await
    .unwrap();
    let mut data = response.data.unwrap();
    for collection in ["AgentOutputSegment", "AgentRequest"] {
        let rows = data[collection].as_array_mut().unwrap();
        rows.sort_by_key(|row| row["_docID"].as_str().unwrap().to_owned());
    }
    data
}

/// Lean `FoldPublication.cases`: each composed script runs through the
/// native claim, authored publication, provider publication and terminal
/// owners, and every step's acceptance, queue and authored keys match the
/// model. Seeded replay: production lease expiry terminalizes the request
/// (`canonical_recovery`), so a model `recover` step is driven by re-pending
/// the same physical request directly and claiming it under a fresh
/// generation; this binds publication reuse and fencing on a replayed
/// request, not a production reclaim path. A rejected step leaves durable state unchanged. A native
/// terminal commit is the model's terminalize-then-finish boundary, so queue
/// facts are compared after `finish`.
#[tokio::test]
async fn generated_fold_publication_scripts_bind_to_native_owners() {
    use crate::lean_vocab_test::LeanFoldPublicationStep as Step;

    let cases = crate::lean_vocab_test::lean_fold_publication_cases();
    let steering_cases = crate::lean_vocab_test::lean_steering_publication_cases();
    assert!(!cases.is_empty());
    assert!(!steering_cases.is_empty());
    let loop_boundary_cases = steering_cases
        .iter()
        .filter(|case| {
            case.steps.iter().any(|step| {
                matches!(
                    step,
                    Step::Intake {
                        safe_boundary: false,
                        ..
                    } | Step::FinishOrIntake {
                        safe_boundary: false,
                        ..
                    }
                )
            })
        })
        .map(|case| case.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        loop_boundary_cases,
        [
            "streaming_boundary_does_not_take_input",
            "natural_completion_during_stream_refuses"
        ],
        "only unsafe-boundary premises are delegated to loop tests"
    );
    for case in cases.iter().chain(steering_cases).filter(|case| {
        !case.steps.iter().any(|step| {
            matches!(
                step,
                Step::Intake {
                    safe_boundary: false,
                    ..
                } | Step::FinishOrIntake {
                    safe_boundary: false,
                    ..
                }
            )
        })
    }) {
        let db = test_db(&case.name).await;
        let session_id = format!("fold-publication-{}", case.name);
        crate::session::ensure_session_with_agent_id_and_requester_did(
            &db.node,
            &session_id,
            db.node_did(),
            TEST_AGENT_ID,
            Some(db.node_did()),
        )
        .await
        .unwrap();
        let (_authority_owner, authority) =
            crate::agent::p2p_reconcile::enrollment_authority_channel();
        let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
            db.node.clone(),
            db.identity.clone(),
            authority,
        );
        let mut bound = HashMap::new();
        let mut arrival_ids = vec![case.head, case.selected];
        let mut saved_message = None;
        let clock_origin = chrono::Utc::now();
        let mut observed_now = None;
        for (arrival, id) in [case.head, case.selected].into_iter().enumerate() {
            let entry = LeanFoldQueueEntry {
                request_id: id,
                execution_origin: "interactive".into(),
                delivery: "queue".into(),
                order_key: id,
                source: "user".into(),
                policy: "append".into(),
                queued_after: Some(10),
                requester_id: Some(1),
                turn_context: 0,
                fresh: true,
            };
            bound.insert(id, enqueue_signed(&db, &session_id, &entry, arrival).await);
        }
        let head_doc = bound[&case.head].clone();
        let writer = DefraStreamWriter::new(db.node.clone(), db.node_did(), Duration::ZERO);
        let mut generations: HashMap<u64, ActiveTurn> = HashMap::new();
        let mut first = claim_head(&db, &bound, case.head, &[case.selected]).await;
        first.begin(&writer).await;
        generations.insert(8, first);
        let scope: gents_protocol::rendered_request::CaptureScope = "inference.1".parse().unwrap();
        assert_eq!(case.steps.len(), case.expected.len());
        for (index, (step, expected)) in case.steps.iter().zip(&case.expected).enumerate() {
            let before = durable_facts(&db.node, &session_id).await;
            let accepted = match step {
                Step::ObserveDeadline { now, deadline } => {
                    observed_now = Some(clock_origin + chrono::Duration::milliseconds(*now as i64));
                    for turn in generations.values_mut() {
                        turn.lifecycle.claimed_deadline_at =
                            Some(clock_origin + chrono::Duration::milliseconds(*deadline as i64));
                    }
                    true
                }
                Step::PublishPrompt { generation } | Step::PublishChangedPrompt { generation } => {
                    let content = if matches!(step, Step::PublishPrompt { .. }) {
                        "prompt"
                    } else {
                        "changed prompt"
                    };
                    writer
                        .publish_authored_message(
                            &generations[generation].lifecycle,
                            "prompt",
                            &gents_protocol::message::Message::user(content),
                        )
                        .await
                        .is_ok()
                }
                Step::PublishFolded {
                    generation,
                    request_id,
                } => writer
                    .publish_authored_message_with_time(
                        &generations[generation].lifecycle,
                        &folded_input_key(&bound[request_id]),
                        &gents_protocol::message::Message::user(format!("message {request_id}")),
                        observed_now,
                    )
                    .await
                    .is_ok(),
                Step::AcceptTurn { generation } => {
                    writer
                        .start_provider_attempt(&head_doc, 0, 0, scope.clone())
                        .await;
                    match writer
                        .publish_native_turn(
                            &generations[generation].lifecycle,
                            0,
                            0,
                            &gents_protocol::message::Message::assistant("answer"),
                        )
                        .await
                    {
                        Ok(receipt) => {
                            saved_message = Some(receipt.message_doc_id);
                            true
                        }
                        Err(_) => false,
                    }
                }
                Step::Recover { expected, fresh } => {
                    let mut request = generations[expected].lifecycle.request().clone();
                    request.deadline = None;
                    crate::config_client::ConfigAccess::write_local_response(
                        &db.node,
                        "test.fold_publication_reclaim",
                        &format!(
                            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "pending", deadline: null }}) {{ _docID }} }}"#,
                            escape_graphql_string(&head_doc)
                        ),
                    )
                    .await
                    .unwrap();
                    let still_queued = fold_rows(&db.node, &session_id)
                        .await
                        .iter()
                        .filter(|row| {
                            modeled_id(row) == case.selected
                                && row.lifecycle_state == RequestLifecycleState::Pending
                        })
                        .map(modeled_id)
                        .collect::<Vec<_>>();
                    let mut reclaimed = claim_request(&db, &bound, request, &still_queued).await;
                    reclaimed.begin(&writer).await;
                    generations.insert(*fresh, reclaimed);
                    true
                }
                Step::Terminalize { generation } => generations
                    .get_mut(generation)
                    .unwrap()
                    .lifecycle
                    .terminalize_owned(
                        crate::lifecycle::RequestTerminalOutcome::Completed,
                        gents_protocol::output::TerminalOutput::NoMessage,
                        None,
                    )
                    .await
                    .is_ok(),
                Step::Finish => true,
                Step::EnqueueSteering { request_id } => {
                    let entry = LeanFoldQueueEntry {
                        request_id: *request_id,
                        execution_origin: "interactive".into(),
                        delivery: "steer".into(),
                        order_key: *request_id,
                        source: "user".into(),
                        policy: "append".into(),
                        queued_after: Some(case.head),
                        requester_id: Some(1),
                        turn_context: 0,
                        fresh: true,
                    };
                    let id = enqueue_signed(&db, &session_id, &entry, arrival_ids.len()).await;
                    bound.insert(*request_id, id);
                    arrival_ids.push(*request_id);
                    true
                }
                Step::Intake {
                    generation,
                    safe_boundary,
                }
                | Step::FinishOrIntake {
                    generation,
                    safe_boundary,
                } => {
                    assert!(
                        *safe_boundary,
                        "unsafe provider boundary belongs to loop owner"
                    );
                    let turn = generations.get_mut(generation).unwrap();
                    if !turn.lifecycle.owns_execution().await.unwrap()
                        || !crate::lifecycle::RequestLifecycle::input_publication_before_deadline(
                            observed_now.unwrap_or_else(chrono::Utc::now),
                            turn.lifecycle.claimed_deadline_at(),
                        )
                    {
                        false
                    } else {
                        let snapshot = super::super::intake::steering_snapshot(
                            &db.node,
                            turn.lifecycle.request(),
                            &verifier,
                        )
                        .await
                        .unwrap();
                        let selected = snapshot
                            .inputs
                            .iter()
                            .map(|input| {
                                *bound
                                    .iter()
                                    .find(|(_, doc)| folded_input_key(doc) == input.key)
                                    .unwrap()
                                    .0
                            })
                            .collect::<Vec<_>>();
                        if !selected.is_empty() || matches!(step, Step::Intake { .. }) {
                            turn.selected = selected;
                            true
                        } else {
                            turn.lifecycle
                                .finish_natural_turn(
                                    saved_message.as_deref().expect("accepted provider turn"),
                                    &snapshot.pending_request_doc_ids,
                                )
                                .await
                                .unwrap()
                        }
                    }
                }
                Step::CancelFirstPending => {
                    let access = crate::config_client::ConfigAccess::Local(db.node.clone());
                    let snapshot =
                        pending_user_queue(&access, db.node_did(), &session_id, db.node_did())
                            .await
                            .unwrap();
                    if let Some(first) = snapshot.entries.first() {
                        let result = replace_pending_user_messages(&access, db.identity.as_ref(),
                            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(db.node_did()),
                            db.node_did(), &session_id, db.node_did(), PendingQueueEdit {
                                expected_request_doc_ids: snapshot.entries.iter().map(|entry| entry.request_doc_id.clone()).collect(),
                                selected_request_doc_ids: vec![first.request_doc_id.clone()], messages: vec![],
                            }).await;
                        if result.is_ok() {
                            for turn in generations.values_mut() {
                                turn.selected.clear();
                            }
                        }
                        result.is_ok()
                    } else {
                        false
                    }
                }
            };
            let label = format!("{} step {index} {step:?}", case.name);
            assert_eq!(accepted, expected.accepted, "{label}: acceptance");
            if !accepted {
                assert_eq!(
                    durable_facts(&db.node, &session_id).await,
                    before,
                    "{label}: a rejected step leaves durable state unchanged"
                );
            }
            let keys = authored_keys(&db.node, &head_doc)
                .await
                .into_iter()
                .filter_map(|key| {
                    key.strip_prefix(&crate::session::canonical_rows::authored_message_key(
                        &head_doc, "",
                    ))
                    .map(str::to_owned)
                })
                .map(|key| {
                    bound
                        .iter()
                        .find(|(_, doc)| key == folded_input_key(doc))
                        .map(|(id, _)| format!("folded:{id}"))
                        .unwrap_or(key)
                })
                .collect::<Vec<_>>();
            assert_eq!(keys, expected.authored_keys, "{label}: authored keys");
            if matches!(step, Step::Terminalize { .. }) {
                continue;
            }
            let rows = fold_rows(&db.node, &session_id).await;
            let state = |id: u64| {
                rows.iter()
                    .find(|row| modeled_id(row) == id)
                    .unwrap()
                    .lifecycle_state
            };
            let head_active = matches!(
                state(case.head),
                RequestLifecycleState::Claimed | RequestLifecycleState::Processing
            );
            let latest = generations.keys().max().copied().unwrap();
            let folding = if head_active {
                generations[&latest]
                    .selected
                    .iter()
                    .copied()
                    .filter(|id| state(*id) == RequestLifecycleState::Pending)
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            let pending = arrival_ids
                .iter()
                .copied()
                .filter(|id| *id != case.head)
                .filter(|id| state(*id) == RequestLifecycleState::Pending && !folding.contains(id))
                .collect::<Vec<_>>();
            let terminal = [case.head, case.selected]
                .into_iter()
                .filter(|id| state(*id).is_terminal())
                .collect::<Vec<_>>();
            assert_eq!(
                head_active.then_some(case.head),
                expected.active,
                "{label}: active"
            );
            assert_eq!(folding, expected.folding, "{label}: selected");
            assert_eq!(pending, expected.pending, "{label}: pending");
            assert_eq!(terminal, expected.terminal, "{label}: terminal");
        }
    }
}

#[tokio::test]
async fn claim_selected_publication_keeps_later_incompatible_pending_row() {
    let db = test_db("fold-selected-after-head").await;
    let session_id = "fold-selected-after-head";
    let mut bound = HashMap::new();
    for (arrival, (id, context, queued_after)) in
        [(2, 0, None), (3, 0, Some(2))].into_iter().enumerate()
    {
        let entry = LeanFoldQueueEntry {
            request_id: id,
            execution_origin: "interactive".into(),
            delivery: "queue".into(),
            order_key: id,
            source: "user".into(),
            policy: "append".into(),
            queued_after,
            requester_id: Some(1),
            turn_context: context,
            fresh: true,
        };
        bound.insert(id, enqueue_signed(&db, session_id, &entry, arrival).await);
    }
    let mut active = claim_head(&db, &bound, 2, &[3]).await;
    assert_eq!(active.selected, vec![3]);
    let writer = DefraStreamWriter::new(db.node.clone(), db.node_did(), Duration::ZERO);
    active.begin(&writer).await;
    let incompatible = LeanFoldQueueEntry {
        request_id: 1,
        execution_origin: "interactive".into(),
        delivery: "queue".into(),
        order_key: 1,
        source: "user".into(),
        policy: "append".into(),
        queued_after: None,
        requester_id: Some(1),
        turn_context: 1,
        fresh: true,
    };
    bound.insert(1, enqueue_signed(&db, session_id, &incompatible, 2).await);
    let before = fold_rows(&db.node, session_id).await;
    assert_eq!(
        before
            .iter()
            .find(|row| row.request_id == "1")
            .unwrap()
            .lifecycle_state,
        RequestLifecycleState::Pending
    );

    writer
        .publish_authored_message(
            &active.lifecycle,
            &folded_input_key(&bound[&3]),
            &gents_protocol::message::Message::user("message 3"),
        )
        .await
        .unwrap();
    let rows = fold_rows(&db.node, session_id).await;
    let incompatible = rows.iter().find(|row| row.request_id == "1").unwrap();
    let folded = rows.iter().find(|row| row.request_id == "3").unwrap();
    assert_eq!(incompatible.lifecycle_state, RequestLifecycleState::Pending);
    assert!(incompatible.superseded_by_request_doc_id.is_none());
    assert_eq!(folded.lifecycle_state, RequestLifecycleState::Superseded);
    assert_eq!(
        folded.superseded_by_request_doc_id.as_deref(),
        Some(bound[&2].as_str())
    );
}

#[tokio::test]
async fn expired_or_malformed_steering_is_a_modelled_publication_barrier() {
    use crate::lean_vocab_test::LeanQueueManagementOperation;
    for (name, ttl) in [
        ("expired_steering_is_barrier", "2000-01-01T00:00:00Z"),
        ("malformed_ttl_steering_is_barrier", "not-a-timestamp"),
    ] {
        let case = crate::lean_vocab_test::lean_queue_management_cases()
            .iter()
            .find(|case| case.name == name)
            .unwrap();
        let LeanQueueManagementOperation::Intake { active: head, .. } = &case.operation else {
            panic!("expected intake case");
        };
        let db = test_db(name).await;
        let session_id = name;
        let head_doc = enqueue_signed(&db, session_id, head, 0).await;
        let mut bound = HashMap::from([(head.request_id, head_doc)]);
        let mut active = claim_head(&db, &bound, head.request_id, &[]).await;
        let writer = DefraStreamWriter::new(db.node.clone(), db.node_did(), Duration::ZERO);
        active.begin(&writer).await;
        for (index, entry) in case.before.pending.iter().enumerate() {
            let doc = if entry.fresh {
                enqueue_signed(&db, session_id, entry, index + 1).await
            } else {
                // Corrupt TTL cannot pass canonical signing; seed it directly to
                // exercise the reader's fail-closed handling of stored corruption.
                enqueue_with_ttl(&db, session_id, entry, index + 1, Some(ttl)).await
            };
            bound.insert(entry.request_id, doc);
        }
        let (_owner, authority) = crate::agent::p2p_reconcile::enrollment_authority_channel();
        let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
            db.node.clone(),
            db.identity.clone(),
            authority,
        );
        let snapshot = super::super::intake::steering_snapshot(
            &db.node,
            active.lifecycle.request(),
            &verifier,
        )
        .await
        .unwrap();
        let expected = case.expected.as_ref().unwrap();
        assert_eq!(snapshot.inputs.len(), expected.folding.len(), "{name}");
        assert!(snapshot.inputs.is_empty(), "{name}");
        let blocked = &case.before.pending[0];
        let error = writer
            .publish_authored_message(
                &active.lifecycle,
                &folded_input_key(&bound[&blocked.request_id]),
                &gents_protocol::message::Message::user(format!("message {}", blocked.request_id)),
            )
            .await
            .unwrap_err();
        assert!(
            error.is::<super::super::PendingInputChanged>(),
            "{name}: {error:#}"
        );
        let rows = fold_rows(&db.node, session_id).await;
        for entry in &expected.pending {
            let row = rows
                .iter()
                .find(|row| row.request_id == entry.request_id.to_string())
                .unwrap();
            assert_eq!(
                row.lifecycle_state,
                RequestLifecycleState::Pending,
                "{name}"
            );
            assert!(row.superseded_by_request_doc_id.is_none(), "{name}");
        }
    }
}
