use super::*;
use crate::goal::operator_resume::support::{Fixture, PARENT, SESSION};
use gents_lean_vocab::families::goal_client_start_contracts_replacements::{
    ReplacedWire, ReplacementInput, ReplacementOutput,
};
use gents_protocol::request_admission::RequestPurpose;
use std::sync::Arc;

#[tokio::test]
async fn generated_goal_replacements_retain_prior_assignments() {
    for modeled in gents_lean_vocab::families::goal_client_start_contracts_replacements::cases::<
        String,
        String,
    >() {
        let output = drive_replacement(&modeled.input).await;
        modeled.check(output);
    }
}

async fn drive_replacement(c: &ReplacementInput<String, String>) -> ReplacementOutput<String> {
    let start = &c.start;
    let before = &c.before.current;
    let execution = serde_json::to_value(&before.snapshot.execution).unwrap();
    let fixture = Fixture::new_with_parent_state(&execution, !start.busy).await;
    let node = &fixture.node;
    let identity: Arc<dyn NodeIdentity> = fixture.identity.clone();
    let access = fixture.user_access();
    let session = access
        .transact_readonly("test.replacement_session", |txn| {
            Box::pin(async {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    identity.did(),
                    SESSION,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        ::identity::Did::new(&session.session.agent_did).unwrap(),
        fixture.agent.as_ref(),
    )
    .unwrap();
    let old_root = &fixture.parent.doc_id;
    agent_access
        .write(
            "test.replacement_accounting",
            &format!(
                r#"mutation {{update_Goal(docID: "{}", input: {{
        active_time_seconds: {}, active_started_at: null, token_usage_baseline: {},
        assignment_root_request_doc_id: "{}"
    }}) {{_docID}}}}"#,
                escape_graphql_string(&fixture.goal.doc_id),
                before.active_seconds,
                before.usage_baseline,
                escape_graphql_string(old_root)
            ),
        )
        .await
        .unwrap();
    let owner = escape_graphql_string(identity.did());
    agent_access.write("test.replacement_usage", &format!(r#"mutation {{add_InferenceCall(input: {{
        call_id: "old-usage", runtime_instance_id: "contract", request_id: "{PARENT}",
        request_doc_id: "{}", call_seq: 1, backend_id: "contract", agent_did: "{}",
        node_did: "{owner}", call_kind: "inference", attempt: 1, call_state: "completed",
        queued_at: "2026-01-01T00:00:00Z", started_at: "2026-01-01T00:00:00Z", ended_at: "2026-01-01T00:00:01Z",
        priority: 0, queue_depth_at_enqueue: 0, controller_generation: 1, backend_config_fingerprint: "contract",
        prompt_tokens: {}, completion_tokens: 0
    }}) {{_docID}}}}"#, escape_graphql_string(old_root), escape_graphql_string(fixture.agent.did()), c.session_usage)).await.unwrap();
    let old_claim = deterministic_goal_creation_key(identity.did(), SESSION);
    agent_access.write("test.replacement_original_claim", &format!(r#"mutation {{create_GoalCreationClaim(input: {{
        creation_key: "{}", goal_id: "{}", node_did: "{owner}", session_id: "{SESSION}",
        session_doc_id: "{}", agent_did: "{}",
        objective: "Finish the original work", token_budget: 1000, created_at: "2026-01-01T00:00:00Z"
    }}) {{_docID}}}}"#, escape_graphql_string(&old_claim), escape_graphql_string(&fixture.goal.goal_id),
        escape_graphql_string(&session.doc_id), escape_graphql_string(&session.session.agent_did))).await.unwrap();
    let original = fixture.current_goal().await;
    let mut historical_doc = None;
    if !c.before.retained.is_empty() {
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
        let response = agent_access.transact_write("test.retained_goal", |txn| {
            let input = input.clone();
            Box::pin(async move {
                let created = txn.execute_with_variables(
                    "mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}", &json!({"input":input}),
                ).await?;
                let id = gents_store::graphql::created_doc_id(&created, "Goal")?;
                txn.execute(&format!(r#"mutation {{delete_Goal(docID: "{}") {{_docID}}}}"#, escape_graphql_string(&id))).await?;
                Ok::<_, anyhow::Error>(id)
            })
        }).await.unwrap();
        historical_doc = Some(response);
    }
    let command_id = "replacement-command";
    let fresh_identity = replacement_goal_identity(identity.did(), SESSION, command_id);
    if start.claim.is_some() {
        agent_access
            .write(
                "test.replacement_conflicting_claim",
                &format!(
                    r#"mutation {{create_GoalCreationClaim(input: {{
            creation_key: "{}", goal_id: "{}", node_did: "{owner}", session_id: "{SESSION}",
            session_doc_id: "{}", agent_did: "{}",
            objective: "earlier", token_budget: 1000, created_at: "2026-01-01T00:00:00Z"
        }}) {{_docID}}}}"#,
                    escape_graphql_string(&fresh_identity.creation_key),
                    escape_graphql_string(&fresh_identity.goal_id),
                    escape_graphql_string(&session.doc_id),
                    escape_graphql_string(&session.session.agent_did)
                ),
            )
            .await
            .unwrap();
    }
    let claims_before = access
        .execute("{GoalCreationClaim {_docID creation_key goal_id objective token_budget}}")
        .await
        .unwrap();
    let current = fixture.goal_commit(&original.doc_id).await;
    let mut initial = AgentRequestCreate::base(
        RequestPurpose::Normal,
        "replacement-first",
        identity.did(),
        fixture.user.did(),
        &fixture.parent.agent_did,
        SESSION,
        &start.objective,
        "interactive",
        Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        AgentRequestAdmissionRecord::client(fixture.user.did()),
    );
    initial.session_doc_id = Some(fixture.session_doc_id.clone());
    initial.agent_doc_id = Some(fixture.agent_doc_id.clone());
    initial.input = gents_session::request::lifecycle::prepare_user_message_input(
        &access,
        identity.did(),
        SESSION,
        RequestInput::default(),
        QueueDelivery::Queue,
    )
    .await
    .unwrap();
    initial.input.goal_assignment = start.first_goal.as_deref().map(|goal| {
        gents_protocol::request_input::GoalAssignmentInput {
            goal_id: match goal {
                "new-assignment" => fresh_identity.goal_id.clone(),
                "old-assignment" => original.goal_id.clone(),
                other => other.to_owned(),
            },
            root_request_doc_id: start.first_root.as_deref().map(|root| {
                if root == "old-first-request" {
                    old_root.clone()
                } else {
                    root.into()
                }
            }),
        }
    });
    initial.retry_key = Some(format!("session-goal:{command_id}"));
    gents_session::request::admission::sign_agent_request_create(
        fixture.user.as_ref(),
        &mut initial,
    )
    .await
    .unwrap();
    if !start.first_request_valid {
        initial.admission.signature[0] ^= 1;
    }
    let wrong_session = if start.expected_session_document.as_deref() != Some("session-document") {
        assert_eq!(
            start.expected_session_document.as_deref(),
            Some("stale-session")
        );
        let alternate = access
            .transact_write("test.replacement.alternate_session", |txn| {
                let identity = identity.as_ref();
                let user = fixture.user.as_ref();
                let agent_did = &session.session.agent_did;
                Box::pin(async move {
                    gents_session::session::ensure_session_in_txn(
                        txn,
                        "replacement-alternate-session",
                        identity.did(),
                        agent_did,
                        Some(user.did()),
                        None,
                        None,
                        "2026-01-01T00:00:01Z",
                    )
                    .await?;
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        identity.did(),
                        "replacement-alternate-session",
                    )
                    .await?
                    .context("alternate session is missing")
                })
            })
            .await
            .unwrap();
        assert_ne!(alternate.doc_id, session.doc_id);
        gents_session::session::grant_created_session_relationships(
            &access,
            &alternate.doc_id,
            &fixture.agent_doc_id,
            &session.session.agent_did,
        )
        .await
        .unwrap();
        Some(alternate)
    } else {
        None
    };
    let now = Utc::now();
    let mut command = SessionGoalControl {
        version: SESSION_GOAL_CONTROL_VERSION,
        command_id: command_id.into(),
        requester_did: fixture.user.did().into(),
        requester_peer_id: None,
        node_did: identity.did().into(),
        agent_did: session.session.agent_did.clone(),
        agent_doc_id: super::agent_document_for_test(
            &access,
            identity.did(),
            &session.session.agent_did,
        )
        .await,
        session_id: SESSION.into(),
        session_doc_id: Some(
            wrong_session
                .as_ref()
                .map(|alternate| &alternate.doc_id)
                .unwrap_or(&session.doc_id)
                .clone(),
        ),
        issued_at: (now - chrono::Duration::seconds(20)).to_rfc3339(),
        expires_at: (now
            + chrono::Duration::seconds(if start.now >= start.expires_at {
                -10
            } else {
                300
            }))
        .to_rfc3339(),
        expected: Some(GoalRevision {
            document: if c.expected_revision.document == before.snapshot.revision.document {
                original.doc_id.clone()
            } else {
                "foreign-document".into()
            },
            composite: if c.expected_revision.composite == before.snapshot.revision.composite {
                current.cid
            } else {
                "stale-revision".into()
            },
        }),
        operation: GoalOperation::Create {
            objective: start.objective.clone(),
            token_budget: start.budget,
        },
        initial_request: Some(Box::new(initial)),
        native_evidence: None,
        signature: Vec::new(),
    };
    let initial = command.initial_request.as_deref().unwrap();
    command.native_evidence = Some(gents_protocol::goal_control::GoalNativeEvidence {
        request: gents_store::config_client::prepare_request_evidence(
            fixture.user.as_ref(),
            gents_store::config_client::client_request_input(initial).unwrap(),
        )
        .await
        .unwrap(),
        replacement_delete: Some(
            gents_store::config_client::prepare_goal_delete_evidence(
                &access,
                fixture.user.as_ref(),
                &original.doc_id,
            )
            .await
            .unwrap(),
        ),
    });
    command.signature = fixture
        .user
        .sign(&command.signing_payload().unwrap())
        .await
        .unwrap();
    if !start.signature_valid {
        command.signature[0] ^= 1;
    }
    if let Some(alternate) = &wrong_session {
        assert!(start.signature_valid);
        assert!(start.first_request_valid);
        let initial = command.initial_request.as_deref().unwrap();
        assert_eq!(
            initial.session_doc_id.as_deref(),
            Some(session.doc_id.as_str())
        );
        assert_eq!(
            command.session_doc_id.as_deref(),
            Some(alternate.doc_id.as_str())
        );
        assert_eq!(initial.session_id, session.session.session_id);
        assert_eq!(initial.node_did, session.session.node_did);
        assert_eq!(initial.agent_did, session.session.agent_did);
        assert_eq!(
            Some(initial.requester_did.as_str()),
            session.session.requester_did.as_deref()
        );
        assert!(fixture
            .user
            .verify(
                fixture.user.did(),
                &initial.signing_payload(),
                &initial.admission.signature
            )
            .await
            .unwrap());
        assert!(fixture
            .user
            .verify(
                fixture.user.did(),
                &command.signing_payload().unwrap(),
                &command.signature
            )
            .await
            .unwrap());
        let doc_id = persist_command(&access, &command).await;
        for target in [&session.doc_id, &alternate.doc_id] {
            let visible = agent_access
                .execute(&format!(
                    r#"{{ AgentSession(docID: "{}") {{ _docID }} }}"#,
                    escape_graphql_string(target)
                ))
                .await
                .unwrap();
            assert_eq!(visible["data"]["AgentSession"][0]["_docID"], *target);
        }
        let visible_command = agent_access
            .execute(&format!(
                r#"{{ AgentSessionGoalControl(docID: "{}") {{ _docID }} }}"#,
                escape_graphql_string(&doc_id)
            ))
            .await
            .unwrap();
        assert_eq!(
            visible_command["data"]["AgentSessionGoalControl"][0]["_docID"],
            doc_id
        );
        let before_effects = agent_access
            .execute("{ GoalCreationClaim { _docID creation_key goal_id } AgentRequest { _docID request_id } }")
            .await
            .unwrap();
        assert!(!before_effects["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["request_id"] == "replacement-first"));
        process_command(node, &identity, &doc_id, fixture.agent.as_ref())
            .await
            .unwrap();
        assert!(
            receipt(&agent_access, &doc_id).await.is_none(),
            "{}",
            c.name
        );
        assert!(receipt(&access, &doc_id).await.is_none(), "{}", c.name);
        assert_eq!(agent_access.execute("{ GoalCreationClaim { _docID creation_key goal_id } AgentRequest { _docID request_id } }").await.unwrap(), before_effects);
        assert_eq!(
            serde_json::to_value(fixture.current_goal().await).unwrap(),
            serde_json::to_value(&original).unwrap()
        );
        let after = access
            .transact_readonly("test.replacement.current_session_after", |txn| {
                Box::pin(
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        identity.did(),
                        SESSION,
                    ),
                )
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after.doc_id, session.doc_id);
        assert_eq!(after.session, session.session);
        node.shutdown().await.expect("node shutdown");
        return ReplacementOutput { expected: None };
    }
    let doc_id = persist_command(&access, &command).await;
    let applied = if start.commit {
        process_command(node, &identity, &doc_id, fixture.agent.as_ref())
            .await
            .unwrap();
        let result = receipt(&access, &doc_id).await;
        if !start.first_request_valid || !start.signature_valid {
            assert!(result.is_none(), "{}", c.name);
            false
        } else {
            let result =
                result.unwrap_or_else(|| panic!("{}: expected signed control receipt", c.name));
            result.validate_for(&command).unwrap();
            assert!(identity
                .verify(
                    &result.agent_did,
                    &result.signing_payload().unwrap(),
                    &result.signature
                )
                .await
                .unwrap());
            matches!(result.outcome, GoalControlOutcome::Applied { .. })
        }
    } else {
        let txn = ConfigApplyTxn::begin_local_with_signer(
            node,
            ::identity::Did::new(&command.agent_did).unwrap(),
            fixture.agent.managed_agent_signer().unwrap(),
        )
        .await
        .unwrap();
        let mut prepared = prepare_goal_control(&txn, &command).await.unwrap().unwrap();
        attach_client_goal_graphs(&txn, &command, &mut prepared)
            .await
            .unwrap()
            .unwrap();
        stage_goal_control(&txn, identity.as_ref(), prepared)
            .await
            .unwrap();
        txn.discard().await.unwrap();
        assert!(receipt(&access, &doc_id).await.is_none());
        false
    };
    let after = fixture.current_goal().await;
    let history_response = agent_access.execute(&format!(
        r#"{{ Goal(filter: {{node_did: {{_eq: "{}"}}, goal_id: {{_eq: "{}"}}}}, showDeleted: true, order: [{{created_at: ASC}}, {{goal_id: ASC}}]) {{{GOAL_FIELDS}}} }}"#,
        escape_graphql_string(identity.did()), escape_graphql_string(&original.goal_id),
    )).await.unwrap();
    let history = history_response["data"]["Goal"]
        .as_array()
        .unwrap()
        .first()
        .cloned()
        .map(|row| serde_json::from_value::<GoalDocument>(row).unwrap());
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
            c.name, original.goal_id
        );
    }
    let history = history.unwrap();
    assert_eq!(
        serde_json::to_value(&history).unwrap(),
        serde_json::to_value(&original).unwrap(),
        "{} prior accounting/history",
        c.name
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
    let expected = if applied {
        let deleted = access
            .execute(&format!(
                r#"{{ Goal(docID: "{}", showDeleted: true) {{ _version {{ cid }} }} }}"#,
                escape_graphql_string(&original.doc_id),
            ))
            .await
            .unwrap();
        let cid = deleted["data"]["Goal"][0]["_version"][0]["cid"]
            .as_str()
            .unwrap();
        let (block_bytes, signature_bytes) = node
            .authorized_signed_block_bytes(cid, Some(fixture.user.did()))
            .await
            .unwrap();
        let block = defra_core::block::Block::from_dag_cbor(&block_bytes).unwrap();
        let signature = defra_core::block::Signature::from_dag_cbor(&signature_bytes).unwrap();
        let evidence = command
            .native_evidence
            .as_ref()
            .unwrap()
            .replacement_delete
            .as_ref()
            .unwrap();
        assert_eq!(block.generate_cid().unwrap().to_string(), cid);
        assert_eq!(block.to_dag_cbor().unwrap(), block_bytes);
        assert_eq!(signature.to_dag_cbor().unwrap(), signature_bytes);
        let mut unsigned = block.clone();
        assert_eq!(
            unsigned.signature.take(),
            Some(signature.generate_cid().unwrap())
        );
        assert_eq!(
            unsigned.to_dag_cbor().unwrap(),
            serde_json::from_value::<Vec<u8>>(evidence["composite_block"].clone()).unwrap()
        );
        assert_eq!(
            signature.value,
            serde_json::from_value::<Vec<u8>>(evidence["composite_signature"].clone()).unwrap()
        );
        assert_eq!(
            db::block::verify::verified_signature_signer_did(&block, &signature).unwrap(),
            fixture.user.did()
        );
        assert_eq!(after.goal_id, fresh_identity.goal_id);
        assert_ne!(after.doc_id, original.doc_id);
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
        let count = |value: Option<i64>| {
            u64::try_from(value.expect("a replacement records its counters")).unwrap()
        };
        Some(ReplacedWire {
            status: after.status.clone(),
            objective: after.objective.clone(),
            tokens_used: count(after.tokens_used),
            token_budget: after
                .token_budget
                .map(|budget| u64::try_from(budget).unwrap()),
            sequence: u64::try_from(after.continuation_sequence()).unwrap(),
            usage_baseline: count(after.token_usage_baseline),
            active_seconds: count(after.active_time_seconds),
        })
    } else {
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&original).unwrap(),
            "{} refusal/rollback",
            c.name
        );
        assert_eq!(claims_after, claims_before);
        let requests = access
            .execute("{AgentRequest(filter: {request_id: {_eq: \"replacement-first\"}}) {_docID}}")
            .await
            .unwrap();
        assert!(
            requests["data"]["AgentRequest"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{} must not publish a replacement request",
            c.name
        );
        None
    };
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
            before.snapshot.execution.tokens_used
        );
    }
    let session_after = access
        .transact_readonly("test.replacement_session_after", |txn| {
            Box::pin(async {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn,
                    identity.did(),
                    SESSION,
                )
                .await
            })
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.session, session_after.session);
    node.shutdown().await.expect("node shutdown");
    ReplacementOutput { expected }
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
