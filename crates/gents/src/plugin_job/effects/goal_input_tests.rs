use super::*;
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use crate::lifecycle::test_support::{pin_fixed_signing_identity, PIN_FIXED_DID};
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};
use serde_json::Value;

fn reference(value: &Value, name: &str, roots: &[String; 2]) -> Option<GoalAssignmentInput> {
    value.is_object().then(|| GoalAssignmentInput {
        goal_id: format!("{name}-goal-{}", value["goal"]),
        root_request_doc_id: value["root"]
            .as_u64()
            .map(|root| roots[(root / 100 - 1) as usize].clone()),
    })
}

async fn write(node: &Arc<EmbeddedNode>, mutation: &str) -> Value {
    ConfigAccess::write_local(node, "test.delivery_goal_input", mutation)
        .await
        .unwrap()
}

async fn observe_goal(
    node: &Arc<EmbeddedNode>,
    owner: &str,
    session: &str,
    assignment: Option<&GoalAssignmentInput>,
) {
    let Some(assignment) = assignment else { return };
    let goal = crate::goal::set_goal(node, owner, session, Some("Destination work"), None, None)
        .await
        .unwrap();
    let root = assignment
        .root_request_doc_id
        .as_deref()
        .map(|root| format!("\"{}\"", escape_graphql_string(root)))
        .unwrap_or_else(|| "null".into());
    write(node, &format!(r#"mutation {{update_Goal(docID: "{}", input: {{goal_id: "{}", assignment_root_request_doc_id: {root}}}) {{_docID}}}}"#,
        escape_graphql_string(&goal.doc_id), escape_graphql_string(&assignment.goal_id))).await;
}

async fn stored(node: &Arc<EmbeddedNode>, request_id: &str) -> AgentRequestRow {
    let response = ConfigAccess::local(node.clone())
        .execute(&format!(
            r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{{}}}}}"#,
            escape_graphql_string(request_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ))
        .await
        .unwrap();
    let rows = response["data"]["AgentRequest"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row: AgentRequestRow = serde_json::from_value(rows[0].clone()).unwrap();
    crate::request_admission::verify_request_receipt_signature(&row).unwrap();
    row
}

async fn deliver(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    case: &Value,
    owner: &str,
    session: &str,
    captured: Option<GoalAssignmentInput>,
) -> Result<(String, String)> {
    let name = case["name"].as_str().unwrap();
    if case["plugin"] == true {
        let retry_key = if case["existing"] == true {
            job_message_key(name, "worker", 1, 1)
        } else {
            job_spawn_key(name, "worker", 1)
        };
        let create =
            ConfigAccess::transact_local_write(node, None, "test.plugin_delivery", |txn| {
                let retry_key = retry_key.clone();
                Box::pin(async move {
                    stage_request(
                        txn,
                        cause,
                        owner,
                        "agent",
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
        let row = ConfigAccess::local(node.clone())
            .execute(&format!(
                r#"{{AgentRequest(filter: {{retry_key: {{_eq: "{}"}}}}) {{_docID request_id}}}}"#,
                escape_graphql_string(&retry_key)
            ))
            .await?;
        let row = &row["data"]["AgentRequest"][0];
        assert_eq!(
            create.input.goal_assignment,
            stored(node, row["request_id"].as_str().unwrap())
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
        owner_did: PIN_FIXED_DID.into(),
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
    let mut spec = crate::lifecycle::materialize::pending_agent_request_spec(
        PIN_FIXED_DID,
        "agent",
        "Delivery work",
        crate::lifecycle::ExecutionOrigin::Interactive,
        crate::lifecycle::TriggerLineage {
            trigger_kind: Some("manual".into()),
            ..Default::default()
        },
        None,
        None,
        &fire.request_id,
        session,
        Some(&fire.fire_key),
        Some(PIN_FIXED_DID),
        None,
    )?;
    spec.input.goal_assignment = captured;
    let create = crate::lifecycle::build_signed_request(
        spec,
        crate::lifecycle::RequestSigner::RegisteredTarget,
    )
    .await?;
    let prepared = crate::triggers::engine::durable::PreparedFire {
        receipt: fire,
        target_existing: case["existing"] == true,
        replace_prior: false,
    };
    let admitted = crate::lifecycle::write_trigger_delivery(
        node,
        identity::Did::new(PIN_FIXED_DID).unwrap(),
        &prepared,
        &create,
    )
    .await?;
    Ok((admitted.request.doc_id, admitted.request.request_id))
}

#[tokio::test]
async fn generated_deliveries_capture_destination_goal_in_the_admission_transaction() {
    let contracts: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contracts["goal_delivery_input_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 20);
    let directory = tempfile::tempdir().unwrap();
    let signer = pin_fixed_signing_identity(directory.path());
    let remote: Arc<dyn NodeIdentity> =
        Arc::new(KeyIdentity::load_or_create(directory.path().join("remote.key"), None).unwrap());
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(PIN_FIXED_DID)
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    crate::test_support::install_test_agent(&node, PIN_FIXED_DID, "agent").await;
    crate::test_support::install_test_agent(&node, remote.did(), "agent").await;
    let mut caller = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "delivery-caller",
        PIN_FIXED_DID,
        PIN_FIXED_DID,
        "agent",
        "caller-session",
        "Send destination work",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(PIN_FIXED_DID),
    );
    caller.input.goal_assignment = Some(GoalAssignmentInput {
        goal_id: "caller-goal".into(),
        root_request_doc_id: Some("caller-root".into()),
    });
    crate::sign_agent_request_create(&signer, &mut caller)
        .await
        .unwrap();
    let caller = write(&node, &caller.graphql_mutation().unwrap()).await;
    let cause = SessionMessageCause {
        caller_node_did: PIN_FIXED_DID.into(),
        caller_agent_id: "caller-agent".to_owned(),
        caller_session_id: "caller-session".to_owned(),
        caller_requester_did: None,
        caller_request_id: "delivery-caller".into(),
        caller_request_doc_id: crate::graphql::created_doc_id(&caller, "AgentRequest").unwrap(),
        caller_hop: 0,
        tool_call_id: "delivery-call".into(),
        tool_call_doc_id: "delivery-call-doc".into(),
        correlation: None,
    };
    let mut failures = Vec::new();
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let target: &dyn NodeIdentity = if case["remote"] == true {
            remote.as_ref()
        } else {
            &signer
        };
        let owner = target.did();
        let identity = FireIdentity {
            owner_did: PIN_FIXED_DID.into(),
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
            continue;
        }
        if case["existing"] == true {
            crate::session::ensure_session_with_agent_id_and_requester_did(
                &node,
                &session,
                owner,
                "agent",
                Some(PIN_FIXED_DID),
            )
            .await
            .unwrap();
        }
        let mut roots = [String::new(), String::new()];
        for (index, root) in roots.iter_mut().enumerate() {
            let mut opening = AgentRequestCreate::base(
                RequestPurpose::Normal,
                format!("{name}-root-{index}"),
                owner,
                owner,
                "agent",
                &session,
                "Destination opening",
                "interactive",
                "2026-10-21T00:00:00Z",
                AgentRequestAdmissionRecord::local_self(owner),
            );
            crate::sign_agent_request_create(target, &mut opening)
                .await
                .unwrap();
            *root = crate::graphql::created_doc_id(
                &write(&node, &opening.graphql_mutation().unwrap()).await,
                "AgentRequest",
            )
            .unwrap();
        }
        let captured = reference(&case["captured"], name, &roots);
        let observed = reference(&case["observed"], name, &roots);
        let prior = reference(&case["admitted_reference"], name, &roots);
        let first_assignment = if case["admitted"] == true {
            prior.as_ref()
        } else {
            observed.as_ref()
        };
        observe_goal(&node, owner, &session, first_assignment).await;
        if case["remote"] == true && case["observation_present"] == true {
            crate::goal::reconcile_goal_observations(&node, &remote, true, &[])
                .await
                .unwrap();
            let response = ConfigAccess::local(node.clone()).execute(&format!(
                r#"{{GoalObservation(filter: {{node_did: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{_docID observation_json}}}}"#,
                escape_graphql_string(owner), escape_graphql_string(&session))).await.unwrap();
            let row = &response["data"]["GoalObservation"][0];
            let mut observation: gents_protocol::goal_control::SessionGoalObservation =
                serde_json::from_str(row["observation_json"].as_str().unwrap()).unwrap();
            if let Some(goal) = &mut observation.goal {
                if case["foreign_owner"] == true {
                    goal.snapshot.node_did = PIN_FIXED_DID.into();
                }
                if case["foreign_session"] == true {
                    goal.snapshot.session_id = "foreign-session".into();
                }
            }
            observation.signature = remote
                .sign(&observation.signing_payload().unwrap())
                .await
                .unwrap();
            if case["observation_verified"] == false {
                observation.signature[0] ^= 1;
            }
            write(&node, &format!(
                r#"mutation {{update_GoalObservation(docID: "{}", input: {{observation_json: "{}"}}) {{_docID}}}}"#,
                escape_graphql_string(row["_docID"].as_str().unwrap()), escape_graphql_string(&serde_json::to_string(&observation).unwrap()))).await;
        }
        let first = deliver(&node, &cause, case, owner, &session, captured.clone()).await;
        if case["expected"]["kind"] == "pending" {
            if first.is_ok() {
                failures.push(format!(
                    "{name}: unverified destination input was published"
                ));
            } else {
                let response = ConfigAccess::local(node.clone())
                    .execute(&format!(
                        r#"{{AgentRequest(filter: {{retry_key: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                        escape_graphql_string(&job_message_key(name, "worker", 1, 1))
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
            }
            continue;
        }
        let first = first.unwrap();
        let initial_row = stored(&node, &first.1).await;
        if case["admitted"] == true {
            observe_goal(&node, owner, &session, observed.as_ref()).await;
        }
        let result = deliver(&node, &cause, case, owner, &session, captured)
            .await
            .unwrap();
        assert_eq!(
            first, result,
            "{name}: duplicate keeps exact physical request"
        );
        let row = stored(&node, &result.1).await;
        assert_eq!(
            row.admission_signature, initial_row.admission_signature,
            "{name}: duplicate keeps signed receipt"
        );
        assert_eq!(
            row.input, initial_row.input,
            "{name}: duplicate keeps signed input"
        );
        let expected = reference(&case["expected"]["reference"], name, &roots);
        let actual = row
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.clone());
        if actual != expected {
            failures.push(format!("{name}: expected {expected:?}, got {actual:?}"));
        }
        assert_ne!(
            actual.as_ref().map(|r| r.goal_id.as_str()),
            Some("caller-goal")
        );
    }
    node.shutdown().await;
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
