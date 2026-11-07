use super::*;
use crate::goal::operator_resume_test_support::Fixture;
use crate::store::config_client::ConfigAccess;
use crate::store::graphql::escape_graphql_string;
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

#[tokio::test]
async fn generated_deliveries_capture_destination_goal_in_the_admission_transaction() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["goal_delivery_input_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 14);
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let f = Fixture::new(&before).await;
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
        // A job child's session is requested by the job's own Agent, which
        // signs the delivered request, as a staged spawn records it.
        let requester = f.agent.did();
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
            // A delivered client request requires its existing session's
            // relationships to be complete, as session creation records them.
            access
                .transact_write("test.delivery.session_relationships", |txn| {
                    let session_doc = &session_doc;
                    let agent_doc = &f.agent_doc_id;
                    let agent = &f.agent;
                    Box::pin(async move {
                        crate::session::stage_created_session_relationships_in_txn(
                            txn,
                            session_doc,
                            agent_doc,
                            &identity::Did::new(agent.did())?,
                        )
                        .await
                    })
                })
                .await
                .unwrap();
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
        // A delivery's source is the caller's pinned job row in its own
        // session; the delivered request's provenance and job keys name it.
        let tool_call_doc_id = {
            let mut pin = crate::tools::call::lifecycle::admission_fixture::fixture_job_pin();
            pin.chain_root_doc_id = name.to_owned();
            crate::tools::background::job::input::tests::insert_as(
                &access,
                "AgentToolCall",
                json!({
                    "request_id": f.parent.request_id,
                    "request_doc_id": f.parent.doc_id,
                    "node_did": f.identity.did(),
                    "requester_did": f.parent.requester_did,
                    "session_id": f.parent.session_id,
                    "tool_call_id": "delivery-call",
                    "tool_name": "workflow",
                    "lifecycle_state": "running",
                    "started_at": "2026-10-21T00:00:00Z",
                    "job_pin": pin,
                }),
            )
            .await
        };
        let cause = SessionMessageCause {
            caller_node_did: f.identity.did().into(),
            caller_agent_did: f.agent.did().to_owned(),
            caller_session_id: f.parent.session_id.clone(),
            caller_requester_did: f.parent.requester_did.clone(),
            caller_request_id: f.parent.request_id.clone(),
            caller_request_doc_id: f.parent.doc_id.clone(),
            caller_hop: 0,
            tool_call_id: "delivery-call".into(),
            tool_call_doc_id,
            correlation: None,
        };
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

#[tokio::test]
async fn staging_into_another_runtime_is_refused_before_any_write() {
    let before = json!({"status":"active","sequence":0,"last_continued_from":null,
        "latest_request":10,"children":[],"blocked_audits":0,"wrapup_requested":false,
        "wrapup_completed":false,"tokens_used":37,"token_budget":1000});
    let f = Fixture::new(&before).await;
    let cause = SessionMessageCause {
        caller_node_did: f.identity.did().into(),
        caller_agent_did: f.agent.did().to_owned(),
        caller_session_id: f.parent.session_id.clone(),
        caller_requester_did: f.parent.requester_did.clone(),
        caller_request_id: f.parent.request_id.clone(),
        caller_request_doc_id: f.parent.doc_id.clone(),
        caller_hop: 0,
        tool_call_id: "remote-call".into(),
        tool_call_doc_id: "remote-call-doc".into(),
        correlation: None,
    };
    let retry_key = job_message_key("remote-staging", "worker", 1, 1);
    for existing in [false, true] {
        let access = f.agent_access();
        let refused = access
            .transact_write("test.remote_staging", |txn| {
                let retry_key = retry_key.clone();
                let cause = &cause;
                let agent = f.agent.did();
                Box::pin(async move {
                    stage_request(
                        txn,
                        cause,
                        "did:key:z6MkpeerRuntimeThatIsNotThisOne",
                        agent,
                        job_session_id("remote-staging", "worker", 1),
                        "Delivery work",
                        None,
                        retry_key,
                        1,
                        None,
                        existing,
                    )
                    .await
                })
            })
            .await;
        let error = refused.expect_err("cross-runtime staging must be refused");
        assert!(
            format!("{error:#}").contains("cross-runtime agent delivery is not supported"),
            "{error:#}"
        );
        let rows = access
            .execute(&format!(
                r#"{{AgentRequest(filter: {{retry_key: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                escape_graphql_string(&retry_key)
            ))
            .await
            .unwrap();
        assert!(rows["data"]["AgentRequest"].as_array().unwrap().is_empty());
    }
    f.node.shutdown().await.unwrap();
}
