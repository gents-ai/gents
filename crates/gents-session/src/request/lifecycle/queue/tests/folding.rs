use super::*;
use crate::request::lifecycle::RequestTerminalOutcome;
use gents_lean_vocab::LeanFoldQueueEntry;
use rapidhash::{HashMapExt, RapidHashMap};
use serde::Deserialize;
use serde_json::json;

const FOREIGN_REQUESTER: &str = "did:test:fold-foreign-requester";

const OTHER_CWD: &str = "/tmp/gents-fold-other-context";

async fn insert_row(db: &TestDb, input: serde_json::Value) -> String {
    db.agent_access.transact_write("test.fold.insert", |txn| {
        let input = input.clone();
        Box::pin(async move {
            let result = txn
                .execute_with_variables(
                    "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                    &json!({ "input": input }),
                )
                .await?;
            Ok::<_, anyhow::Error>(gents_protocol::graphql::extract_mutation_doc_id(&result, "AgentRequest")?)
        })
    })
    .await
    .unwrap()
}

fn requester_did(db: &TestDb, requester: Option<u64>) -> Option<String> {
    match requester {
        Some(1) => Some(db.agent_did().to_owned()),
        Some(2) => Some(FOREIGN_REQUESTER.to_owned()),
        None => None,
        Some(other) => panic!("no native node for modeled requester {other}"),
    }
}

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
        db,
        json!({
            "valid_until": valid_until,
            "request_id": entry.request_id.to_string(),
            "purpose": "normal",
            "node_did": db.node_did(),
            "requester_did": requester_did(db, entry.requester_id),
            "agent_did": db.agent_did(),
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
        db.agent_did(),
        db.agent_did(),
        session_id,
        format!("message {}", entry.request_id),
        "interactive",
        format!("2026-09-01T00:00:{arrival:02}Z"),
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(db.agent_did()),
    );
    create.input = generated_input(entry);
    publish_signed_queue_request(db, &mut create).await
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

async fn fold_rows(db: &TestDb, session_id: &str) -> Vec<FoldRow> {
    let response = db
        .agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
                _docID request_id lifecycle_state superseded_by_request_doc_id failure_reason
            }} }}"#,
            escape_graphql_string(session_id)
        ))
        .await
        .unwrap();
    match response["data"]["AgentRequest"].clone() {
        serde_json::Value::Null => Vec::new(),
        rows => serde_json::from_value(rows).unwrap(),
    }
}

fn modeled_id(row: &FoldRow) -> u64 {
    row.request_id.parse().unwrap()
}

pub(super) struct ActiveTurn {
    pub(super) lifecycle: RequestLifecycle,
    selected: Vec<u64>,
    consumed: usize,
    begun: bool,
}

pub(super) async fn claim_head(
    db: &TestDb,
    bound: &RapidHashMap<u64, String>,
    head: u64,
    admitted: &[u64],
) -> ActiveTurn {
    let request = crate::request::binding::load_agent_request_by_doc_id_with_access(
        &db.agent_access,
        &bound[&head],
    )
    .await
    .unwrap()
    .unwrap();
    claim_request(db, bound, request, admitted).await
}

/// Signed fixture rows are verified by the Agent's admission owner before
/// claim, as the daemon does; unsigned fixture rows carry no admission.
async fn verify_claim_admission(
    db: &TestDb,
    request: gents_protocol::row::AgentRequest,
) -> (
    gents_protocol::row::AgentRequest,
    Option<crate::request::admission::ClientClaimAdmission>,
) {
    let escaped = escape_graphql_string(&request.doc_id);
    let response = db
        .agent_access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 2) {{ admission_kind }} }}"#
        ))
        .await
        .unwrap();
    let signed = response["data"]["AgentRequest"][0]["admission_kind"]
        .as_str()
        .is_some_and(|kind| !kind.is_empty());
    if !signed {
        return (request, None);
    }
    let (_authority_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
    let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
        db.node.clone(),
        db.identity.clone(),
        authority,
    );
    verifier
        .verify_fresh_for_claim_with_identity(&request, db.agent_identity.as_ref())
        .await
        .expect("verify signed fixture admission")
}

async fn claim_request(
    db: &TestDb,
    bound: &RapidHashMap<u64, String>,
    request: gents_protocol::row::AgentRequest,
    admitted: &[u64],
) -> ActiveTurn {
    let head = request.request_id.clone();
    let (request, client_claim) = verify_claim_admission(db, request).await;
    let mut lifecycle = RequestLifecycle::new_with_node_did(db.node.clone(), request, 60);
    lifecycle
        .bind_managed_agent(db.agent_identity.as_ref())
        .unwrap();
    // As the daemon does, signed folded input is re-verified by the held Agent
    // when its publication consumes it; unsigned fixture rows carry no admission.
    if client_claim.is_some() {
        let (_authority_owner, authority) =
            gents_p2p::p2p_reconcile::enrollment_authority_channel();
        lifecycle.set_steering_admission(
            crate::request::admission::AgentRequestAdmissionVerifier::new(
                db.node.clone(),
                db.identity.clone(),
                authority,
            ),
            db.agent_identity.clone(),
        );
    }
    lifecycle.set_client_claim_admission(client_claim);
    lifecycle.set_fold_admitted(admitted.iter().map(|id| bound[id].clone()).collect());
    assert_eq!(
        lifecycle.claim_with_identity().await.unwrap(),
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
    for case in gents_lean_vocab::families::fold_queue_contracts_fold_script::cases::<
        String,
        String,
        String,
        String,
        String,
    >() {
        let observed = drive(&case.input).await;
        case.check(Some(observed));
    }
}

fn modeled_entry<T>(entry: &T) -> LeanFoldQueueEntry
where
    T: serde::Serialize,
{
    let mut value = serde_json::to_value(entry).unwrap();
    let object = value.as_object_mut().unwrap();
    for absent in ["queue_key", "execution_directory", "goal_assignment"] {
        assert!(
            object.remove(absent).unwrap().is_null(),
            "the fixture builds no {absent}"
        );
    }
    object.remove("created_at");
    serde_json::from_value(value).unwrap()
}

async fn drive(
    case: &gents_lean_vocab::families::fold_queue_contracts_fold_script::FoldInput<
        String,
        String,
        String,
        String,
        String,
    >,
) -> gents_lean_vocab::families::fold_queue_contracts_fold_script::FoldObservation {
    assert_eq!(case.node_id, 1, "fixture maps modeled node 1 to its DID");
    let db = test_db(&case.name).await;
    let session_id = case.session_id.to_string();
    let mut bound: RapidHashMap<u64, String> = RapidHashMap::new();
    let mut arrivals: Vec<u64> = Vec::new();
    let mut active: Option<ActiveTurn> = None;
    let mut claims = Vec::new();
    let writer =
        DefraStreamWriter::new_managed(db.node.clone(), db.agent_identity.as_ref(), Duration::ZERO)
            .unwrap();
    for event in &case.inputs {
        match event.kind.as_str() {
            "enqueue" => {
                let entry = modeled_entry(event.entry.as_ref().expect("an enqueued entry"));
                let entry = &entry;
                let doc = enqueue(&db, &session_id, entry, bound.len()).await;
                bound.insert(entry.request_id, doc);
                arrivals.push(entry.request_id);
            }
            "claim" => {
                let admitted = &event.admitted;
                assert!(active.is_none(), "{}: claim while active", case.name);
                let rows = fold_rows(&db, &session_id).await;
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
            "consume" => {
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
            "finish" => {
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
            kind => panic!("{}: no fixture step for {kind}", case.name),
        }
    }

    let rows = fold_rows(&db, &session_id).await;
    let by_id: RapidHashMap<u64, &FoldRow> =
        rows.iter().map(|row| (modeled_id(row), row)).collect();
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
    assert!(
        native_active.len() <= 1,
        "{}: one active request",
        case.name
    );
    let native_folding = active
        .as_ref()
        .map(|turn| turn.selected[turn.consumed..].to_vec())
        .unwrap_or_default();

    let native_pending = arrivals
        .iter()
        .copied()
        .filter(|id| by_id[id].lifecycle_state == RequestLifecycleState::Pending)
        .filter(|id| !native_folding.contains(id))
        .collect::<Vec<_>>();

    let mut native_terminal = rows
        .iter()
        .filter(|row| row.lifecycle_state.is_terminal())
        .map(modeled_id)
        .collect::<Vec<_>>();
    native_terminal.sort_unstable();

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
    gents_lean_vocab::families::fold_queue_contracts_fold_script::FoldObservation {
        active: native_active.first().copied(),
        pending: native_pending,
        folding: native_folding,
        terminal: native_terminal,
        claims: claims
            .into_iter()
            .map(|(head, folded)| {
                gents_lean_vocab::families::fold_queue_contracts_fold_script::ClaimWire {
                    head,
                    folded,
                }
            })
            .collect(),
    }
}

#[tokio::test]
async fn generated_handover_fold_claims_bind_to_native_claims() {
    for case in gents_lean_vocab::families::fold_queue_contracts_handover_claim::cases::<
        String,
        String,
        String,
        String,
    >() {
        let db = test_db(&case.input.name).await;
        let session_id = format!("handover-{}", case.input.name);
        let mut bound = RapidHashMap::new();
        for (arrival, entry) in case.input.pending.iter().enumerate() {
            bound.insert(
                entry.request_id,
                enqueue(&db, &session_id, &modeled_entry(entry), arrival).await,
            );
        }
        let head = case.input.pending[0].request_id;
        let turn = claim_head(&db, &bound, head, &case.input.admitted).await;
        let rows = fold_rows(&db, &session_id).await;
        let state = |id: u64| {
            rows.iter()
                .find(|row| modeled_id(row) == id)
                .unwrap()
                .lifecycle_state
        };
        let ids = case.input.pending.iter().map(|entry| entry.request_id);
        let active = ids
            .clone()
            .filter(|id| {
                matches!(
                    state(*id),
                    RequestLifecycleState::Claimed | RequestLifecycleState::Processing
                )
            })
            .collect::<Vec<_>>();
        assert!(active.len() <= 1, "{}: one active request", case.input.name);
        let pending = ids
            .filter(|id| state(*id) == RequestLifecycleState::Pending)
            .filter(|id| !turn.selected.contains(id))
            .collect::<Vec<_>>();
        case.check(Some(
            gents_lean_vocab::families::fold_queue_contracts_handover_claim::HandoverQueue {
                active: active.first().copied(),
                folding: turn.selected.clone(),
                pending,
            },
        ));
    }
}

async fn authored_keys(db: &TestDb, request_doc_id: &str) -> Vec<String> {
    let response = db
        .agent_access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{}" }} }}, order: {{ sequence: ASC }}) {{ message_key }} }}"#,
            escape_graphql_string(request_doc_id)
        ))
        .await
        .unwrap();
    response["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["message_key"].as_str().unwrap().to_owned())
        .collect()
}

async fn durable_facts(db: &TestDb, session_id: &str) -> serde_json::Value {
    let session = escape_graphql_string(session_id);
    let response = db
        .agent_access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}, order: {{ sequence: ASC }}) {{ _docID message_key sequence }}
               AgentOutputSegment(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ _docID }}
               AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ _docID lifecycle_state execution_generation superseded_by_request_doc_id }} }}"#
        ))
        .await
        .unwrap();
    let mut data = response["data"].clone();
    for collection in ["AgentOutputSegment", "AgentRequest"] {
        let rows = data[collection].as_array_mut().unwrap();
        rows.sort_by_key(|row| row["_docID"].as_str().unwrap().to_owned());
    }
    data
}

#[tokio::test]
async fn generated_fold_publication_scripts_bind_to_native_owners() {
    for case in
        gents_lean_vocab::families::fold_queue_contracts_publication_script::cases::<String>()
    {
        let input = &case.input;
        let db = test_db(&input.name).await;
        let session_id = format!("fold-publication-{}", input.name);
        let (_authority_owner, authority) =
            gents_p2p::p2p_reconcile::enrollment_authority_channel();
        let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
            db.node.clone(),
            db.identity.clone(),
            authority,
        );
        let mut bound = RapidHashMap::new();
        let mut arrival_ids = vec![input.head, input.selected];
        let mut saved_message = None;
        let clock_origin = chrono::Utc::now();
        let mut observed_now = None;
        for (arrival, id) in [input.head, input.selected].into_iter().enumerate() {
            let entry = LeanFoldQueueEntry {
                request_id: id,
                execution_origin: "interactive".into(),
                delivery: if id == input.selected {
                    "steer"
                } else {
                    "queue"
                }
                .into(),
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
        let head_doc = bound[&input.head].clone();
        let writer = DefraStreamWriter::new_managed(
            db.node.clone(),
            db.agent_identity.as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        let mut generations: RapidHashMap<u64, ActiveTurn> = RapidHashMap::new();
        let mut first = claim_head(&db, &bound, input.head, &[input.selected]).await;
        first.begin(&writer).await;
        generations.insert(8, first);
        let scope: gents_protocol::rendered_request::CaptureScope = "inference.1".parse().unwrap();
        let mut observations = Vec::new();
        for (index, step) in input.steps.iter().enumerate() {
            let generation = step.generation.unwrap_or_default();
            let before = durable_facts(&db, &session_id).await;
            let accepted = match step.kind.as_str() {
                "observe_deadline" => {
                    let (now, deadline) = (step.now.unwrap(), step.deadline.unwrap());
                    observed_now = Some(clock_origin + chrono::Duration::milliseconds(now as i64));
                    for turn in generations.values_mut() {
                        turn.lifecycle.claimed_deadline_at =
                            Some(clock_origin + chrono::Duration::milliseconds(deadline as i64));
                    }
                    true
                }
                "publish_prompt" | "publish_changed_prompt" => {
                    let content = if step.kind == "publish_prompt" {
                        "prompt"
                    } else {
                        "changed prompt"
                    };
                    writer
                        .publish_authored_message(
                            &generations[&generation].lifecycle,
                            "prompt",
                            &gents_protocol::message::Message::user(content),
                        )
                        .await
                        .is_ok()
                }
                "publish_folded" => writer
                    .publish_authored_message_with_time(
                        &generations[&generation].lifecycle,
                        &folded_input_key(&bound[&step.request_id.unwrap()]),
                        &gents_protocol::message::Message::user(format!(
                            "message {}",
                            step.request_id.unwrap()
                        )),
                        observed_now,
                    )
                    .await
                    .is_ok(),
                "accept_turn" => {
                    writer.start_provider_attempt(&head_doc, 0, 0, scope).await;
                    match writer
                        .publish_native_turn(
                            &generations[&generation].lifecycle,
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
                "recover" => {
                    let (expected, fresh) = (step.expected.unwrap(), step.fresh.unwrap());
                    let mut request = generations[&expected].lifecycle.request().clone();
                    request.deadline = None;
                    // The expired head is the Agent's own row; recovery resets it as the Agent.
                    db.agent_access.write(
                        "test.fold_publication_reclaim",
                        &format!(
                            r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ lifecycle_state: "pending", deadline: null }}) {{ _docID }} }}"#,
                            escape_graphql_string(&head_doc)
                        ),
                    )
                    .await
                    .unwrap();
                    let still_queued = fold_rows(&db, &session_id)
                        .await
                        .iter()
                        .filter(|row| {
                            modeled_id(row) == input.selected
                                && row.lifecycle_state == RequestLifecycleState::Pending
                        })
                        .map(modeled_id)
                        .collect::<Vec<_>>();
                    let mut reclaimed = claim_request(&db, &bound, request, &still_queued).await;
                    reclaimed.begin(&writer).await;
                    generations.insert(fresh, reclaimed);
                    true
                }
                "terminalize" => generations
                    .get_mut(&generation)
                    .unwrap()
                    .lifecycle
                    .terminalize_owned(
                        crate::request::lifecycle::RequestTerminalOutcome::Completed,
                        gents_protocol::output::TerminalOutput::NoMessage,
                        None,
                    )
                    .await
                    .is_ok(),
                "finish" => {
                    generations.clear();
                    true
                }
                "enqueue_steering" => {
                    let request_id = step.request_id.unwrap();
                    let entry = LeanFoldQueueEntry {
                        request_id,
                        execution_origin: "interactive".into(),
                        delivery: "steer".into(),
                        order_key: request_id,
                        source: "user".into(),
                        policy: "append".into(),
                        queued_after: Some(input.head),
                        requester_id: Some(1),
                        turn_context: 0,
                        fresh: true,
                    };
                    let id = enqueue_signed(&db, &session_id, &entry, arrival_ids.len()).await;
                    bound.insert(request_id, id);
                    arrival_ids.push(request_id);
                    true
                }
                "intake" | "finish_or_intake" => {
                    assert!(
                        step.safe_boundary.unwrap(),
                        "unsafe provider boundary belongs to loop owner"
                    );
                    let turn = generations.get_mut(&generation).unwrap();
                    if !turn.lifecycle.owns_execution().await.unwrap()
                        || !crate::request::lifecycle::RequestLifecycle::input_publication_before_deadline(
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
                            None,
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
                        if !selected.is_empty() || step.kind == "intake" {
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
                "cancel_first_pending" => {
                    let access = &db.agent_access;
                    let snapshot =
                        pending_user_queue(access, db.node_did(), &session_id, db.agent_did())
                            .await
                            .unwrap();
                    if let Some(first) = snapshot.entries.first() {
                        let result = replace_pending_user_messages(
                            access,
                            db.agent_identity.as_ref(),
                            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                                db.agent_did(),
                            ),
                            PendingEditActor {
                                node_did: db.node_did(),
                                session_id: &session_id,
                                requester_did: db.agent_did(),
                            },
                            PendingQueueEdit {
                                expected_request_doc_ids: snapshot
                                    .entries
                                    .iter()
                                    .map(|entry| entry.request_doc_id.clone())
                                    .collect(),
                                selected_request_doc_ids: vec![first.request_doc_id.clone()],
                                messages: vec![],
                            },
                            &[],
                        )
                        .await;
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
                kind => panic!("{}: no fixture step for {kind}", input.name),
            };
            let label = format!("{} step {index} {step:?}", input.name);
            if !accepted {
                assert_eq!(
                    durable_facts(&db, &session_id).await,
                    before,
                    "{label}: a rejected step leaves durable state unchanged"
                );
            }
            let keys = authored_keys(&db, &head_doc)
                .await
                .into_iter()
                .filter_map(|key| {
                    key.strip_prefix(&gents_protocol::output::rows::authored_message_key(
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
            let rows = fold_rows(&db, &session_id).await;
            let state = |id: u64| {
                rows.iter()
                    .find(|row| modeled_id(row) == id)
                    .unwrap()
                    .lifecycle_state
            };
            let head_active = matches!(
                state(input.head),
                RequestLifecycleState::Claimed | RequestLifecycleState::Processing
            );
            let folding = generations
                .keys()
                .max()
                .map(|latest| {
                    generations[latest]
                        .selected
                        .iter()
                        .copied()
                        .filter(|id| state(*id) == RequestLifecycleState::Pending)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let pending = arrival_ids
                .iter()
                .copied()
                .filter(|id| *id != input.head)
                .filter(|id| state(*id) == RequestLifecycleState::Pending && !folding.contains(id))
                .collect::<Vec<_>>();
            let terminal = [input.head, input.selected]
                .into_iter()
                .filter(|id| state(*id).is_terminal())
                .collect::<Vec<_>>();
            observations.push(
                gents_lean_vocab::families::fold_queue_contracts_publication_script::PublicationObservation {
                    accepted,
                    active: head_active.then_some(input.head),
                    pending,
                    folding,
                    terminal,
                    authored_keys: keys,
                },
            );
        }
        case.check(Some(observations));
    }
}

#[tokio::test]
async fn claim_selected_publication_keeps_later_incompatible_pending_row() {
    let db = test_db("fold-selected-after-head").await;
    let session_id = "fold-selected-after-head";
    let mut bound = RapidHashMap::new();
    for (arrival, (id, context, queued_after)) in
        [(2, 0, None), (3, 0, Some(2))].into_iter().enumerate()
    {
        let entry = LeanFoldQueueEntry {
            request_id: id,
            execution_origin: "interactive".into(),
            delivery: if id == 3 { "steer" } else { "queue" }.into(),
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
    let writer =
        DefraStreamWriter::new_managed(db.node.clone(), db.agent_identity.as_ref(), Duration::ZERO)
            .unwrap();
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
    let before = fold_rows(&db, session_id).await;
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
    let rows = fold_rows(&db, session_id).await;
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
async fn direct_user_steer_passes_ordinary_queue_without_consuming_it() {
    for case in
        gents_lean_vocab::families::contracts_steering_boundary_publication::cases::<String>()
    {
        let db = test_db("direct-steer-queued-followups").await;
        let session_id = "direct-steer-queued-followups";
        let entry = |id, delivery: &str| LeanFoldQueueEntry {
            request_id: id,
            execution_origin: "interactive".into(),
            delivery: delivery.into(),
            order_key: id,
            source: "user".into(),
            policy: "append".into(),
            queued_after: Some(99),
            requester_id: Some(1),
            turn_context: 0,
            fresh: true,
        };
        let mut bound = RapidHashMap::new();
        let head = LeanFoldQueueEntry {
            queued_after: None,
            ..entry(99, "queue")
        };
        bound.insert(99, enqueue_signed(&db, session_id, &head, 0).await);
        let mut active = claim_head(&db, &bound, 99, &[]).await;
        let writer = DefraStreamWriter::new_managed(
            db.node.clone(),
            db.agent_identity.as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        active.begin(&writer).await;
        for (index, item) in case.input.inputs.iter().enumerate() {
            let delivery = match item.kind.as_str() {
                "user_steer" => "steer",
                "queued" => "queue",
                other => panic!("the served fixture cannot enqueue a {other} input"),
            };
            bound.insert(
                item.id,
                enqueue_signed(&db, session_id, &entry(item.id, delivery), index + 1).await,
            );
        }
        let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
        let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
            db.node.clone(),
            db.identity.clone(),
            authority,
        );
        let snapshot = super::super::intake::steering_snapshot(
            &db.node,
            active.lifecycle.request(),
            &verifier,
            None,
        )
        .await
        .unwrap();
        let steered = snapshot
            .inputs
            .iter()
            .map(|input| {
                case.input
                    .inputs
                    .iter()
                    .map(|item| item.id)
                    .find(|id| folded_input_key(&bound[id]) == input.key)
                    .expect("steering input is one of the enqueued turns")
            })
            .collect::<Vec<_>>();
        for id in &steered {
            writer
                .publish_authored_message(
                    &active.lifecycle,
                    &folded_input_key(&bound[id]),
                    &gents_protocol::message::Message::user(format!("message {id}")),
                )
                .await
                .unwrap();
        }
        let rows = fold_rows(&db, session_id).await;
        let queue = pending_user_queue(&db.agent_access, db.node_did(), session_id, db.node_did())
            .await
            .unwrap();
        let state_of = |id: u64| {
            rows.iter()
                .find(|row| row.request_id == id.to_string())
                .unwrap()
        };
        let ids = case.input.inputs.iter().map(|item| item.id);
        let pending_rows = ids
            .clone()
            .filter(|id| {
                let row = state_of(*id);
                row.lifecycle_state == RequestLifecycleState::Pending
                    && row.superseded_by_request_doc_id.is_none()
            })
            .collect::<Vec<_>>();
        let superseded = ids
            .filter(|id| {
                let row = state_of(*id);
                row.lifecycle_state == RequestLifecycleState::Superseded
                    && row.superseded_by_request_doc_id.as_deref() == Some(bound[&99].as_str())
            })
            .collect::<Vec<_>>();
        let pending = queue
            .entries
            .iter()
            .map(|item| item.request_id.parse::<u64>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pending, pending_rows, "{:?}", case.input);
        case.check(
            gents_lean_vocab::families::contracts_steering_boundary_publication::Publication {
                steered,
                pending,
                superseded,
            },
        );
    }
}

#[tokio::test]
async fn expired_or_malformed_steering_is_a_modelled_publication_barrier() {
    use gents_lean_vocab::LeanQueueManagementOperation;
    let case = gents_lean_vocab::lean_queue_management_cases()
        .iter()
        .find(|case| case.name == "expired_steering_is_barrier")
        .unwrap();
    for (variant, ttl) in ["2000-01-01T00:00:00Z", "not-a-timestamp"]
        .into_iter()
        .enumerate()
    {
        let name = &format!("{}-{variant}", case.name);
        let LeanQueueManagementOperation::Intake { active: head, .. } = &case.operation else {
            panic!("expected intake case");
        };
        let db = test_db(name).await;
        let session_id = name;
        let head_doc = enqueue_signed(&db, session_id, head, 0).await;
        let mut bound = RapidHashMap::from_iter([(head.request_id, head_doc)]);
        let mut active = claim_head(&db, &bound, head.request_id, &[]).await;
        let writer = DefraStreamWriter::new_managed(
            db.node.clone(),
            db.agent_identity.as_ref(),
            Duration::ZERO,
        )
        .unwrap();
        active.begin(&writer).await;
        for (index, entry) in case.before.pending.iter().enumerate() {
            let doc = if entry.fresh {
                enqueue_signed(&db, session_id, entry, index + 1).await
            } else {
                enqueue_with_ttl(&db, session_id, entry, index + 1, Some(ttl)).await
            };
            bound.insert(entry.request_id, doc);
        }
        let (_owner, authority) = gents_p2p::p2p_reconcile::enrollment_authority_channel();
        let verifier = crate::request::admission::AgentRequestAdmissionVerifier::new(
            db.node.clone(),
            db.identity.clone(),
            authority,
        );
        let snapshot = super::super::intake::steering_snapshot(
            &db.node,
            active.lifecycle.request(),
            &verifier,
            None,
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
        let rows = fold_rows(&db, session_id).await;
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

impl ActiveTurn {
    async fn begin(&mut self, writer: &DefraStreamWriter) {
        if !self.begun {
            self.lifecycle.begin_owned_execution(writer).await.unwrap();
            self.begun = true;
        }
    }
}
