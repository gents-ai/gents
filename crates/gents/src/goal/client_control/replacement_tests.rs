use super::*;
use crate::goal::operator_resume::support::{Fixture, PARENT, SESSION};

pub(super) async fn run(case: &Value) {
    let c = &case["replacement"];
    let start = &c["start"];
    let before = &c["before"]["current"];
    let execution = &before["snapshot"]["execution"];
    let fixture = Fixture::new_with_parent_state(execution, start["busy"] == false).await;
    let node = &fixture.node;
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    let access = ConfigAccess::local(node.clone());
    crate::session::ensure_session_with_agent_id_and_requester_did(
        node,
        SESSION,
        identity.did(),
        "contract-behavior",
        Some(identity.did()),
    )
    .await
    .unwrap();
    let session = access
        .transact_readonly("test.replacement_session", |txn| {
            Box::pin(async {
                crate::session::load_agent_session_owner_in_txn(txn, identity.did(), SESSION).await
            })
        })
        .await
        .unwrap()
        .unwrap();
    let old_root = &fixture.parent.doc_id;
    access
        .write(
            "test.replacement_accounting",
            &format!(
                r#"mutation {{update_Goal(docID: "{}", input: {{
        active_time_seconds: {}, active_started_at: null, token_usage_baseline: {},
        assignment_root_request_doc_id: "{}"
    }}) {{_docID}}}}"#,
                escape_graphql_string(&fixture.goal.doc_id),
                before["active_seconds"],
                before["usage_baseline"],
                escape_graphql_string(old_root)
            ),
        )
        .await
        .unwrap();
    let owner = escape_graphql_string(identity.did());
    access.write("test.replacement_usage", &format!(r#"mutation {{add_InferenceCall(input: {{
        call_id: "old-usage", runtime_instance_id: "contract", request_id: "{PARENT}",
        request_doc_id: "{}", call_seq: 1, backend_id: "contract", agent_id: "contract-behavior",
        node_did: "{owner}", call_kind: "inference", attempt: 1, call_state: "completed",
        queued_at: "2026-01-01T00:00:00Z", started_at: "2026-01-01T00:00:00Z", ended_at: "2026-01-01T00:00:01Z",
        priority: 0, queue_depth_at_enqueue: 0, controller_generation: 1, backend_config_fingerprint: "contract",
        prompt_tokens: {}, completion_tokens: 0
    }}) {{_docID}}}}"#, escape_graphql_string(old_root), c["session_usage"])).await.unwrap();
    let old_claim = deterministic_goal_creation_key(identity.did(), SESSION);
    access.write("test.replacement_original_claim", &format!(r#"mutation {{create_GoalCreationClaim(input: {{
        creation_key: "{}", goal_id: "{}", node_did: "{owner}", session_id: "{SESSION}",
        objective: "Finish the original work", token_budget: 1000, created_at: "2026-01-01T00:00:00Z"
    }}) {{_docID}}}}"#, escape_graphql_string(&old_claim), escape_graphql_string(&fixture.goal.goal_id))).await.unwrap();
    let original = load_canonical_goal(node, identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let mut historical_doc = None;
    if !c["before"]["retained"].as_array().unwrap().is_empty() {
        let row = access
            .execute(&format!(
                r#"{{Goal(docID: "{}") {{{GOAL_FIELDS}}}}}"#,
                escape_graphql_string(&original.doc_id)
            ))
            .await
            .unwrap();
        let mut input = row["data"]["Goal"][0].clone();
        let fields = input.as_object_mut().unwrap();
        fields.remove("_docID");
        fields.insert("goal_id".into(), json!("historical-assignment"));
        fields.insert("creation_key".into(), json!("historical-claim"));
        fields.insert("status".into(), json!("complete"));
        let response = access.transact_write("test.retained_goal", |txn| {
            let input = input.clone();
            Box::pin(async move {
                let created = txn.execute_with_variables(
                    "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}", &json!({"input":input}),
                ).await?;
                let id = crate::store::graphql::created_doc_id(&created, "Goal")?;
                txn.execute(&format!(r#"mutation {{delete_Goal(docID: "{}") {{_docID}}}}"#, escape_graphql_string(&id))).await?;
                Ok::<_, anyhow::Error>(id)
            })
        }).await.unwrap();
        historical_doc = Some(response);
    }
    let command_id = "replacement-command";
    let fresh_identity = replacement_goal_identity(identity.did(), SESSION, command_id);
    if !start["claim"].is_null() {
        access
            .write(
                "test.replacement_conflicting_claim",
                &format!(
                    r#"mutation {{create_GoalCreationClaim(input: {{
            creation_key: "{}", goal_id: "{}", node_did: "{owner}", session_id: "{SESSION}",
            objective: "earlier", token_budget: 1000, created_at: "2026-01-01T00:00:00Z"
        }}) {{_docID}}}}"#,
                    escape_graphql_string(&fresh_identity.creation_key),
                    escape_graphql_string(&fresh_identity.goal_id)
                ),
            )
            .await
            .unwrap();
    }
    let claims_before = access
        .execute("{GoalCreationClaim {_docID creation_key goal_id objective token_budget}}")
        .await
        .unwrap();
    let current = crate::store::graphql::newest_document_composite_commit(
        node,
        &original.doc_id,
        "test.replacement_revision",
    )
    .await
    .unwrap()
    .unwrap();
    let mut initial = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "replacement-first",
        identity.did(),
        identity.did(),
        "contract-behavior",
        SESSION,
        start["objective"].as_str().unwrap(),
        "interactive",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    initial.input = crate::request::lifecycle::prepare_user_message_input(
        &access,
        identity.did(),
        SESSION,
        RequestInput::default(),
        QueueDelivery::Queue,
    )
    .await
    .unwrap();
    initial.input.goal_assignment = start["first_goal"].as_str().map(|goal| {
        gents_protocol::request_input::GoalAssignmentInput {
            goal_id: match goal {
                "new-assignment" => fresh_identity.goal_id.clone(),
                "old-assignment" => original.goal_id.clone(),
                other => other.to_owned(),
            },
            root_request_doc_id: start["first_root"].as_str().map(|root| {
                if root == "old-first-request" {
                    old_root.clone()
                } else {
                    root.into()
                }
            }),
        }
    });
    initial.retry_key = Some(format!("session-goal:{command_id}"));
    crate::sign_agent_request_create(identity.as_ref(), &mut initial)
        .await
        .unwrap();
    if start["first_request_valid"] == false {
        initial.admission.signature[0] ^= 1;
    }
    let now = Utc::now();
    let mut command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: command_id.into(),
        requester_did: identity.did().into(),
        requester_peer_id: None,
        node_did: identity.did().into(),
        session_id: SESSION.into(),
        session_doc_id: Some(
            if start["expected_session_document"] == "session-document" {
                session.doc_id.clone()
            } else {
                "stale-session".into()
            },
        ),
        issued_at: (now - chrono::Duration::seconds(20)).to_rfc3339(),
        expires_at: (now
            + chrono::Duration::seconds(
                if start["now"].as_i64().unwrap() >= start["expires_at"].as_i64().unwrap() {
                    -10
                } else {
                    300
                },
            ))
        .to_rfc3339(),
        expected: Some(GoalRevision {
            document: if c["expected_revision"]["document"]
                == before["snapshot"]["revision"]["document"]
            {
                original.doc_id.clone()
            } else {
                "foreign-document".into()
            },
            composite: if c["expected_revision"]["composite"]
                == before["snapshot"]["revision"]["composite"]
            {
                current.cid
            } else {
                "stale-revision".into()
            },
        }),
        operation: GoalOperation::Create {
            objective: start["objective"].as_str().unwrap().into(),
            token_budget: start["budget"].as_i64(),
        },
        initial_request: Some(Box::new(initial)),
        signature: Vec::new(),
    };
    command.signature = identity
        .sign(&command.signing_payload().unwrap())
        .await
        .unwrap();
    if start["signature_valid"] == false {
        command.signature[0] ^= 1;
    }
    let doc_id = persist_command(&access, &command).await;
    let accepted = !c["expected"].is_null();
    if start["commit"] == true {
        process_command(node, &identity, &doc_id).await.unwrap();
        let result = receipt(&access, &doc_id).await.unwrap();
        result.validate_for(&command).unwrap();
        assert!(identity
            .verify(
                identity.did(),
                &result.signing_payload().unwrap(),
                &result.signature
            )
            .await
            .unwrap());
        assert_eq!(
            matches!(result.outcome, GoalControlOutcome::Applied { .. }),
            accepted,
            "{}: {:?}",
            case["name"],
            result.outcome
        );
    } else {
        let txn = ConfigApplyTxn::begin_local(node, None).await.unwrap();
        let prepared = prepare_goal_control(&txn, &command).await.unwrap().unwrap();
        stage_goal_control(&txn, identity.as_ref(), prepared)
            .await
            .unwrap();
        txn.discard().await.unwrap();
        assert!(receipt(&access, &doc_id).await.is_none());
    }
    let after = load_canonical_goal(node, identity.did(), SESSION)
        .await
        .unwrap()
        .unwrap();
    let history = load_goal_by_id(node, identity.did(), &original.goal_id)
        .await
        .unwrap();
    if history.is_none() {
        let diagnostic = access
            .execute(&format!(
                r#"{{
            all: Goal(showDeleted: true) {{{GOAL_FIELDS}}}
            physical: Goal(docID: "{}", showDeleted: true) {{{GOAL_FIELDS}}}
        }}"#,
                escape_graphql_string(&original.doc_id)
            ))
            .await
            .unwrap();
        panic!(
            "{}: retained assignment {} is absent from its ID lookup: {diagnostic}",
            case["name"], original.goal_id
        );
    }
    let history = history.unwrap();
    assert_eq!(
        serde_json::to_value(&history).unwrap(),
        serde_json::to_value(&original).unwrap(),
        "{} prior accounting/history",
        case["name"]
    );
    let claims_after = access
        .execute("{GoalCreationClaim {_docID creation_key goal_id objective token_budget}}")
        .await
        .unwrap();
    for original_claim in claims_before["data"]["GoalCreationClaim"]
        .as_array()
        .unwrap()
    {
        assert!(claims_after["data"]["GoalCreationClaim"]
            .as_array()
            .unwrap()
            .contains(original_claim));
    }
    if accepted {
        let expected = &c["expected"]["current"];
        let fields = &expected["snapshot"]["execution"];
        assert_eq!(after.goal_id, fresh_identity.goal_id);
        assert_ne!(after.doc_id, original.doc_id);
        assert_eq!(after.status, fields["status"].as_str().unwrap());
        assert_eq!(
            after.objective,
            expected["snapshot"]["objective"].as_str().unwrap()
        );
        assert_eq!(after.tokens_used, fields["tokens_used"].as_i64());
        assert_eq!(after.token_budget, fields["token_budget"].as_i64());
        assert_eq!(
            after.token_usage_baseline,
            expected["usage_baseline"].as_i64()
        );
        assert_eq!(
            after.active_time_seconds,
            expected["active_seconds"].as_i64()
        );
        assert_eq!(
            after.continuation_sequence(),
            fields["sequence"].as_i64().unwrap()
        );
        let requests = access
            .execute("{AgentRequest(filter: {request_id: {_eq: \"replacement-first\"}}) {_docID}}")
            .await
            .unwrap();
        let request_doc = requests["data"]["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap();
        assert_eq!(
            after.assignment_root_request_doc_id.as_deref(),
            Some(request_doc)
        );
        assert_eq!(
            claims_after["data"]["GoalCreationClaim"]
                .as_array()
                .unwrap()
                .len(),
            claims_before["data"]["GoalCreationClaim"]
                .as_array()
                .unwrap()
                .len()
                + 1
        );
    } else {
        assert_eq!(
            serde_json::to_value(after).unwrap(),
            serde_json::to_value(&original).unwrap(),
            "{} refusal/rollback",
            case["name"]
        );
        assert_eq!(claims_after, claims_before);
    }
    if let Some(id) = historical_doc {
        let rows = access
            .execute(&format!(
                r#"{{Goal(docID: "{}", showDeleted: true) {{goal_id status tokens_used}}}}"#,
                escape_graphql_string(&id)
            ))
            .await
            .unwrap();
        assert_eq!(rows["data"]["Goal"][0]["status"], "complete");
        assert_eq!(
            rows["data"]["Goal"][0]["tokens_used"],
            execution["tokens_used"]
        );
    }
    let session_after = access
        .transact_readonly("test.replacement_session_after", |txn| {
            Box::pin(async {
                crate::session::load_agent_session_owner_in_txn(txn, identity.did(), SESSION).await
            })
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.session, session_after.session);
    node.shutdown().await;
}

#[test]
fn replacement_identity_separates_opaque_scope_components() {
    let first = replacement_goal_identity("node", "session", "a:1:b");
    let second = replacement_goal_identity("node", "session:5:a", "b");
    assert_ne!(first.goal_id, second.goal_id);
    assert_ne!(first.creation_key, second.creation_key);
    let replay = replacement_goal_identity("node", "session", "a:1:b");
    assert_eq!(first.goal_id, replay.goal_id);
    assert_eq!(first.creation_key, replay.creation_key);
}
