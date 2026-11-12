use super::*;
use crate::triggers::engine::durable;
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
    gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap()
}

fn decode<T>(value: &serde_json::Value) -> T
where
    T: serde::de::DeserializeOwned,
{
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
        goal_id: fire.goal_backed.then(|| {
            gents_protocol::goal::deterministic_goal_id(&fire.identity.owner_did, &fire.session)
        }),
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
    request_mutation_as(receipt, &spy_test_agent().agent_did)
}

fn request_mutation_as(receipt: &TriggerFire, agent_did: &str) -> String {
    format!(
        r#"mutation {{ create_AgentRequest(input: {{
        request_id: "{}", node_did: "{}", session_id: "{}",
        agent_did: "{}", content: "contract task", purpose: "normal", lifecycle_state: "pending",
        created_at: "{}"
    }}) {{ _docID }} }}"#,
        escape_graphql_string(&receipt.request_id),
        escape_graphql_string(&receipt.identity.owner_did),
        escape_graphql_string(&receipt.session_id),
        escape_graphql_string(agent_did),
        escape_graphql_string(&receipt.created_at)
    )
}

#[test]
fn durable_delivery_predicates_match_executable_lean_owners() {
    let cases = contract();
    for case in cases["trigger_delivery_option_cases"].as_array().unwrap() {
        let kind = match case["kind"].as_str().unwrap() {
            "schedule" => TriggerKind::Schedule,
            "event" => TriggerKind::Event,
            "manual" => TriggerKind::Manual,
            other => panic!("unknown modeled trigger kind {other}"),
        };
        assert_eq!(
            gents_documents::task_delivery::validate_task_delivery_options(
                kind,
                case["emit_outcome"].as_bool().unwrap(),
                decode(&case["concurrency"]),
                case["target_existing"].as_bool().unwrap(),
            )
            .is_ok(),
            case["allowed"].as_bool().unwrap(),
            "{case}"
        );
    }
    gents_lean_vocab::families::trigger_delivery_fire_identity::replay(|input| {
        let id: FireIdentity = gents_lean_vocab::family::recode(&input.identity);
        gents_lean_vocab::families::trigger_delivery_fire_identity::FireIdentityKeys {
            key: durable::fire_key(&id),
            request_id: id.request_id(),
            session_id: id.session_id(),
            outcome_id: id.outcome_id(),
        }
    });
    for case in cases["trigger_delivery_session_cases"].as_array().unwrap() {
        let id: FireIdentity = decode(&case["identity"]);
        assert_eq!(
            durable::resolve_session_id(
                &id,
                case["target"].as_str(),
                case["owned"].as_bool().unwrap(),
                case["create_missing"].as_bool().unwrap()
            ),
            decode::<Option<String>>(&case["resolved"])
        );
    }
    gents_lean_vocab::families::trigger_delivery_outcome_reasons::replay(|input| {
        let state: State = gents_lean_vocab::family::recode(&input.pre);
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
        gents_lean_vocab::families::trigger_delivery_outcome_reasons::OutcomeReasonOutput {
            due: reason.is_some(),
            reason,
        }
    });
    for case in cases["trigger_delivery_retained_outcome_cases"]
        .as_array()
        .unwrap()
    {
        let request: Request = decode(&case["request"]);
        let mut fire = receipt(&request.fire, 0);
        fire.goal_assignment_applied = request.goal_assignment_applied;
        let retained: Option<GoalBinding> = decode(&case["retained"]);
        let current: Option<GoalBinding> = decode(&case["current"]);
        let retained = retained.as_ref().map(GoalBinding::observation);
        let current = current.as_ref().map(GoalBinding::observation);
        let binding =
            durable::select_goal_outcome_binding(&fire, retained.as_ref(), current.as_ref());
        assert_eq!(
            durable::fire_outcome_reason(
                &fire,
                request.terminal,
                durable::assignment_replaced(&fire, binding.as_ref()),
                binding.as_ref()
            ),
            decode::<Option<String>>(&case["classification"]),
            "{}",
            case["name"]
        );
    }
    gents_lean_vocab::families::trigger_delivery_queue_claims::replay(|input| {
        let identity: FireIdentity = gents_lean_vocab::family::recode(&input.identity);
        let rows: Vec<gents_session::request::lifecycle::arrival_order::ClaimObservation> =
            gents_lean_vocab::family::recode(&input.observations);
        let candidate = rows
            .iter()
            .find(|row| row.document == identity.fire_key())
            .unwrap();
        gents_lean_vocab::families::trigger_delivery_queue_claims::QueueClaimOutput {
            can_claim: gents_session::request::lifecycle::arrival_order::observed_claim_allowed(
                candidate, &rows,
            ),
        }
    });
    for case in cases["trigger_delivery_self_session_cases"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            gents_tools::native::session_history::is_current_session(
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
async fn generated_latest_delivery_admission_preserves_rejected_and_replayed_work() {
    for (case_index, case) in contract()["trigger_delivery_latest_admission_cases"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        for pending_state in [
            gents_protocol::request_lifecycle::RequestLifecycleState::Pending,
            gents_protocol::request_lifecycle::RequestLifecycleState::WorkspaceBindingPending,
        ] {
            let (db, _user, _client, behavior) = signed_materializer_agent(&format!(
                "latest-delivery-{case_index}-{}",
                pending_state.as_str()
            ))
            .await;
            let node = db.node.clone();
            let access = agent_access(&node, &behavior);
            let owner = behavior.node_did().to_owned();
            let agent_did = behavior.agent_did.clone();
            let pre: State = decode(&case["pre"]);
            let next: Fire = decode(&case["fire"]);
            let allowed = case["destination_allowed"].as_bool().unwrap();
            let mut task = resolved_task("replace work");
            task.agent_did = agent_did.clone();
            if allowed {
                access
                    .transact_write("test.latest_delivery_session", |txn| {
                        let (session, owner, agent_did) = (&next.session, &owner, &agent_did);
                        Box::pin(async move {
                            gents_session::session::ensure_session_in_txn(
                                txn,
                                session,
                                owner,
                                agent_did,
                                Some(owner.as_str()),
                                None,
                                None,
                                &chrono::Utc::now().to_rfc3339(),
                            )
                            .await
                        })
                    })
                    .await
                    .unwrap();
            }
            for (index, request) in pre.requests.iter().enumerate() {
                let mut fire = receipt(&request.fire, index);
                fire.identity.owner_did = owner.clone();
                fire.fire_key = fire.identity.fire_key();
                fire.request_id = fire.identity.request_id();
                fire.task_id = task.task_id.clone();
                access.transact_write(
                "test.seed_latest_delivery", |txn| {
                    let fire = &fire;
                    let owner = &owner;
                    let agent_did = &agent_did;
                    Box::pin(async move {
                        durable::stage_fire_receipt(txn, fire).await?;
                        txn.execute_with_variables(
                            "mutation($input: AgentRequestMutationInputArg!) { create_AgentRequest(input: $input) { _docID } }",
                            &serde_json::json!({"input": {
                                "request_id": fire.request_id, "node_did": owner,
                                "requester_did": owner, "session_id": fire.session_id,
                                "agent_did": agent_did, "purpose": "normal", "content": "prior work",
                                "lifecycle_state": pending_state.as_str(), "caused_by_trigger_id": fire.identity.trigger_id,
                                "caused_by_trigger_kind": "schedule", "created_at": fire.created_at
                            }}),
                        ).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                },
            ).await.unwrap();
            }
            let mut schedule = resolved_schedule(&next.identity.trigger_id, task.clone());
            schedule.session_id_template = Some(next.session.clone());
            schedule.concurrency = ConcurrencyMode::LatestOnly;
            let snapshot = snapshot_with_agent_and_schedules(
                behavior,
                RapidHashMap::from_iter([(next.identity.trigger_id.clone(), schedule)]),
            );
            let (_tx, rx) = watch::channel(snapshot);
            let materializer = Arc::new(ProductionMaterializer::new(node.clone(), rx.clone()));
            let engine = TriggerEngine::new(rx, materializer);
            let result = engine
                .dispatch(FireIntent {
                    trigger_id: Some(next.identity.trigger_id.clone()),
                    trigger_kind: TriggerKind::Schedule,
                    task,
                    concurrency: ConcurrencyMode::LatestOnly,
                    event_vars: serde_json::json!({}),
                    doc_vars: None,
                    correlation: None,
                    group_vars: None,
                    trigger_context: None,
                    args_vars: None,
                    durable_fire_key: next.identity.source_doc_id.clone(),
                    pre_materialized_request_id: None,
                    event_source_generation: None,
                    on_result: Box::new(|_| {}),
                })
                .await;
            assert_eq!(
                matches!(
                    result,
                    FireResult::Fired { .. } | FireResult::Duplicate { .. }
                ),
                allowed,
                "{case}: {result:?}"
            );
            let response = access
                .execute("{ TriggerFire { fire_key } AgentRequest { request_id lifecycle_state } }")
                .await
                .unwrap();
            let post: State = decode(&case["post"]);
            let data = &response["data"];
            assert_eq!(
                data["TriggerFire"].as_array().unwrap().len(),
                post.receipts.len(),
                "{case}"
            );
            let rows = data["AgentRequest"].as_array().unwrap();
            assert_eq!(rows.len(), post.requests.len(), "{case}");
            for request in post.requests {
                let mut id = request.fire.identity;
                let existed = pre.requests.iter().any(|prior| prior.fire.identity == id);
                id.owner_did = owner.clone();
                let row = rows
                    .iter()
                    .find(|row| row["request_id"] == id.request_id())
                    .unwrap();
                assert_eq!(
                    row["lifecycle_state"],
                    if request.terminal {
                        "superseded"
                    } else if existed {
                        pending_state.as_str()
                    } else {
                        "pending"
                    },
                    "{case}"
                );
            }
        }
    }
}
#[tokio::test]
async fn generated_fire_transactions_are_atomic_and_owner_scoped() {
    for case in gents_lean_vocab::families::trigger_delivery_fire_admission::cases() {
        let node_db = gents_inference::test_support::test_db(
            "generated-fire-transactions-are-atomic-a",
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = node_db.node.clone();
        let pre: State = gents_lean_vocab::family::recode(&case.input.pre);
        for (index, request) in pre.requests.iter().enumerate() {
            let fire = receipt(&request.fire, index);
            let lifecycle = if request.terminal {
                "completed"
            } else if request.running {
                "claimed"
            } else {
                "pending"
            };
            let mutation = request_mutation(&fire).replace(
                "lifecycle_state: \"pending\"",
                &format!("lifecycle_state: \"{lifecycle}\""),
            );
            gents_store::config_client::ConfigAccess::transact_local_write(
                &node,
                None,
                "test.seed_fire",
                |txn| Box::pin(async { durable::stage_fire_request(txn, &fire, &mutation).await }),
            )
            .await
            .unwrap();
        }
        let f: Fire = gents_lean_vocab::family::recode(&case.input.fire);
        let fire = receipt(&f, pre.requests.len());
        let mutation = request_mutation(&fire);
        let commit = case.input.commit;
        let result = gents_store::config_client::ConfigAccess::transact_local_write(
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
        let post = observed_delivery_state(
            &gents_store::config_client::ConfigAccess::local(node.clone()),
            FireIdentity::clone,
        )
        .await;
        case.check(
            gents_lean_vocab::families::trigger_delivery_fire_admission::AdmissionOutput {
                source_allowed: gents_documents::event_trigger::outcome_source_allowed(
                    &fire.identity.source_collection,
                    fire.emit_outcome,
                ),
                committed: result.is_ok(),
                post: gents_lean_vocab::family::recode(&post),
            },
        );
    }
}

/// The trigger-delivery state production holds: receipts in admission order,
/// the admitted request of each receipt, published outcomes in receipt order and
/// Goal bindings whose assignment is an admitted fire. `modeled` maps a stored
/// identity back to the modeled one.
async fn observed_delivery_state<F>(
    access: &gents_store::config_client::ConfigAccess,
    modeled: F,
) -> serde_json::Value
where
    F: Fn(&FireIdentity) -> FireIdentity,
{
    let response = access
        .execute(&format!(
            "{{ TriggerFire {{ {} }} AgentRequest {{ _docID request_id lifecycle_state }} FireOutcome {{ handoff_id }} Goal {{ node_did session_id assignment_root_request_doc_id status }} }}",
            durable::FIRE_FIELDS
        ))
        .await
        .unwrap();
    let data = &response["data"];
    let mut fires: Vec<TriggerFire> = decode(&data["TriggerFire"]);
    fires.sort_by(|left, right| left.created_at.cmp(&right.created_at));
    let requests: Vec<gents_protocol::row::AgentRequestRow> = decode(&data["AgentRequest"]);
    let published: Vec<String> = data["FireOutcome"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["handoff_id"].as_str().unwrap().to_owned())
        .collect();
    let bindings: Vec<durable::GoalOutcomeBinding> = data["Goal"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|goal| {
            let root = goal["assignment_root_request_doc_id"].as_str()?;
            let assignment = requests
                .iter()
                .find(|row| row.doc_id.as_deref() == Some(root))?;
            Some(durable::GoalOutcomeBinding {
                owner_did: goal["node_did"].as_str().unwrap().to_owned(),
                session_id: goal["session_id"].as_str().unwrap().to_owned(),
                assignment_request_id: assignment.request_id.clone(),
                status: goal["status"].as_str().unwrap().to_owned(),
            })
        })
        .collect();
    let identity = |fire: &TriggerFire| serde_json::to_value(modeled(&fire.identity)).unwrap();
    let binding = |fire: &TriggerFire| {
        bindings
            .iter()
            .find(|goal| {
                goal.owner_did == fire.identity.owner_did && goal.session_id == fire.session_id
            })
            .cloned()
    };
    serde_json::json!({
        "receipts": fires.iter().map(identity).collect::<Vec<_>>(),
        "requests": fires.iter().filter_map(|fire| {
            let state = requests.iter().find(|row| row.request_id == fire.request_id)?.lifecycle_state;
            Some(serde_json::json!({
                "fire": {
                    "identity": identity(fire), "session": fire.session_id,
                    "serial": fire.queued_serial, "emit_outcome": fire.emit_outcome,
                    "goal_backed": fire.goal_id.is_some(),
                },
                "running": state.is_some_and(|state| matches!(
                    state,
                    gents_protocol::request_lifecycle::RequestLifecycleState::Claimed
                        | gents_protocol::request_lifecycle::RequestLifecycleState::Processing
                )),
                "terminal": state.is_some_and(|state| state.is_terminal()),
                "assignment_replaced": durable::assignment_replaced(fire, binding(fire).as_ref()),
                "goal_assignment_applied": fire.goal_assignment_applied,
            }))
        }).collect::<Vec<_>>(),
        "outcomes": fires.iter()
            .filter(|fire| published.contains(&fire.identity.outcome_id()))
            .map(identity)
            .collect::<Vec<_>>(),
        "goals": bindings.iter().filter_map(|goal| {
            let fire = fires.iter().find(|fire| fire.request_id == goal.assignment_request_id)?;
            let assignment = modeled(&fire.identity);
            Some(serde_json::json!({
                "owner": if goal.owner_did == fire.identity.owner_did {
                    assignment.owner_did.clone()
                } else {
                    goal.owner_did.clone()
                },
                "session": goal.session_id, "assignment": assignment, "status": goal.status,
            }))
        }).collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn restart_outcome_recovery_matches_modeled_crash_boundaries() {
    for case in gents_lean_vocab::families::trigger_delivery_outcome_recovery::cases() {
        let pre: State = gents_lean_vocab::family::recode(&case.input.pre);
        let request = &pre.requests[0];
        let identity = request.fire.identity.clone();
        let session = request.fire.session.clone();
        let fire = TriggerFire {
            fire_key: identity.fire_key(),
            identity: identity.clone(),
            task_id: "recovery-task".into(),
            request_id: identity.request_id(),
            session_id: session.clone(),
            goal_id: request.fire.goal_backed.then(|| "recovery-goal".into()),
            goal_objective: request
                .fire
                .goal_backed
                .then(|| "finish assignment".into()),
            goal_token_budget: None,
            goal_assignment_applied: request.goal_assignment_applied,
            emit_outcome: request.fire.emit_outcome,
            queued_serial: request.fire.serial,
            source_handoff_id: Some("source-assignment".into()),
            reply_session_id: None,
            shard_id: None,
            attempt: None,
            created_at: "2026-01-01T00:00:00Z".into(),
        };
        let directory = tempfile::tempdir().unwrap();
        let (root, key) = (
            gents_store::identity::NodeIdentity::did(
                &crate::triggers::callback::test_root_identity(),
            )
            .to_owned(),
            directory.path().join("node.key"),
        );
        tokio::task::spawn_blocking(move || {
            gents_store::test_support::write_held_identity_key(&root, &key)
        })
        .await
        .unwrap();
        let mut db = gents_inference::test_support::test_db_at(
            directory.path(),
            None,
            crate::lifecycle_hooks::composition(),
        )
        .await
        .unwrap();
        let node = db.node.clone();
        crate::triggers::callback::seed_test_agent(&node, db.node_identity.did()).await;
        let agent = crate::triggers::callback::test_agent_identity();
        let terminal = request.terminal;
        let state = if terminal { "completed" } else { "pending" };
        let agent_did = crate::triggers::callback::test_agent_did();
        let create_request = |request_id: &str, created_at: &str| {
            format!(
                r#"mutation {{create_AgentRequest(input: {{
                request_id: "{}", node_did: "{}", session_id: "{}", agent_did: "{}",
                purpose: "normal", lifecycle_state: "{state}", created_at: "{}"
            }}) {{_docID}} }}"#,
                escape_graphql_string(request_id),
                escape_graphql_string(&fire.identity.owner_did),
                escape_graphql_string(&session),
                escape_graphql_string(&agent_did),
                escape_graphql_string(created_at),
            )
        };
        let request_mutation = create_request(&fire.request_id, "2026-01-01T00:00:00Z");
        let replacement_mutation = create_request("recovery-replacement", "2026-01-01T00:00:01Z");
        let goal_status = pre.goals.first().map(|goal| goal.status.clone());
        let replaced = request.assignment_replaced;
        let published = !pre.outcomes.is_empty();
        let access = crate::triggers::callback::test_agent_access(&node);
        access
            .transact_write("test.seed_terminal_outcome_boundary", |txn| {
                let (fire, request_mutation, agent_did, goal_status) =
                    (&fire, &request_mutation, &agent_did, &goal_status);
                Box::pin(async move {
                    gents_session::session::ensure_session_in_txn(
                        txn,
                        &fire.session_id,
                        &fire.identity.owner_did,
                        agent_did,
                        None,
                        None,
                        None,
                        "2026-01-01T00:00:00Z",
                    )
                    .await?;
                    durable::stage_fire_request(txn, fire, request_mutation).await?;
                    if goal_status.is_some() || replaced {
                        let response = txn
                            .execute(&format!(
                                "{{AgentRequest(filter: {{request_id: {{_eq: \"{}\"}}}}) {{_docID}}}}",
                                escape_graphql_string(&fire.request_id)
                            ))
                            .await?;
                        let root = response["data"]["AgentRequest"][0]["_docID"]
                            .as_str()
                            .unwrap();
                        let session = txn
                            .execute(&format!(
                                "{{AgentSession(filter: {{session_id: {{_eq: \"{}\"}}}}) {{_docID}}}}",
                                escape_graphql_string(&fire.session_id)
                            ))
                            .await?;
                        txn.execute_with_variables(
                            "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",
                            &serde_json::json!({"input": {
                                "goal_id": fire.goal_id, "node_did": fire.identity.owner_did,
                                "session_id": fire.session_id, "objective": "finish assignment",
                                "session_doc_id": session["data"]["AgentSession"][0]["_docID"],
                                "status": goal_status.as_deref().unwrap_or("active"),
                                "assignment_root_request_doc_id": root,
                            }}),
                        )
                        .await?;
                    }
                    if published {
                        durable::stage_outcome(
                            txn,
                            fire,
                            durable::OutcomeStage {
                                goal: None,
                                assignment_replaced: false,
                                request_terminal: terminal,
                                terminal_state: state,
                                reason: state,
                            },
                            "2026-01-01T00:00:01Z",
                        )
                        .await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        if replaced {
            access
                .transact_write("test.seed_replaced_assignment", |txn| {
                    let (fire, replacement_mutation) = (&fire, &replacement_mutation);
                    Box::pin(async move {
                        let created = txn.execute(replacement_mutation).await?;
                        let replacement =
                            gents_store::graphql::created_doc_id(&created, "AgentRequest")?;
                        txn.execute(&format!(
                            "mutation {{update_Goal(filter: {{session_id: {{_eq: \"{}\"}}}}, input: {{assignment_root_request_doc_id: \"{}\"}}) {{_docID}}}}",
                            escape_graphql_string(&fire.session_id),
                            escape_graphql_string(&replacement),
                        ))
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
        }
        drop(access);
        drop(node);
        db.simulate_process_crash().await.unwrap();
        let node = db.node.clone();
        let recovered = durable::recover_outcomes(&node, &fire.identity.owner_did, agent.as_ref())
            .await
            .unwrap();
        let recovered_again =
            durable::recover_outcomes(&node, &fire.identity.owner_did, agent.as_ref())
                .await
                .unwrap();
        let post = observed_delivery_state(
            &crate::triggers::callback::test_agent_access(&node),
            FireIdentity::clone,
        )
        .await;
        case.check(
            gents_lean_vocab::families::trigger_delivery_outcome_recovery::OutcomeRecoveryOutput {
            post: gents_lean_vocab::family::recode(&post),
            recovered: recovered as u64,
            recovered_again: recovered_again as u64,
            },
        );
        node.shutdown().await.expect("node shutdown");
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
    access: &gents_store::config_client::ConfigAccess,
) -> Vec<gents_protocol::row::AgentRequestRow> {
    let response = access
        .execute(&format!(
            "{{AgentRequest {{{} lifecycle_state}}}}",
            gents_protocol::row::AGENT_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap()
}

#[tokio::test]
async fn generated_terminal_outcome_action_traces_use_native_owners() {
    for trace in contract()["trigger_delivery_outcome_trace_cases"]
        .as_array()
        .unwrap()
    {
        // The Task's Agent admits, claims, terminalizes and recovers its fires.
        let (db, _user, _access, binding) =
            crate::test_support::signed_test_node(trace["name"].as_str().unwrap()).await;
        let node = db.node.clone();
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            ::identity::Did::new(agent.did()).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let mut published = std::collections::BTreeMap::new();
        let mut terminal_states = std::collections::BTreeMap::new();
        for (index, step) in trace["steps"].as_array().unwrap().iter().enumerate() {
            let action: OutcomeAction = decode(&step["action"]);
            let expected: State = decode(&step["post"]);
            match action {
                OutcomeAction::Admit { fire } => {
                    let receipt = receipt(&fire, index);
                    let mutation = request_mutation_as(&receipt, agent.did());
                    let result = access
                        .transact_write("test.outcome_trace_admit", |txn| {
                            let receipt = &receipt;
                            let mutation = &mutation;
                            let agent = agent.did();
                            Box::pin(async move {
                                gents_session::session::ensure_session_in_txn(
                                    txn,
                                    &receipt.session_id,
                                    &receipt.identity.owner_did,
                                    agent,
                                    None,
                                    None,
                                    None,
                                    &receipt.created_at,
                                )
                                .await?;
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
                    let request = gents_protocol::row::AgentRequest::try_from(row.clone()).unwrap();
                    let mut lifecycle = gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                        node.clone(),
                        request,
                        60,
                        gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                        "test-backend",
                    );
                    lifecycle.bind_managed_agent(agent.as_ref()).unwrap();
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
                    let result = access.transact_write("test.inject_terminal_publication_gap", |txn| {
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
                        durable::recover_outcomes(&node, "owner-a", agent.as_ref())
                            .await
                            .unwrap();
                    } else {
                        let result: anyhow::Result<()> = access
                            .transact_write("test.outcome_publication_rollback", |txn| {
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
    access: &gents_store::config_client::ConfigAccess,
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
    gents_store::graphql::created_doc_id(&response, "Work").unwrap()
}

fn handoff_consumer() -> gents_protocol::event_delivery::EventConsumer {
    gents_protocol::event_delivery::EventConsumer::Trigger {
        trigger_id: "handoff".into(),
    }
}

async fn saved_arrival_cursor(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
) -> String {
    access
        .transact_write("test.read_arrival_cursor", |txn| {
            Box::pin(async move {
                Ok::<_, anyhow::Error>(
                    gents_documents::event_consumer::load_or_seed(txn, owner, &handoff_consumer())
                        .await?
                        .cursor
                        .after,
                )
            })
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn generated_arrival_checkpoints_preserve_committed_delivery_across_crashes() {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents_store::config_client::{ConfigAccess, NodePermission};
    use gents_store::identity::NodeIdentity as _;
    for case in contract()["trigger_delivery_cursor_cases"]
        .as_array()
        .unwrap()
    {
        let db = gents_inference::test_support::test_db(
            "arrival-checkpoints",
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let root = gents_store::test_support::held_runtime_identity(db.node_identity.did());
        let owner = root.did();
        let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
        gents_inference::test_support::install_test_agent_binding(
            &node, owner, "trigger", &binding,
        )
        .await;
        let selected = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let access = ConfigAccess::local_as(node.clone(), ::identity::Did::new(owner).unwrap());
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            access
                .grant_node_permission(
                    &::identity::Did::new(&binding.agent_did).unwrap(),
                    permission,
                )
                .await
                .unwrap();
        }
        let agent = ConfigAccess::local_managed_agent(
            node.clone(),
            ::identity::Did::new(&binding.agent_did).unwrap(),
            selected.as_ref(),
        )
        .unwrap();
        access
            .add_collection("type Work { label: String eligible: Boolean }")
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
        let plan = DesiredStateApplyPlan::new([
            (gents_store::collection::Collection::Task, serde_json::json!({"node_did":owner,"task_id":"contract-task",
                "agent_did":binding.agent_did,"prompt_template":"contract task"})),
            (gents_store::collection::Collection::EventSource, serde_json::json!({"node_did":owner,"event_source_id":"source",
                "source_collection":"Work","event_kind":"created","filter":"{eligible: {_eq: true}}"})),
            (gents_store::collection::Collection::Trigger, serde_json::json!({"node_did":owner,"trigger_id":"handoff",
                "task_id":"contract-task","source":{"kind":"event","event_source_id":"source"},
                "enabled":case["enabled"],"concurrency":case["mode"]})),
        ].into_iter().map(|(collection, value)| DesiredStateApplyDocument {collection, add: value.clone(), update: value}).collect()).unwrap();
        access
            .transact_write("test.arrival_config", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                    .map(|_| ())
                })
            })
            .await
            .unwrap();
        assert_eq!(
            saved_arrival_cursor(&agent, owner).await,
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
        agent
            .transact_write("test.prior_checkpoint", |txn| {
                Box::pin(async move {
                    gents_documents::event_consumer::advance(
                        txn,
                        owner,
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
                saved_arrival_cursor(&agent, owner).await,
                pre_after,
                "{}",
                case["name"]
            );
        }
        let adapt_fire = |mut fire: Fire| {
            fire.identity.owner_did = owner.to_owned();
            fire.identity.source_doc_id = documents[&fire.identity.source_doc_id].clone();
            fire
        };
        let pre: State = decode(&case["pre"]);
        for (index, request) in pre.requests.iter().enumerate() {
            let receipt = receipt(&adapt_fire(request.fire.clone()), index);
            let mutation = request_mutation_as(&receipt, &binding.agent_did);
            agent
                .transact_write("test.prior_arrival_admission", |txn| {
                    let receipt = &receipt;
                    let mutation = &mutation;
                    Box::pin(async move {
                        durable::stage_fire_request(txn, receipt, mutation).await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
        }
        if let Some(commit) = case["admission_commit"].as_bool() {
            let receipt = receipt(&adapt_fire(decode(&case["fire"])), pre.requests.len());
            let mutation = request_mutation_as(&receipt, &binding.agent_did);
            let admitted: anyhow::Result<()> = agent
                .transact_write("test.arrival_admission_crash", |txn| {
                    let receipt = &receipt;
                    let mutation = &mutation;
                    Box::pin(async move {
                        durable::stage_fire_request(txn, receipt, mutation).await?;
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
            let checkpointed: anyhow::Result<bool> = agent
                .transact_write("test.arrival_checkpoint_crash", |txn| {
                    let entry = &entry;
                    Box::pin(async move {
                        let accepted = gents_documents::event_consumer::checkpoint_prefix(
                            txn,
                            owner,
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
        let after = saved_arrival_cursor(&agent, owner).await;
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
        let rows = agent
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
        node.shutdown().await.expect("node shutdown");
    }
}

fn handoff_binding() -> gents_protocol::event_delivery::EventConsumer {
    gents_protocol::event_delivery::EventConsumer::CallbackBinding {
        binding_id: "handoff".into(),
    }
}

async fn admit_callback_arrival(
    node: &std::sync::Arc<defra_node::Node>,
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    agent_did: &str,
    doc_id: &str,
    version: &str,
) {
    let mut invocation = crate::triggers::callback::create_pending_invocation(
        node,
        access,
        &crate::triggers::callback::CallbackInvocationDoc {
            input: serde_json::json!({}),
            invocation_id: uuid::Uuid::new_v4().to_string(),
            agent_did: agent_did.to_owned(),
            owner_node_did: owner.into(),
            callback_id: "callback".into(),
            configuration_generation: None,
            claimed_callback: None,
            origin: gents_documents::CallbackInvocationOrigin::Event {
                binding_id: "handoff".into(),
                source_collection: "Work".into(),
                source_doc_id: doc_id.into(),
                source_version: Some(version.into()),
            },
            idempotency_key: gents_protocol::trigger_delivery::idempotency_key(
                "handoff", "Work", doc_id,
            ),
            caused_by_correlation: None,
            lifecycle_state: crate::triggers::callback::LIFECYCLE_PENDING.into(),
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
    if invocation.claimed_at.is_none() {
        invocation.lifecycle_state = crate::triggers::callback::LIFECYCLE_CLAIMED.into();
        invocation.claimed_at = Some("2030-01-01T00:00:00Z".into());
        invocation.claimed_callback = Some(
            serde_json::json!({
                "node_did": owner, "callback_id": "callback", "enabled": true,
                "handler": {"kind": "built_in", "emitter": "create_workspace"}
            })
            .to_string(),
        );
        assert!(crate::triggers::callback::update_invocation(
            access,
            &invocation,
            Some(crate::triggers::callback::LIFECYCLE_PENDING),
        )
        .await
        .unwrap());
    }
}

#[tokio::test]
async fn generated_callback_arrival_checkpoints_use_invocation_receipts() {
    use gents_documents::apply_desired_state_plan;
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents_store::config_client::{ConfigAccess, NodePermission};
    for case in contract()["trigger_delivery_callback_cursor_cases"]
        .as_array()
        .unwrap()
    {
        let name = &case["name"];
        use gents_store::identity::NodeIdentity as _;
        let db = gents_inference::test_support::test_db(
            "callback-checkpoints",
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let root = gents_store::test_support::held_runtime_identity(db.node_identity.did());
        let binding = gents_store::identity::provision_agent_identity(&root).unwrap();
        let owner = root.did();
        gents_inference::test_support::install_test_agent_binding(
            &node, owner, "callback", &binding,
        )
        .await;
        let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            ::identity::Did::new(&binding.agent_did).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        let access = ConfigAccess::local_as(node.clone(), ::identity::Did::new(owner).unwrap());
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            access
                .grant_node_permission(
                    &::identity::Did::new(&binding.agent_did).unwrap(),
                    permission,
                )
                .await
                .unwrap();
        }
        access
            .add_collection("type Work { label: String eligible: Boolean }")
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
        let plan = DesiredStateApplyPlan::new([
            (gents_store::collection::Collection::Callback, serde_json::json!({"node_did":owner,"callback_id":"callback",
                "enabled":callback_enabled,"handler":{"kind":"built_in","emitter":"create_workspace"}})),
            (gents_store::collection::Collection::EventSource, serde_json::json!({"node_did":owner,"event_source_id":"source",
                "source_collection":"Work","event_kind":"created","filter":"{eligible: {_eq: true}}"})),
            (gents_store::collection::Collection::CallbackBinding, serde_json::json!({"node_did":owner,"binding_id":"handoff",
                "agent_did":binding.agent_did,"event_source_id":"source","callback_id":"callback","enabled":binding_enabled})),
        ].into_iter().map(|(collection, value)| DesiredStateApplyDocument {collection, add: value.clone(), update: value}).collect()).unwrap();
        access
            .transact_write("test.callback_arrival_config", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await?;
                    gents_documents::event_consumer::load_or_seed(txn, owner, &handoff_binding())
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let saved = || {
            let access = agent_access.clone();
            async move {
                access
                    .transact_write("test.read_callback_cursor", |txn| {
                        Box::pin(async move {
                            Ok::<_, anyhow::Error>(
                                gents_documents::event_consumer::load_or_seed(
                                    txn,
                                    owner,
                                    &handoff_binding(),
                                )
                                .await?
                                .cursor
                                .after,
                            )
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
        agent_access
            .transact_write("test.prior_callback_checkpoint", |txn| {
                Box::pin(async move {
                    gents_documents::event_consumer::advance(
                        txn,
                        owner,
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
            admit_callback_arrival(
                &node,
                &agent_access,
                owner,
                &binding.agent_did,
                &documents[&receipt.source_doc_id],
                "v1",
            )
            .await;
        }
        let entry: Arrival = decode(&case["entry"]);
        let doc_id = documents[&entry.identity.source_doc_id].clone();
        if case["admission_commit"].as_bool() == Some(true) {
            admit_callback_arrival(
                &node,
                &agent_access,
                owner,
                &binding.agent_did,
                &doc_id,
                "v1",
            )
            .await;
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
                admit_callback_arrival(
                    &node,
                    &agent_access,
                    owner,
                    &binding.agent_did,
                    &doc_id,
                    "v2",
                )
                .await;
            }
        }
        if let Some(commit) = case["checkpoint_commit"].as_bool() {
            let checkpointed: anyhow::Result<bool> = agent_access
                .transact_write("test.callback_checkpoint_crash", |txn| {
                    let entry = &entry;
                    Box::pin(async move {
                        let accepted = gents_documents::event_consumer::checkpoint_prefix(
                            txn,
                            owner,
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
        let invocations = agent_access
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
        node.shutdown().await.expect("node shutdown");
    }
}

#[test]
fn observed_claim_cohorts_match_lean() {
    let contract = gents_lean_contract::load_contract_snapshot::<serde_json::Value>().unwrap();
    for case in contract["trigger_delivery_observed_claim_cases"]
        .as_array()
        .unwrap()
    {
        let candidate: gents_session::request::lifecycle::arrival_order::ClaimObservation =
            serde_json::from_value(case["candidate"].clone()).unwrap();
        let rows: Vec<gents_session::request::lifecycle::arrival_order::ClaimObservation> =
            serde_json::from_value(case["rows"].clone()).unwrap();
        assert_eq!(
            gents_session::request::lifecycle::arrival_order::observed_claim_allowed(
                &candidate, &rows
            ),
            case["allowed"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
    }
}

fn trigger_with_options(
    template: Option<&str>,
    create_session: Option<bool>,
    budget: Option<u32>,
    receipted: bool,
) -> gents_documents::Trigger {
    let source = if receipted {
        serde_json::json!({"kind": "event", "event_source_id": "s"})
    } else {
        serde_json::json!({"kind": "schedule", "schedule_id": "s"})
    };
    serde_json::from_value(serde_json::json!({
        "node_did": "did:key:zOwner", "trigger_id": "t", "task_id": "task",
        "source": source,
        "session_id_template": template,
        "create_session": create_session,
        "max_fires_per_hour": budget,
    }))
    .unwrap()
}

#[test]
fn generated_session_key_cases_match_admission_and_config() {
    let cases = contract();
    for case in cases["trigger_delivery_session_config_cases"]
        .as_array()
        .unwrap()
    {
        let trigger = trigger_with_options(
            case["template"].as_str(),
            case["create_session"].as_bool(),
            None,
            true,
        );
        assert_eq!(
            trigger.validate_options().is_ok(),
            case["allowed"].as_bool().unwrap(),
            "{case}"
        );
    }
}

/// A signed Node whose Agent admits fires of the User-owned Trigger `handoff`.
async fn budget_fixture(
    name: &str,
) -> (
    gents_inference::test_support::TestDb,
    gents_store::config_client::ConfigAccess,
    gents_store::config_client::ConfigAccess,
    ProductionMaterializer,
    ResolvedTask,
    String,
    String,
) {
    let (db, _user, client, behavior) = signed_materializer_agent(name).await;
    let owner = behavior.node_did().to_owned();
    let agent_did = behavior.agent_did.clone();
    let config = serde_json::from_value(serde_json::json!({
        "node":{"node_did":owner},
        "default_agent_selection":{"node_did":owner,"default_agent_did":agent_did},
        "tasks":[{"node_did":owner,"task_id":"budget-task","agent_did":agent_did,"prompt_template":"work"}],
        "schedules":[{"node_did":owner,"schedule_id":"s","cadence":{"kind":"interval","interval_secs":60}}],
        "triggers":[{"node_did":owner,"trigger_id":"handoff","task_id":"budget-task","source":{"kind":"schedule","schedule_id":"s"}}]
    }))
    .unwrap();
    let trigger_doc = apply_user_config(&client, &config, "handoff").await;
    let agent = agent_access(&db.node, &behavior);
    let snapshot = snapshot_with_agent_and_schedules(behavior, RapidHashMap::new());
    let (_tx, rx) = watch::channel(snapshot);
    let materializer = ProductionMaterializer::new(db.node.clone(), rx);
    let task = ResolvedTask {
        task_id: "budget-task".into(),
        agent_did,
        ..resolved_task("work")
    };
    (db, client, agent, materializer, task, owner, trigger_doc)
}

fn budget_fire(owner: &str, document: &str, created_at: String) -> TriggerFire {
    let identity = FireIdentity {
        owner_did: owner.to_owned(),
        trigger_id: "handoff".into(),
        source_collection: "Schedule".into(),
        source_doc_id: document.to_owned(),
    };
    TriggerFire {
        fire_key: identity.fire_key(),
        request_id: identity.request_id(),
        session_id: identity.session_id(),
        identity,
        task_id: "budget-task".into(),
        goal_id: None,
        goal_objective: None,
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: false,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at,
    }
}

const BUDGET_NOW: &str = "2030-01-01T12:00:00Z";

async fn seed_receipt(agent: &gents_store::config_client::ConfigAccess, fire: &TriggerFire) {
    agent
        .transact_write("test.budget_receipt", |txn| {
            Box::pin(async { durable::stage_fire_receipt(txn, fire).await })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn generated_fire_budget_cases_stop_at_the_modeled_receipt_count() {
    let cases = contract();
    assert_eq!(
        cases["trigger_fire_budget_default"].as_u64(),
        Some(gents_documents::DEFAULT_MAX_FIRES_PER_HOUR as u64)
    );
    assert_eq!(
        cases["trigger_fire_budget_status"].as_str(),
        Some(gents_documents::BUDGET_EXHAUSTED_STATUS)
    );
    for case in cases["trigger_fire_budget_config_cases"]
        .as_array()
        .unwrap()
    {
        let trigger = trigger_with_options(
            None,
            None,
            case["configured"].as_u64().map(|value| value as u32),
            case["receipted"].as_bool().unwrap(),
        );
        assert_eq!(
            trigger.validate_options().is_ok(),
            case["allowed"].as_bool().unwrap(),
            "{case}"
        );
    }
    let now = chrono::DateTime::parse_from_rfc3339(BUDGET_NOW).unwrap();
    for case in cases["trigger_fire_budget_cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let (_db, client, agent, materializer, task, owner, trigger_doc) =
            budget_fixture(&format!("fire-budget-{name}")).await;
        let budget = trigger_with_options(
            None,
            None,
            case["configured"].as_u64().map(|v| v as u32),
            true,
        )
        .effective_max_fires_per_hour();
        assert_eq!(budget as u64, case["budget"].as_u64().unwrap(), "{case}");
        let current = budget_fire(&owner, "budget-fire", BUDGET_NOW.to_owned());
        let prepared = durable::PreparedFire {
            receipt: current.clone(),
            target_existing: false,
            create_session: false,
            replace_prior: false,
            event_source_generation: None,
            max_fires_per_hour: budget,
        };
        let materialize = || {
            materializer.materialize(
                &task,
                Some("handoff"),
                TriggerKind::Schedule,
                Some(&trigger_doc),
                None,
                None,
                None,
                "work",
                None,
                &prepared.receipt.fire_key,
                Some(&prepared),
                None,
            )
        };
        if case["replay"].as_bool().unwrap() {
            materialize().await.unwrap();
        }
        for (index, age) in case["prior_ages"].as_array().unwrap().iter().enumerate() {
            let at = now - chrono::Duration::seconds(age.as_i64().unwrap());
            let prior = budget_fire(
                &owner,
                &format!("prior-{index}"),
                at.to_rfc3339_opts(SecondsFormat::Secs, true),
            );
            seed_receipt(&agent, &prior).await;
        }
        let result = materialize().await;
        let admitted = result.is_ok();
        let exhausted = result
            .as_ref()
            .err()
            .is_some_and(|error| error.downcast_ref::<MaterializeBudgetExhausted>().is_some());
        let row = client
            .execute(r#"{ Trigger(filter: {trigger_id: {_eq: "handoff"}}) { enabled last_status last_error } }"#)
            .await
            .unwrap();
        let row = &row["data"]["Trigger"][0];
        assert_eq!(admitted, case["admitted"].as_bool().unwrap(), "{name}");
        assert_eq!(exhausted, case["stopped"].as_bool().unwrap(), "{name}");
        assert_eq!(
            row["enabled"].as_bool() == Some(false),
            case["stopped"].as_bool().unwrap(),
            "{name}: {row}"
        );
        assert_eq!(
            row["last_status"].as_str(),
            case["status"].as_str(),
            "{name}"
        );
        if exhausted {
            assert!(
                row["last_error"]
                    .as_str()
                    .is_some_and(|error| error.contains("max_fires_per_hour")),
                "{name}: {row}"
            );
        }
    }
}
