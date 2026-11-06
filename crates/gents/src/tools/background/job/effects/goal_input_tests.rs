use super::*;
use crate::goal::operator_resume_test_support::Fixture;
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
use serde_json::{json, Value};

fn reference(value: &Value, name: &str, roots: &[String; 2]) -> Option<GoalAssignmentInput> {
    value.is_object().then(|| GoalAssignmentInput {
        goal_id: format!("{name}-goal-{}", value["goal"]),
        root_request_doc_id: value["root"]
            .as_u64()
            .map(|root| roots[(root / 100 - 1) as usize].clone()),
    })
}

async fn observe_goal(f: &Fixture, session: &str, assignment: Option<&GoalAssignmentInput>) {
    let Some(assignment) = assignment else { return };
    let access = f.agent_access();
    let goal = crate::goal::set_goal_from_access(
        &access,
        f.identity.did(),
        session,
        Some("Destination work"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    let root = assignment
        .root_request_doc_id
        .as_deref()
        .map(|root| format!("\"{}\"", escape_graphql_string(root)))
        .unwrap_or_else(|| "null".into());
    access.write("test.delivery_goal_input", &format!(r#"mutation {{update_Goal(docID: "{}", input: {{goal_id: "{}", assignment_root_request_doc_id: {root}}}) {{_docID}}}}"#,
        escape_graphql_string(&goal.doc_id), escape_graphql_string(&assignment.goal_id))).await.unwrap();
}

async fn stored(access: &ConfigAccess, request_id: &str) -> AgentRequestRow {
    let response = access
        .execute(&format!(
            r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{{}}}}}"#,
            escape_graphql_string(request_id),
            crate::request::admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row: AgentRequestRow = serde_json::from_value(rows[0].clone()).unwrap();
    crate::request::admission::verify_request_receipt_signature(&row).unwrap();
    row
}

async fn deliver(
    f: &Fixture,
    cause: &SessionMessageCause,
    case: &Value,
    session: &str,
    captured: Option<GoalAssignmentInput>,
) -> Result<(String, String)> {
    let name = case["name"].as_str().unwrap();
    let access = f.agent_access();
    if case["plugin"] == true {
        let retry_key = if case["existing"] == true {
            job_message_key(name, "worker", 1, 1)
        } else {
            job_spawn_key(name, "worker", 1)
        };
        let create = access
            .transact_write("test.plugin_delivery", |txn| {
                let retry_key = retry_key.clone();
                Box::pin(async move {
                    stage_request(
                        txn,
                        cause,
                        f.identity.did(),
                        f.agent.did(),
                        session.into(),
                        "Delivery work",
                        None,
                        retry_key,
                        1,
                        None,
                        case["existing"] == true,
                    )
                    .await
                })
            })
            .await?;
        let row = access
            .execute(&format!(
                r#"{{AgentRequest(filter: {{retry_key: {{_eq: "{}"}}}}) {{_docID request_id}}}}"#,
                escape_graphql_string(&retry_key)
            ))
            .await?;
        let row = &row["data"]["AgentRequest"][0];
        assert_eq!(
            create.input.goal_assignment,
            stored(&access, row["request_id"].as_str().unwrap())
                .await
                .input
                .and_then(|input| input.goal_assignment)
        );
        return Ok((
            row["_docID"].as_str().unwrap().into(),
            row["request_id"].as_str().unwrap().into(),
        ));
    }
    let identity = FireIdentity {
        owner_did: f.identity.did().into(),
        trigger_id: "manual:delivery".into(),
        source_collection: "Task".into(),
        source_doc_id: name.into(),
    };
    let fire = TriggerFire {
        fire_key: identity.fire_key(),
        request_id: identity.request_id(),
        identity,
        task_id: "delivery".into(),
        session_id: session.into(),
        goal_id: captured.as_ref().map(|r| r.goal_id.clone()),
        goal_objective: captured.as_ref().map(|_| "Prepared Task work".into()),
        goal_token_budget: None,
        goal_assignment_applied: false,
        emit_outcome: false,
        queued_serial: false,
        source_handoff_id: None,
        reply_session_id: None,
        shard_id: None,
        attempt: None,
        created_at: "2026-10-21T00:00:00Z".into(),
    };
    let mut spec = crate::request::lifecycle::materialize::pending_agent_request_spec(
        f.identity.did(),
        f.agent.did(),
        "Delivery work",
        crate::request::lifecycle::ExecutionOrigin::Interactive,
        crate::request::lifecycle::TriggerLineage {
            trigger_kind: Some("manual".into()),
            ..Default::default()
        },
        None,
        None,
        &fire.request_id,
        session,
        Some(&fire.fire_key),
        Some(f.agent.did()),
        None,
    )?;
    spec.input.goal_assignment = captured;
    let create = crate::request::lifecycle::build_signed_request(
        spec,
        crate::request::lifecycle::RequestSigner::Identity(f.agent.as_ref()),
    )
    .await?;
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: fire,
        target_existing: case["existing"] == true,
        replace_prior: false,
        event_source_generation: None,
    };
    let admitted = crate::request::lifecycle::materialize::write_trigger_delivery_with_identity(
        &f.node,
        identity::Did::new(f.agent.did()).unwrap(),
        &prepared,
        &create,
        Some(f.agent.as_ref()),
    )
    .await?;
    Ok((admitted.request.doc_id, admitted.request.request_id))
}

async fn foreign_source(root: &KeyIdentity) -> (Arc<EmbeddedNode>, Arc<dyn NodeIdentity>, String) {
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(root.did())
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let access = ConfigAccess::local(node.clone());
    let node_did = escape_graphql_string(root.did());
    access
        .write(
            "test.remote_job_source.node",
            &format!(r#"mutation {{ create_Node(input: {{node_did: "{node_did}"}}) {{_docID}} }}"#),
        )
        .await
        .unwrap();
    let binding = crate::store::identity::provision_agent_identity(root).unwrap();
    crate::test_support::install_test_agent_binding(
        &node,
        root.did(),
        "foreign job caller",
        &binding,
    )
    .await;
    let agent = crate::store::identity::resolve_agent_identity(root, &binding).unwrap();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "delivery-caller",
        root.did(),
        agent.did(),
        agent.did(),
        "caller-session",
        "Send destination work",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(agent.did()),
    );
    create.input.goal_assignment = Some(GoalAssignmentInput {
        goal_id: "caller-goal".into(),
        root_request_doc_id: Some("caller-root".into()),
    });
    crate::sign_agent_request_create(agent.as_ref(), &mut create)
        .await
        .unwrap();
    let created = access
        .write(
            "test.remote_job_source.request",
            &create.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc_id = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    (node, agent, doc_id)
}

async fn deliver_remote(
    access: &ConfigAccess,
    cause: &SessionMessageCause,
    target_node: &str,
    target_agent: &str,
    case: &Value,
    session: &str,
) -> Result<(String, String)> {
    let name = case["name"].as_str().unwrap();
    assert_eq!(case["plugin"], true, "remote delivery is a plugin message");
    let retry_key = job_message_key(name, "worker", 1, 1);
    let create = access
        .transact_write("test.remote_plugin_delivery", |txn| {
            let retry_key = retry_key.clone();
            Box::pin(async move {
                stage_request(
                    txn,
                    cause,
                    target_node,
                    target_agent,
                    session.into(),
                    "Delivery work",
                    None,
                    retry_key,
                    1,
                    None,
                    true,
                )
                .await
            })
        })
        .await?;
    let response = access
        .execute(&format!(
            r#"{{AgentRequest(filter: {{retry_key: {{_eq: "{}"}}}}) {{_docID request_id}}}}"#,
            escape_graphql_string(&retry_key)
        ))
        .await?;
    let row = &response["data"]["AgentRequest"][0];
    assert_eq!(
        create.input.goal_assignment,
        stored(access, row["request_id"].as_str().unwrap())
            .await
            .input
            .and_then(|input| input.goal_assignment)
    );
    Ok((
        row["_docID"].as_str().unwrap().into(),
        row["request_id"].as_str().unwrap().into(),
    ))
}

#[tokio::test]
async fn generated_deliveries_capture_destination_goal_in_the_admission_transaction() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["goal_delivery_input_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 20);
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let f = Fixture::new(&before).await;
        let temp = tempfile::tempdir().unwrap();
        let caller = KeyIdentity::load_or_create(temp.path().join("caller"), None).unwrap();
        let remote = case["remote"] == true;
        let identity = FireIdentity {
            owner_did: f.identity.did().into(),
            trigger_id: "manual:delivery".into(),
            source_collection: "Task".into(),
            source_doc_id: name.into(),
        };
        let session = if case["plugin"] == true {
            job_session_id(name, "worker", 1)
        } else if case["existing"] == true {
            format!("existing-{name}")
        } else {
            identity.session_id()
        };
        if case["expected"].is_null() {
            assert!(crate::triggers::engine::durable::resolve_session_id(
                &identity,
                Some(&session),
                false
            )
            .is_none());
            f.node.shutdown().await.unwrap();
            continue;
        }
        let foreign_source = if remote {
            Some(foreign_source(&caller).await)
        } else {
            None
        };
        let requester = if remote {
            caller.did()
        } else if case["plugin"] == true {
            f.identity.did()
        } else {
            f.agent.did()
        };
        let access = f.agent_access();
        if case["existing"] == true {
            access
                .transact_write("test.delivery.session", |txn| {
                    Box::pin(async {
                        crate::session::ensure_session_in_txn(
                            txn,
                            &session,
                            f.identity.did(),
                            f.agent.did(),
                            Some(requester),
                            None,
                            None,
                            "2026-01-01T00:00:00Z",
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
            let session_doc = access
                .transact_readonly("test.delivery.session_doc", |txn| {
                    Box::pin(async {
                        Ok::<_, anyhow::Error>(
                            crate::session::load_agent_session_owner_in_txn(
                                txn,
                                f.identity.did(),
                                &session,
                            )
                            .await?
                            .unwrap()
                            .doc_id,
                        )
                    })
                })
                .await
                .unwrap();
            access
                .add_resource_relationship(
                    "AgentSession",
                    &session_doc,
                    "parent",
                    "agents",
                    &f.agent_doc_id,
                )
                .await
                .unwrap();
            if remote {
                access
                    .add_actor_relationship(
                        "AgentSession",
                        &session_doc,
                        "reader",
                        &identity::Did::new(caller.did()).unwrap(),
                    )
                    .await
                    .unwrap();
            }
        }
        let mut roots = [String::new(), String::new()];
        for (index, root) in roots.iter_mut().enumerate() {
            let mut opening = AgentRequestCreate::base(
                RequestPurpose::Normal,
                format!("{name}-root-{index}"),
                f.identity.did(),
                f.agent.did(),
                f.agent.did(),
                &session,
                "Destination opening",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(f.agent.did()),
            );
            crate::sign_agent_request_create(f.agent.as_ref(), &mut opening)
                .await
                .unwrap();
            let created = access
                .write(
                    "test.delivery.opening",
                    &opening.graphql_mutation().unwrap(),
                )
                .await
                .unwrap();
            *root = crate::store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
        }
        let captured = reference(&case["captured"], name, &roots);
        let observed = reference(&case["observed"], name, &roots);
        let prior = reference(&case["admitted_reference"], name, &roots);
        let first_assignment = if case["admitted"] == true {
            prior.as_ref()
        } else {
            observed.as_ref()
        };
        if case["existing"] == true {
            observe_goal(&f, &session, first_assignment).await;
        } else if let Some(orphan) = first_assignment {
            access.write("test.delivery.orphan", &format!(r#"mutation {{ create_Goal(input: {{goal_id: "{}", node_did: "{}", agent_did: "{}", session_id: "{}", objective: "Orphan", status: "active"}}) {{_docID}} }}"#,
                escape_graphql_string(&orphan.goal_id), escape_graphql_string(f.identity.did()),
                escape_graphql_string(f.agent.did()), escape_graphql_string(&session))).await.unwrap();
        }
        let cause = SessionMessageCause {
            caller_node_did: if remote {
                caller.did().into()
            } else {
                f.identity.did().into()
            },
            caller_agent_did: foreign_source.as_ref().map_or_else(
                || f.agent.did().to_owned(),
                |(_, agent, _)| agent.did().to_owned(),
            ),
            caller_session_id: if remote {
                "caller-session".into()
            } else {
                f.parent.session_id.clone()
            },
            caller_requester_did: foreign_source.as_ref().map_or_else(
                || f.parent.requester_did.clone(),
                |(_, agent, _)| Some(agent.did().into()),
            ),
            caller_request_id: if remote {
                "delivery-caller".into()
            } else {
                f.parent.request_id.clone()
            },
            caller_request_doc_id: foreign_source
                .as_ref()
                .map_or_else(|| f.parent.doc_id.clone(), |(_, _, doc)| doc.clone()),
            caller_hop: 0,
            tool_call_id: "delivery-call".into(),
            tool_call_doc_id: "delivery-call-doc".into(),
            correlation: None,
        };
        if remote {
            if case["observation_present"] == true {
                let runtime: Arc<dyn NodeIdentity> = f.identity.clone();
                crate::goal::project_session_goal(
                    &f.node,
                    &runtime,
                    &f.agent,
                    &session,
                    Some(requester),
                )
                .await
                .unwrap();
                let response = access.execute(&format!(r#"{{GoalObservation(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{_docID observation_json}}}}"#,
                    escape_graphql_string(f.identity.did()), escape_graphql_string(&session))).await.unwrap();
                let row = &response["data"]["GoalObservation"][0];
                let mut observation: gents_protocol::goal_control::SessionGoalObservation =
                    serde_json::from_str(row["observation_json"].as_str().unwrap()).unwrap();
                if let Some(goal) = &mut observation.goal {
                    if case["foreign_owner"] == true {
                        goal.snapshot.node_did = caller.did().into();
                    }
                    if case["foreign_session"] == true {
                        goal.snapshot.session_id = "foreign-session".into();
                    }
                }
                observation.signature = f
                    .agent
                    .sign(&observation.signing_payload().unwrap())
                    .await
                    .unwrap();
                if case["observation_verified"] == false {
                    observation.signature[0] ^= 1;
                }
                access.write("test.delivery.observation", &format!(r#"mutation {{update_GoalObservation(docID: "{}", input: {{observation_json: "{}"}}) {{_docID}}}}"#,
                    escape_graphql_string(row["_docID"].as_str().unwrap()), escape_graphql_string(&serde_json::to_string(&observation).unwrap()))).await.unwrap();
            }
            let caller_access =
                ConfigAccess::local_as(f.node.clone(), identity::Did::new(caller.did()).unwrap());
            let state = crate::goal::load_session_goal_observation_state(
                &caller_access,
                &caller,
                f.identity.did(),
                &session,
                Some(requester),
            )
            .await
            .unwrap();
            let result = crate::goal::bind_goal_input(Default::default(), state);
            if case["expected"]["kind"] == "pending" {
                assert!(result.is_err(), "{name}");
                let retry_key = job_message_key(name, "worker", 1, 1);
                let response = access
                    .execute(&format!(
                        r#"{{AgentRequest(filter: {{retry_key: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                        escape_graphql_string(&retry_key),
                    ))
                    .await
                    .unwrap();
                assert!(
                    response["data"]["AgentRequest"]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                    "{name}: refused input must not leak a request"
                );
            } else {
                assert_eq!(
                    result.unwrap().goal_assignment,
                    reference(&case["expected"]["reference"], name, &roots),
                    "{name}"
                );
            }
            let (source_node, source_agent, _) = foreign_source.as_ref().unwrap();
            let source_access = ConfigAccess::local_managed_agent(
                source_node.clone(),
                identity::Did::new(source_agent.did()).unwrap(),
                source_agent.as_ref(),
            )
            .unwrap();
            let first = deliver_remote(
                &source_access,
                &cause,
                f.identity.did(),
                f.agent.did(),
                case,
                &session,
            )
            .await;
            if case["expected"]["kind"] == "pending" {
                assert!(
                    first.is_err(),
                    "{name}: unverified destination input must not publish"
                );
                let retry_key = job_message_key(name, "worker", 1, 1);
                let rows = source_access
                    .execute(&format!(
                        r#"{{AgentRequest(filter: {{retry_key: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                        escape_graphql_string(&retry_key)
                    ))
                    .await
                    .unwrap();
                assert!(
                    rows["data"]["AgentRequest"].as_array().unwrap().is_empty(),
                    "{name}: refused input must not leak a request"
                );
            } else {
                let first = first.unwrap_or_else(|error| panic!("{name}: modeled publication requires authenticated cross-runtime delivery: {error:#}"));
                let initial_row = stored(&source_access, &first.1).await;
                if case["admitted"] == true {
                    observe_goal(&f, &session, observed.as_ref()).await;
                }
                let second = deliver_remote(
                    &source_access,
                    &cause,
                    f.identity.did(),
                    f.agent.did(),
                    case,
                    &session,
                )
                .await
                .unwrap();
                assert_eq!(
                    first, second,
                    "{name}: duplicate keeps exact physical request"
                );
                let row = stored(&source_access, &second.1).await;
                assert_eq!(
                    row.admission_signature, initial_row.admission_signature,
                    "{name}: duplicate keeps signed receipt"
                );
                assert_eq!(
                    row.input, initial_row.input,
                    "{name}: duplicate keeps signed input"
                );
                let actual = row
                    .input
                    .as_ref()
                    .and_then(|input| input.goal_assignment.clone());
                assert_eq!(
                    actual,
                    reference(&case["expected"]["reference"], name, &roots),
                    "{name}"
                );
                assert_ne!(
                    actual.as_ref().map(|goal| goal.goal_id.as_str()),
                    Some("caller-goal"),
                    "{name}"
                );
            }
            source_node.shutdown().await.unwrap();
            f.node.shutdown().await.unwrap();
            continue;
        }
        let first = deliver(&f, &cause, case, &session, captured.clone())
            .await
            .unwrap();
        let initial_row = stored(&access, &first.1).await;
        if case["admitted"] == true {
            observe_goal(&f, &session, observed.as_ref()).await;
        }
        let result = deliver(&f, &cause, case, &session, captured).await.unwrap();
        assert_eq!(
            first, result,
            "{name}: duplicate keeps exact physical request"
        );
        let row = stored(&access, &result.1).await;
        assert_eq!(
            row.admission_signature, initial_row.admission_signature,
            "{name}"
        );
        assert_eq!(row.input, initial_row.input, "{name}");
        assert_eq!(
            row.input
                .as_ref()
                .and_then(|input| input.goal_assignment.clone()),
            reference(&case["expected"]["reference"], name, &roots),
            "{name}"
        );
        assert_ne!(
            row.input
                .as_ref()
                .and_then(|input| input.goal_assignment.as_ref())
                .map(|goal| goal.goal_id.as_str()),
            Some("caller-goal"),
            "{name}: destination must not inherit caller goal",
        );
        f.node.shutdown().await.unwrap();
    }
}
