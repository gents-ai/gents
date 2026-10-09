use super::*;
use crate::trigger_engine::durable;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
use serde::Deserialize;

#[derive(Clone, Deserialize)]
struct Fire {
    identity: FireIdentity,
    session: String,
    serial: bool,
    emit_outcome: bool,
    goal_backed: bool,
}

#[derive(Clone, Deserialize)]
struct Request {
    fire: Fire,
    running: bool,
    terminal: bool,
    assignment_replaced: bool,
    goal_assignment_applied: bool,
}

#[derive(Clone, Deserialize)]
struct GoalBinding {
    owner: String,
    session: String,
    assignment: FireIdentity,
    status: String,
}
impl GoalBinding {
    fn observation(&self) -> durable::GoalOutcomeBinding {
        durable::GoalOutcomeBinding {
            owner_did: self.owner.clone(),
            session_id: self.session.clone(),
            assignment_request_id: self.assignment.request_id(),
            status: self.status.clone(),
        }
    }
}

#[derive(Deserialize)]
struct State {
    receipts: Vec<FireIdentity>,
    requests: Vec<Request>,
    outcomes: Vec<FireIdentity>,
    goals: Vec<GoalBinding>,
}

fn contract() -> serde_json::Value {
    gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap()["trigger_delivery"]
        .clone()
}

fn decode<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}

fn receipt(fire: &Fire, index: usize) -> TriggerFire {
    let key = durable::fire_key(&fire.identity);
    TriggerFire {
        fire_key: key.clone(),
        identity: fire.identity.clone(),
        task_id: "contract-task".into(),
        request_id: fire.identity.request_id(),
        session_id: fire.session.clone(),
        goal_id: fire
            .goal_backed
            .then(|| crate::goal::deterministic_goal_id(&fire.identity.owner_did, &fire.session)),
        goal_objective: fire.goal_backed.then(|| "contract objective".into()),
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: fire.emit_outcome,
        queued_serial: fire.serial,
        source_handoff_id: Some(format!("source:{}", fire.identity.source_doc_id)),
        reply_session_id: Some("reply-session".into()),
        shard_id: None,
        attempt: None,
        created_at: format!("2030-01-01T00:00:{index:02}Z"),
    }
}

fn request_mutation(receipt: &TriggerFire) -> String {
    format!(
        r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{}", node_did: "{}", session_id: "{}",
        agent_id: "general", content: "contract task", purpose: "normal", lifecycle_state: "pending",
        created_at: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&receipt.request_id),
        escape_graphql_string(&receipt.identity.owner_did),
        escape_graphql_string(&receipt.session_id),
        escape_graphql_string(&receipt.created_at)
    )
}

#[test]
fn durable_delivery_predicates_match_executable_lean_owners() {
    let cases = contract();
    for case in cases["identities"].as_array().unwrap() {
        let id: FireIdentity = decode(&case["identity"]);
        let key = durable::fire_key(&id);
        assert_eq!(key, case["key"].as_str().unwrap());
        assert_eq!(id.request_id(), case["request_id"]);
        assert_eq!(id.session_id(), case["session_id"]);
        assert_eq!(id.outcome_id(), case["outcome_id"]);
    }
    for case in cases["sessions"].as_array().unwrap() {
        let id: FireIdentity = decode(&case["identity"]);
        assert_eq!(
            durable::resolve_session_id(
                &id,
                case["target"].as_str(),
                case["owned"].as_bool().unwrap()
            ),
            decode::<Option<String>>(&case["resolved"])
        );
    }
    for case in cases["outcomes"].as_array().unwrap() {
        let state: State = decode(&case["pre"]);
        let request = &state.requests[0];
        let mut fire = receipt(&request.fire, 0);
        fire.goal_assignment_applied = request.goal_assignment_applied;
        let binding = state.goals.first().map(GoalBinding::observation);
        let reason = durable::fire_outcome_reason(
            &fire,
            request.terminal,
            request.assignment_replaced,
            binding.as_ref(),
        );
        assert_eq!(
            reason,
            decode::<Option<String>>(&case["reason"]),
            "{}",
            case["name"]
        );
        assert_eq!(
            reason.is_some(),
            case["due"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
    for case in cases["queues"].as_array().unwrap() {
        let identity: FireIdentity = decode(&case["identity"]);
        let rows: Vec<durable::ClaimObservation> = decode(&case["observations"]);
        let candidate = rows
            .iter()
            .find(|row| row.document == identity.fire_key())
            .unwrap();
        assert_eq!(
            durable::observed_claim_allowed(candidate, &rows),
            case["can_claim"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
    for case in cases["self_sessions"].as_array().unwrap() {
        assert_eq!(
            crate::toolset::session_history::is_current_session(
                case["caller_owner"].as_str().unwrap(),
                case["caller_session"].as_str().unwrap(),
                case["listed_owner"].as_str().unwrap(),
                case["listed_session"].as_str().unwrap()
            ),
            case["current"].as_bool().unwrap()
        );
    }
}

#[tokio::test]
async fn generated_fire_transactions_are_atomic_and_owner_scoped() {
    for case in contract()["admissions"].as_array().unwrap() {
        let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
        ensure_runtime_schemas(&node).await.unwrap();
        let pre: State = decode(&case["pre"]);
        for (index, request) in pre.requests.iter().enumerate() {
            let fire = receipt(&request.fire, index);
            let mutation = request_mutation(&fire);
            crate::config_client::ConfigAccess::transact_local(
                &node,
                None,
                "test.seed_fire",
                |txn| Box::pin(async { durable::stage_fire_request(txn, &fire, &mutation).await }),
            )
            .await
            .unwrap();
        }
        let f: Fire = decode(&case["fire"]);
        let fire = receipt(&f, pre.requests.len());
        let mutation = request_mutation(&fire);
        let commit = case["commit"].as_bool().unwrap();
        let result = crate::config_client::ConfigAccess::transact_local(
            &node,
            None,
            "test.fire_crash_boundary",
            |txn| {
                Box::pin(async {
                    durable::stage_fire_request(txn, &fire, &mutation).await?;
                    anyhow::ensure!(commit, "injected pre-commit crash");
                    Ok(())
                })
            },
        )
        .await;
        if case["source_allowed"].as_bool().unwrap() {
            assert_eq!(result.is_ok(), commit, "{}", case["name"]);
        } else {
            assert!(result.is_err(), "outcome chaining must reject admission");
        }
        let response = crate::graphql::graphql_with_transaction_retry(
            &node,
            "{ TriggerFire { fire_key } AgentRequest { request_id } }",
            "test.fire_receipts",
        )
        .await
        .unwrap();
        let post: State = decode(&case["post"]);
        let data = response.data.unwrap();
        assert_eq!(
            data["TriggerFire"].as_array().unwrap().len(),
            post.receipts.len(),
            "{}",
            case["name"]
        );
        assert_eq!(
            data["AgentRequest"].as_array().unwrap().len(),
            post.requests.len(),
            "{}",
            case["name"]
        );
        assert!(post.outcomes.is_empty());
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OutcomeAction {
    Admit {
        fire: Fire,
    },
    Claim {
        identity: FireIdentity,
    },
    RequestTerminal {
        identity: FireIdentity,
        terminal_state: String,
        commit: bool,
    },
    GoalStatus {
        identity: FireIdentity,
        status: String,
    },
    Recover {
        commit: bool,
    },
}

async fn persisted_outcome_requests(
    access: &crate::config_client::ConfigAccess,
) -> Vec<gents_protocol::row::AgentRequestRow> {
    let response = access
        .execute(&format!(
            "{{AgentRequest {{{} lifecycle_state}}}}",
            crate::watcher::AGENT_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap()
}

#[tokio::test]
async fn generated_terminal_outcome_action_traces_use_native_owners() {
    use crate::config_client::ConfigAccess;
    for trace in contract()["outcome_traces"].as_array().unwrap() {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        let mut published = std::collections::BTreeMap::new();
        let mut terminal_states = std::collections::BTreeMap::new();
        for (index, step) in trace["steps"].as_array().unwrap().iter().enumerate() {
            let action: OutcomeAction = decode(&step["action"]);
            let expected: State = decode(&step["post"]);
            match action {
                OutcomeAction::Admit { fire } => {
                    let receipt = receipt(&fire, index);
                    let mutation = request_mutation(&receipt);
                    let result = access
                        .transact("test.outcome_trace_admit", |txn| {
                            let receipt = &receipt;
                            let mutation = &mutation;
                            Box::pin(async move {
                                durable::stage_fire_request(txn, receipt, mutation).await
                            })
                        })
                        .await;
                    assert_eq!(
                        result.is_ok(),
                        expected.receipts.contains(&fire.identity),
                        "{}",
                        trace["name"]
                    );
                }
                OutcomeAction::Claim { identity } => {
                    let rows = persisted_outcome_requests(&access).await;
                    let row = rows
                        .iter()
                        .find(|row| row.request_id == identity.request_id())
                        .unwrap();
                    let request = crate::watcher::AgentRequest::try_from(row.clone()).unwrap();
                    let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
                        node.clone(),
                        "general",
                        &identity.owner_did,
                        request,
                        60,
                        crate::lifecycle::ExecutionOrigin::Interactive,
                        "test-backend",
                    );
                    let now = chrono::Utc::now();
                    assert!(
                        lifecycle
                            .claim_pending_durable_with_inputs(
                                || now,
                                || (now, uuid::Uuid::new_v4().to_string())
                            )
                            .await
                            .unwrap()
                            .was_claimed(),
                        "{}",
                        trace["name"]
                    );
                }
                OutcomeAction::RequestTerminal {
                    identity,
                    terminal_state,
                    commit,
                } => {
                    if commit {
                        terminal_states.insert(identity.request_id(), terminal_state.clone());
                    }
                    let result = access.transact("test.inject_terminal_publication_gap", |txn| {
                        let identity = &identity;
                        let terminal_state = &terminal_state;
                        Box::pin(async move {
                            txn.execute(&format!("mutation {{update_AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}, input: {{lifecycle_state: \"{}\"}}) {{_docID}}}}", escape_graphql_string(&identity.request_id()), escape_graphql_string(terminal_state))).await?;
                            anyhow::ensure!(commit, "injected terminal transaction crash"); Ok(())
                        })
                    }).await;
                    assert_eq!(result.is_ok(), commit);
                }
                OutcomeAction::GoalStatus { identity, status } => {
                    let rows = persisted_outcome_requests(&access).await;
                    let request = rows
                        .iter()
                        .find(|row| row.request_id == identity.request_id())
                        .unwrap();
                    access.write("test.inject_goal_publication_gap", &format!("mutation {{update_Goal(filter: {{node_did: {{_eq: \"{}\"}}, session_id: {{_eq: \"{}\"}}}}, input: {{status: \"{}\"}}) {{_docID}}}}",
                        escape_graphql_string(&identity.owner_did), escape_graphql_string(request.session_id.as_deref().unwrap()), escape_graphql_string(&status))).await.unwrap();
                }
                OutcomeAction::Recover { commit } => {
                    if commit {
                        durable::recover_outcomes(&node, "owner-a").await.unwrap();
                    } else {
                        let result: anyhow::Result<()> = access
                            .transact("test.outcome_publication_rollback", |txn| {
                                Box::pin(async move {
                                    durable::recover_outcomes_in_txn(txn, "owner-a").await?;
                                    anyhow::bail!("injected outcome transaction crash")
                                })
                            })
                            .await;
                        assert!(result.is_err());
                    }
                }
            }
            for publication in step["published"].as_array().unwrap() {
                let identity: FireIdentity = decode(&publication["identity"]);
                published.insert(
                    identity.outcome_id(),
                    publication["reason"].as_str().unwrap().to_owned(),
                );
            }
            let response = access.execute("{FireOutcome {handoff_id terminal_state} TriggerFire {fire_key goal_assignment_applied} Goal {node_did session_id assignment_root_request_doc_id status}}").await.unwrap();
            let outcomes = response["data"]["FireOutcome"].as_array().unwrap();
            assert_eq!(
                outcomes.len(),
                expected.outcomes.len(),
                "{} step {index}",
                trace["name"]
            );
            for id in &expected.outcomes {
                let outcome = outcomes
                    .iter()
                    .find(|row| row["handoff_id"] == id.outcome_id())
                    .unwrap();
                let reason = &published[&id.outcome_id()];
                assert_eq!(
                    outcome["terminal_state"],
                    if reason == "request_terminal" {
                        terminal_states[&id.request_id()].as_str()
                    } else {
                        reason.as_str()
                    },
                    "{} step {index}",
                    trace["name"]
                );
            }
            let requests = persisted_outcome_requests(&access).await;
            assert_eq!(
                requests.len(),
                expected.requests.len(),
                "{} step {index}",
                trace["name"]
            );
            for expected_request in &expected.requests {
                let row = requests
                    .iter()
                    .find(|row| row.request_id == expected_request.fire.identity.request_id())
                    .unwrap();
                assert_eq!(
                    row.lifecycle_state.is_some_and(|state| state.is_terminal()),
                    expected_request.terminal,
                    "{} step {index}: request {} terminal state",
                    trace["name"],
                    row.request_id
                );
                assert_eq!(
                    row.lifecycle_state.is_some_and(|state| matches!(
                        state,
                        gents_protocol::request_lifecycle::RequestLifecycleState::Claimed
                            | gents_protocol::request_lifecycle::RequestLifecycleState::Processing
                    )),
                    expected_request.running,
                    "{} step {index}: request {} running state",
                    trace["name"],
                    row.request_id
                );
                let receipts = response["data"]["TriggerFire"].as_array().unwrap();
                let receipt = receipts
                    .iter()
                    .find(|row| row["fire_key"] == expected_request.fire.identity.fire_key())
                    .unwrap();
                assert_eq!(
                    receipt["goal_assignment_applied"]
                        .as_bool()
                        .unwrap_or(false),
                    expected_request.goal_assignment_applied,
                    "{} step {index}: request {} Goal assignment",
                    trace["name"],
                    row.request_id
                );
            }
            let goals = response["data"]["Goal"].as_array().unwrap();
            assert_eq!(goals.len(), expected.goals.len());
            for binding in &expected.goals {
                let goal = goals
                    .iter()
                    .find(|row| {
                        row["node_did"] == binding.owner && row["session_id"] == binding.session
                    })
                    .unwrap();
                let assignment = requests
                    .iter()
                    .find(|row| row.request_id == binding.assignment.request_id())
                    .unwrap();
                assert_eq!(
                    goal["assignment_root_request_doc_id"].as_str(),
                    assignment.doc_id.as_deref()
                );
                assert_eq!(goal["status"], binding.status);
            }
        }
    }
}

#[derive(Clone, Deserialize)]
struct Arrival {
    position: String,
    identity: FireIdentity,
}

async fn create_arrival_source(
    access: &crate::config_client::ConfigAccess,
    label: &str,
    eligible: bool,
) -> String {
    let response = access
        .write(
            "test.arrival_document",
            &format!(
                "mutation {{ create_Work(input: {{label: \"{}\", eligible: {eligible}}}) {{ _docID }} }}",
                escape_graphql_string(label),
            ),
        )
        .await
        .unwrap();
    crate::graphql::created_doc_id(&response, "Work").unwrap()
}

fn handoff_consumer() -> gents_protocol::event_delivery::EventConsumer {
    gents_protocol::event_delivery::EventConsumer::Trigger {
        trigger_id: "handoff".into(),
    }
}

async fn saved_arrival_cursor(access: &crate::config_client::ConfigAccess) -> String {
    access
        .transact("test.read_arrival_cursor", |txn| {
            Box::pin(async move {
                Ok(crate::config_client::event_source_cursor::load_or_seed(
                    txn,
                    "owner-a",
                    &handoff_consumer(),
                )
                .await?
                .cursor
                .after)
            })
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn generated_arrival_checkpoints_preserve_committed_delivery_across_crashes() {
    use crate::config_client::{event_source_cursor, ConfigAccess};
    for case in contract()["cursors"].as_array().unwrap() {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        access
            .add_schema("type Work { label: String eligible: Boolean }")
            .await
            .unwrap();
        let source: Vec<Arrival> = decode(&case["source"]);
        let seed_head = case["seed_head"]
            .as_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mut documents = std::collections::BTreeMap::new();
        for entry in source.iter().take(seed_head) {
            documents.insert(
                entry.identity.source_doc_id.clone(),
                create_arrival_source(
                    &access,
                    &entry.identity.source_doc_id,
                    case["source_eligible"][entry.position.parse::<usize>().unwrap() - 1]
                        .as_bool()
                        .unwrap(),
                )
                .await,
            );
        }
        access.transact("test.arrival_config", |txn| Box::pin(async move {
            txn.execute_with_variables(
                "mutation($input:EventSourceMutationInputArg!){create_EventSource(input:$input){_docID}}",
                &serde_json::json!({"input":{"node_did":"owner-a","event_source_id":"source",
                    "source_collection":"Work","event_kind":"created", "filter":"{eligible: {_eq: true}}"}}),
            ).await?;
            txn.execute_with_variables(
                "mutation($input:TriggerMutationInputArg!){create_Trigger(input:$input){_docID}}",
                &serde_json::json!({"input":{"node_did":"owner-a","trigger_id":"handoff",
                    "task_id":"contract-task","source":{"kind":"event","event_source_id":"source"},
                    "enabled":case["enabled"], "concurrency":case["mode"]}}),
            ).await?;
            Ok(())
        })).await.unwrap();
        assert_eq!(
            saved_arrival_cursor(&access).await,
            seed_head.to_string(),
            "{}",
            case["name"]
        );
        for entry in source.iter().skip(seed_head) {
            documents.insert(
                entry.identity.source_doc_id.clone(),
                create_arrival_source(
                    &access,
                    &entry.identity.source_doc_id,
                    case["source_eligible"][entry.position.parse::<usize>().unwrap() - 1]
                        .as_bool()
                        .unwrap(),
                )
                .await,
            );
        }
        let pre_after = case["pre_cursor"]["after"].as_str().unwrap();
        access
            .transact("test.prior_checkpoint", |txn| {
                Box::pin(async move {
                    event_source_cursor::advance(
                        txn,
                        "owner-a",
                        &handoff_consumer(),
                        "Work",
                        pre_after,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        if case["restart"].as_bool().unwrap() {
            assert_eq!(
                saved_arrival_cursor(&access).await,
                pre_after,
                "{}",
                case["name"]
            );
        }
        let adapt_fire = |mut fire: Fire| {
            fire.identity.source_doc_id = documents[&fire.identity.source_doc_id].clone();
            fire
        };
        let pre: State = decode(&case["pre"]);
        for (index, request) in pre.requests.iter().enumerate() {
            let receipt = receipt(&adapt_fire(request.fire.clone()), index);
            let mutation = request_mutation(&receipt);
            access
                .transact("test.prior_arrival_admission", |txn| {
                    let receipt = &receipt;
                    let mutation = &mutation;
                    Box::pin(async move {
                        durable::stage_fire_request(txn, &receipt, &mutation).await?;
                        Ok(())
                    })
                })
                .await
                .unwrap();
        }
        if let Some(commit) = case["admission_commit"].as_bool() {
            let receipt = receipt(&adapt_fire(decode(&case["fire"])), pre.requests.len());
            let mutation = request_mutation(&receipt);
            let admitted: anyhow::Result<()> = access
                .transact("test.arrival_admission_crash", |txn| {
                    let receipt = &receipt;
                    let mutation = &mutation;
                    Box::pin(async move {
                        durable::stage_fire_request(txn, &receipt, &mutation).await?;
                        anyhow::ensure!(commit, "injected crash before receipt/request commit");
                        Ok(())
                    })
                })
                .await;
            assert_eq!(admitted.is_ok(), commit, "{}: {admitted:?}", case["name"]);
        }
        if let Some(commit) = case["checkpoint_commit"].as_bool() {
            let entry: Arrival = decode(&case["entry"]);
            let busy = case["busy"].as_bool().unwrap();
            let checkpointed: anyhow::Result<bool> = access
                .transact("test.arrival_checkpoint_crash", |txn| {
                    let entry = &entry;
                    Box::pin(async move {
                        let accepted = event_source_cursor::checkpoint_prefix(
                            txn,
                            "owner-a",
                            &handoff_consumer(),
                            "Work",
                            &entry.position,
                            busy,
                        )
                        .await?;
                        anyhow::ensure!(commit, "injected crash before checkpoint commit");
                        Ok(accepted)
                    })
                })
                .await;
            let accepted = if commit {
                checkpointed.unwrap_or_else(|error| {
                    panic!("{}: unexpected checkpoint error: {error:#}", case["name"])
                })
            } else {
                let error = checkpointed.expect_err("injected checkpoint must roll back");
                assert!(
                    format!("{error:#}").contains("injected crash before checkpoint commit"),
                    "{}: unexpected rollback error: {error:#}",
                    case["name"]
                );
                false
            };
            assert_eq!(
                accepted,
                case["checkpoint_succeeds"].as_bool().unwrap(),
                "{}",
                case["name"]
            );
        }
        let after = saved_arrival_cursor(&access).await;
        assert_eq!(
            after,
            case["post_cursor"]["after"].as_str().unwrap(),
            "{}",
            case["name"]
        );
        let page = access.execute(&format!(
            "{{ _documentArrivals(collection: \"Work\", after: \"{}\", limit: 128) {{ entries {{ cursor docID }} }} }}",
            escape_graphql_string(&after),
        )).await.unwrap();
        let actual = page["data"]["_documentArrivals"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["cursor"].as_str().unwrap().to_owned(),
                    entry["docID"].as_str().unwrap().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let expected = decode::<Vec<Arrival>>(&case["journal_after"])
            .iter()
            .map(|entry| {
                (
                    entry.position.clone(),
                    documents[&entry.identity.source_doc_id].clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{}", case["name"]);
        let rows = access
            .execute("{ TriggerFire { source_doc_id } AgentRequest { request_id } }")
            .await
            .unwrap();
        let post: State = decode(&case["post"]);
        assert_eq!(
            rows["data"]["TriggerFire"].as_array().unwrap().len(),
            post.receipts.len(),
            "{}",
            case["name"]
        );
        assert_eq!(
            rows["data"]["AgentRequest"].as_array().unwrap().len(),
            post.requests.len(),
            "{}",
            case["name"]
        );
    }
}

fn handoff_binding() -> gents_protocol::event_delivery::EventConsumer {
    gents_protocol::event_delivery::EventConsumer::CallbackBinding {
        binding_id: "handoff".into(),
    }
}

/// Admits `doc_id` for the `handoff` binding through the invocation owner,
/// freezing `version` as its source version.
async fn admit_callback_arrival(node: &defra_node::EmbeddedNode, doc_id: &str, version: &str) {
    crate::callback::create_pending_invocation(
        node,
        &crate::callback::CallbackInvocationDoc {
            input: serde_json::json!({}),
            invocation_id: uuid::Uuid::new_v4().to_string(),
            owner_node_did: "owner-a".into(),
            callback_id: "callback".into(),
            origin: crate::document_config::CallbackInvocationOrigin::Event {
                binding_id: "handoff".into(),
                source_collection: "Work".into(),
                source_doc_id: doc_id.into(),
                source_version: Some(version.into()),
            },
            idempotency_key: crate::callback::idempotency_key("handoff", "Work", doc_id),
            caused_by_correlation: None,
            lifecycle_state: crate::callback::LIFECYCLE_PENDING.into(),
            attempts: Some(0),
            action_plan: None,
            action_journal: None,
            error: None,
            claimed_at: None,
            created_at: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn generated_callback_arrival_checkpoints_use_invocation_receipts() {
    use crate::config_client::{event_source_cursor, ConfigAccess};
    for case in contract()["callback_cursors"].as_array().unwrap() {
        let name = &case["name"];
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        ensure_runtime_schemas(&node).await.unwrap();
        let access = ConfigAccess::Local(node.clone());
        access
            .add_schema("type Work { label: String eligible: Boolean }")
            .await
            .unwrap();
        let source: Vec<Arrival> = decode(&case["source"]);
        let seed_head: usize = case["seed_head"].as_str().unwrap().parse().unwrap();
        let eligible = case["matches_filter"].as_bool().unwrap();
        let mut documents = std::collections::BTreeMap::new();
        for entry in source.iter().take(seed_head) {
            let label = &entry.identity.source_doc_id;
            documents.insert(
                label.clone(),
                create_arrival_source(&access, label, eligible).await,
            );
        }
        let binding_enabled = case["binding_enabled"].as_bool().unwrap();
        let callback_enabled = case["callback_enabled"].as_bool().unwrap();
        access.transact("test.callback_arrival_config", |txn| Box::pin(async move {
            txn.execute_with_variables(
                "mutation($input:CallbackMutationInputArg!){create_Callback(input:$input){_docID}}",
                &serde_json::json!({"input":{"node_did":"owner-a","callback_id":"callback",
                    "enabled":callback_enabled,
                    "handler":{"kind":"built_in","emitter":"create_workspace"}}}),
            ).await?;
            txn.execute_with_variables(
                "mutation($input:EventSourceMutationInputArg!){create_EventSource(input:$input){_docID}}",
                &serde_json::json!({"input":{"node_did":"owner-a","event_source_id":"source",
                    "source_collection":"Work","event_kind":"created", "filter":"{eligible: {_eq: true}}"}}),
            ).await?;
            txn.execute_with_variables(
                "mutation($input:CallbackBindingMutationInputArg!){create_CallbackBinding(input:$input){_docID}}",
                &serde_json::json!({"input":{"node_did":"owner-a","binding_id":"handoff",
                    "event_source_id":"source","callback_id":"callback","enabled":binding_enabled}}),
            ).await?;
            event_source_cursor::load_or_seed(txn, "owner-a", &handoff_binding()).await?;
            Ok(())
        })).await.unwrap();
        let saved = || {
            let access = access.clone();
            async move {
                access
                    .transact("test.read_callback_cursor", |txn| {
                        Box::pin(async move {
                            Ok(event_source_cursor::load_or_seed(
                                txn,
                                "owner-a",
                                &handoff_binding(),
                            )
                            .await?
                            .cursor
                            .after)
                        })
                    })
                    .await
                    .unwrap()
            }
        };
        assert_eq!(saved().await, seed_head.to_string(), "{name}");
        for entry in source.iter().skip(seed_head) {
            let label = &entry.identity.source_doc_id;
            documents.insert(
                label.clone(),
                create_arrival_source(&access, label, eligible).await,
            );
        }
        let pre_after = case["pre_cursor"]["after"].as_str().unwrap();
        access
            .transact("test.prior_callback_checkpoint", |txn| {
                Box::pin(async move {
                    event_source_cursor::advance(
                        txn,
                        "owner-a",
                        &handoff_binding(),
                        "Work",
                        pre_after,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        if case["restart"].as_bool().unwrap() {
            assert_eq!(saved().await, pre_after, "{name}");
        }
        for receipt in decode::<Vec<FireIdentity>>(&case["pre_receipts"]) {
            admit_callback_arrival(&node, &documents[&receipt.source_doc_id], "v1").await;
        }
        let entry: Arrival = decode(&case["entry"]);
        let doc_id = documents[&entry.identity.source_doc_id].clone();
        // An uncommitted admission leaves no invocation behind.
        if case["admission_commit"].as_bool() == Some(true) {
            admit_callback_arrival(&node, &doc_id, "v1").await;
            if case["edit_after_admission"].as_bool().unwrap() {
                access
                    .write(
                        "test.callback_arrival_edit",
                        &format!(
                            "mutation {{ update_Work(docID: \"{}\", input: {{label: \"edited\"}}) {{ _docID }} }}",
                            escape_graphql_string(&doc_id)
                        ),
                    )
                    .await
                    .unwrap();
                admit_callback_arrival(&node, &doc_id, "v2").await;
            }
        }
        if let Some(commit) = case["checkpoint_commit"].as_bool() {
            let checkpointed: anyhow::Result<bool> = access
                .transact("test.callback_checkpoint_crash", |txn| {
                    let entry = &entry;
                    Box::pin(async move {
                        let accepted = event_source_cursor::checkpoint_prefix(
                            txn,
                            "owner-a",
                            &handoff_binding(),
                            "Work",
                            &entry.position,
                            false,
                        )
                        .await?;
                        anyhow::ensure!(commit, "injected crash before checkpoint commit");
                        Ok(accepted)
                    })
                })
                .await;
            let accepted = if commit {
                checkpointed.unwrap_or_else(|error| panic!("{name}: checkpoint error: {error:#}"))
            } else {
                assert!(
                    checkpointed.is_err(),
                    "{name}: injected checkpoint must roll back"
                );
                false
            };
            assert_eq!(
                accepted,
                case["checkpoint_succeeds"].as_bool().unwrap(),
                "{name}"
            );
        }
        let after = saved().await;
        assert_eq!(
            after,
            case["post_cursor"]["after"].as_str().unwrap(),
            "{name}"
        );
        let page = access.execute(&format!(
            "{{ _documentArrivals(collection: \"Work\", after: \"{}\", limit: 128) {{ entries {{ cursor docID }} }} }}",
            escape_graphql_string(&after),
        )).await.unwrap();
        let actual = page["data"]["_documentArrivals"]["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["docID"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let expected = decode::<Vec<Arrival>>(&case["journal_after"])
            .iter()
            .map(|entry| documents[&entry.identity.source_doc_id].clone())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "{name}");
        let invocations = access
            .execute("{ CallbackInvocation { origin } }")
            .await
            .unwrap();
        assert_eq!(
            invocations["data"]["CallbackInvocation"]
                .as_array()
                .unwrap()
                .len(),
            case["post_receipts"].as_array().unwrap().len(),
            "{name}"
        );
        node.shutdown().await;
    }
}

#[test]
fn observed_claim_cohorts_match_lean() {
    let contract = gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap();
    for case in contract["trigger_delivery"]["observed_claims"]
        .as_array()
        .unwrap()
    {
        let candidate: durable::ClaimObservation =
            serde_json::from_value(case["candidate"].clone()).unwrap();
        let rows: Vec<durable::ClaimObservation> =
            serde_json::from_value(case["rows"].clone()).unwrap();
        assert_eq!(
            durable::observed_claim_allowed(&candidate, &rows),
            case["allowed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn task_goal_assignment_root_matches_lean() {
    let contract = gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap();
    for case in contract["trigger_delivery"]["assignment_roots"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            crate::goal::assignment_allows(case["assigned"].as_str(), case["observed"].as_str()),
            case["allowed"].as_bool().unwrap(),
            "{case}"
        );
    }
}
