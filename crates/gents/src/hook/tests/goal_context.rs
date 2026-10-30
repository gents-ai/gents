use super::*;
use crate::config_client::ConfigAccess;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use serde_json::Value;
use std::collections::BTreeMap;

fn goal_id(owner: &str, session: &str, id: u64) -> String {
    if id == 1 {
        crate::goal::deterministic_goal_id(owner, session)
    } else {
        crate::goal::replacement_goal_identity(owner, session, &format!("assignment-{id}")).goal_id
    }
}

async fn request(
    access: &ConfigAccess,
    identity: &KeyIdentity,
    session: &str,
    id: u64,
    reference: Option<GoalAssignmentInput>,
    signature_valid: bool,
) -> String {
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        format!("goal-tool-{id}"),
        identity.did(),
        identity.did(),
        "general",
        session,
        "Work on the caller assignment",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = reference;
    crate::sign_agent_request_create(identity, &mut request)
        .await
        .unwrap();
    if !signature_valid {
        request.content.push_str(" changed after signing");
    }
    let response = access
        .write(
            "test.goal_tool_signed_request",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    crate::graphql::created_doc_id(&response, "AgentRequest").unwrap()
}

pub(super) async fn bind_signed_request(
    node: &Arc<EmbeddedNode>,
    hook: &DefraSessionHook,
    identity: &KeyIdentity,
    request_id: &str,
    session: &str,
    reference: Option<GoalAssignmentInput>,
) {
    let access = ConfigAccess::local(node.clone());
    let mut request = AgentRequestCreate::base(
        RequestPurpose::Normal,
        request_id,
        identity.did(),
        identity.did(),
        "general",
        session,
        "Goal control request",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = reference;
    crate::sign_agent_request_create(identity, &mut request)
        .await
        .unwrap();
    let response = access
        .write(
            "test.goal_tool_bound_request",
            &request.graphql_mutation().unwrap(),
        )
        .await
        .unwrap();
    let doc = crate::graphql::created_doc_id(&response, "AgentRequest").unwrap();
    access
        .transact_write("test.goal_tool_bind_existing_goal", |txn| {
            Box::pin(async {
                if let Some(goal) =
                    crate::goal::load_canonical_goal_in_txn(txn, identity.did(), session).await?
                {
                    if goal.assignment_root_request_doc_id.is_none() {
                        crate::goal::bind_goal_first_request_in_txn(
                            txn,
                            &goal,
                            &doc,
                            request.input.goal_assignment.as_ref(),
                        )
                        .await?;
                    }
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
    access.write("test.goal_tool_finish_previous_requests", &format!(r#"mutation {{update_AgentRequest(filter: {{_docID: {{_neq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}}}}"#,
        crate::graphql::escape_graphql_string(&doc))).await.unwrap();
    claim_signed_request(node, hook, request_id, session).await;
}

async fn claim_signed_request(
    node: &Arc<EmbeddedNode>,
    hook: &DefraSessionHook,
    request_id: &str,
    session: &str,
) {
    claim_request_fixture(node, hook, request_id, session, true).await;
}

async fn claim_request_fixture(
    node: &Arc<EmbeddedNode>,
    hook: &DefraSessionHook,
    request_id: &str,
    session: &str,
    signature_valid: bool,
) {
    let access = ConfigAccess::local(node.clone());
    let response = access.execute(&format!(r#"{{AgentRequest(filter: {{request_id: {{_eq: "{}"}}, session_id: {{_eq: "{}"}}}}) {{{}}}}}"#,
        crate::graphql::escape_graphql_string(request_id), crate::graphql::escape_graphql_string(session), crate::request_admission::SIGNED_REQUEST_FIELDS)).await.unwrap();
    let row: gents_protocol::row::AgentRequestRow =
        serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
    assert_eq!(
        crate::verify_request_receipt_signature(&row).is_ok(),
        signature_valid
    );
    let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_node_did(
        node.clone(),
        "general",
        identity_owner(hook),
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(
        lifecycle.claim().await.unwrap(),
        crate::lifecycle::ClaimOutcome::Claimed
    );
    let writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        identity_owner(hook),
        std::time::Duration::ZERO,
    );
    lifecycle.begin_owned_execution(&writer).await.unwrap();
    hook.set_active_request_lineage(Some(request_id.into()), Some(identity_owner(hook).into()))
        .await
        .unwrap();
    hook.set_request_deadline_at(Some(Utc::now() + chrono::Duration::minutes(5)))
        .await;
    hook_execution_fixtures().lock().await.insert(
        hook_execution_fixture_key(hook, request_id),
        HookExecutionFixture {
            lifecycle,
            writer,
            turn: 0,
        },
    );
}

fn identity_owner(hook: &DefraSessionHook) -> &str {
    &hook.node_did
}

async fn versions(access: &ConfigAccess) -> Value {
    let mut rows = access
        .execute("{Goal(showDeleted: true) {_docID _version {cid}}}")
        .await
        .unwrap()["data"]["Goal"]
        .as_array()
        .unwrap()
        .clone();
    rows.sort_by(|left, right| left["_docID"].as_str().cmp(&right["_docID"].as_str()));
    json!(rows)
}

#[tokio::test]
async fn generated_goal_tool_context_uses_the_signed_callers_assignment() {
    let contract = gents_lean_contract::load_contract_snapshot::<Value>().unwrap();
    let cases = contract["goal_tool_context_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 11);
    for case in cases {
        for tool in [
            crate::goal::GET_GOAL_TOOL_NAME,
            crate::goal::UPDATE_GOAL_TOOL_NAME,
            crate::goal::CREATE_GOAL_TOOL_NAME,
        ] {
            let name = case["name"].as_str().unwrap();
            let directory = tempfile::tempdir().unwrap();
            let identity =
                KeyIdentity::load_or_create(directory.path().join("owner.key"), None).unwrap();
            let node = Arc::new(
                EmbeddedNode::builder()
                    .with_node_identity_did(identity.did())
                    .build()
                    .await
                    .unwrap(),
            );
            crate::ensure_runtime_schemas(&node).await.unwrap();
            let access = ConfigAccess::local(node.clone());
            let hook = DefraSessionHook::with_identity(
                node.clone(),
                identity.did(),
                FailurePolicy::FailClosed,
            )
            .with_goal_tool_authority(true, true);
            assert!(matches!(
                hook.on_completion_call(&user_text_message("Work on this goal"), &[])
                    .await,
                HookAction::Continue
            ));
            let session = hook.session_id().await.unwrap();
            let entry = &case["entry"];
            let entry_id = entry["doc"].as_u64().unwrap();
            let mut root_goals = BTreeMap::new();
            for version in case["versions"].as_array().unwrap() {
                if let Some(root) = version["assignment"]["root"].as_u64() {
                    root_goals
                        .entry(root)
                        .or_insert(version["assignment"]["goal"].as_u64().unwrap());
                }
            }
            if let Some(root) = entry["reference"]["root"].as_u64() {
                root_goals
                    .entry(root)
                    .or_insert(entry["reference"]["goal"].as_u64().unwrap());
            }
            let mut roots = BTreeMap::new();
            for (root, goal) in root_goals {
                let reference = if root == entry_id && entry["reference"].is_null() {
                    None
                } else {
                    Some(GoalAssignmentInput {
                        goal_id: goal_id(identity.did(), &session, goal),
                        root_request_doc_id: None,
                    })
                };
                roots.insert(
                    root,
                    request(&access, &identity, &session, root, reference, true).await,
                );
            }
            let caller = if let Some(doc) = roots.get(&entry_id) {
                doc.clone()
            } else {
                let reference = entry["reference"].as_object().map(|_| GoalAssignmentInput {
                    goal_id: goal_id(
                        identity.did(),
                        &session,
                        entry["reference"]["goal"].as_u64().unwrap(),
                    ),
                    root_request_doc_id: entry["reference"]["root"]
                        .as_u64()
                        .map(|root| roots[&root].clone()),
                });
                request(
                    &access,
                    &identity,
                    &session,
                    entry_id,
                    reference,
                    entry["signature_valid"].as_bool().unwrap(),
                )
                .await
            };
            let mut documents = BTreeMap::<u64, String>::new();
            for version in case["versions"].as_array().unwrap() {
                let id = version["document"].as_u64().unwrap();
                let assignment = &version["assignment"];
                let goal_identity = assignment["goal"].as_u64().unwrap();
                let creation_key = if goal_identity == 1 {
                    crate::goal::deterministic_goal_creation_key(identity.did(), &session)
                } else {
                    crate::goal::replacement_goal_identity(
                        identity.did(),
                        &session,
                        &format!("assignment-{goal_identity}"),
                    )
                    .creation_key
                };
                let input = json!({"goal_id":goal_id(identity.did(), &session, goal_identity), "creation_key": creation_key,
                    "node_did":identity.did(), "session_id":session, "objective":"Original caller goal",
                    "status":version["status"], "assignment_root_request_doc_id":assignment["root"].as_u64().map(|root| &roots[&root]),
                    "tokens_used":317, "token_usage_baseline":0, "active_time_seconds":23,
                    "continuation_sequence":0, "updated_at":"2026-10-21T00:00:00Z"});
                let doc = documents.get(&id).cloned();
                let created = access.transact_write("test.goal_tool_history", |txn| {
                    let doc=doc.clone();let input=input.clone();
                    Box::pin(async move {
                        if let Some(doc)=doc {
                            txn.execute_with_variables("mutation($doc: ID!, $input: GoalMutationInputArg!) {update_Goal(docID: $doc, input: $input) {_docID}}", &json!({"doc":doc,"input":input})).await?;
                            Ok(doc)
                        } else {
                            let response=txn.execute_with_variables("mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",&json!({"input":input})).await?;
                            crate::graphql::created_doc_id(&response,"Goal")
                        }
                    })
                }).await.unwrap();
                documents.insert(id, created);
            }
            let mut claims = BTreeMap::new();
            for version in case["versions"].as_array().unwrap() {
                claims.insert(version["assignment"]["goal"].as_u64().unwrap(), ());
            }
            for (id, ()) in claims {
                let creation_key = if id == 1 {
                    crate::goal::deterministic_goal_creation_key(identity.did(), &session)
                } else {
                    crate::goal::replacement_goal_identity(
                        identity.did(),
                        &session,
                        &format!("assignment-{id}"),
                    )
                    .creation_key
                };
                access.write("test.goal_tool_original_claim", &format!(r#"mutation {{create_GoalCreationClaim(input: {{creation_key: "{}", goal_id: "{}", node_did: "{}", session_id: "{}", objective: "Original caller goal", created_at: "2026-10-21T00:00:00Z"}}) {{_docID}}}}"#,
                    crate::graphql::escape_graphql_string(&creation_key),
                    crate::graphql::escape_graphql_string(&goal_id(identity.did(), &session, id)),
                    crate::graphql::escape_graphql_string(identity.did()),crate::graphql::escape_graphql_string(&session))).await.unwrap();
            }
            for id in case["retire_documents"].as_array().unwrap() {
                access
                    .write(
                        "test.goal_tool_retire",
                        &format!(
                            r#"mutation {{delete_Goal(docID: "{}") {{_docID}}}}"#,
                            crate::graphql::escape_graphql_string(
                                &documents[&id.as_u64().unwrap()]
                            )
                        ),
                    )
                    .await
                    .unwrap();
            }
            access.write("test.goal_tool_terminal_roots", &format!(r#"mutation {{update_AgentRequest(filter: {{_docID: {{_neq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}}}}"#,
                crate::graphql::escape_graphql_string(&caller))).await.unwrap();
            let request_id = format!("goal-tool-{entry_id}");
            claim_request_fixture(
                &node,
                &hook,
                &request_id,
                &session,
                entry["signature_valid"].as_bool().unwrap(),
            )
            .await;
            let args = match tool {
                crate::goal::GET_GOAL_TOOL_NAME => "{}",
                crate::goal::UPDATE_GOAL_TOOL_NAME => r#"{"objective":"Changed caller goal"}"#,
                _ => r#"{"objective":"Original caller goal"}"#,
            };
            accept_hook_tool_call(&hook, "goal-context-call", tool, args, None).await;
            let before = versions(&access).await;
            if tool == crate::goal::GET_GOAL_TOOL_NAME && case["read_only"] == true {
                let context = crate::goal::load_goal_tool_context(
                    &node,
                    identity.did(),
                    &session,
                    &request_id,
                    Some(&caller),
                )
                .await
                .unwrap();
                let crate::goal::RequestAssignmentObservation::Assigned(observed) =
                    context.assignment
                else {
                    panic!("{name}: retained assignment unavailable");
                };
                assert!(
                    crate::goal::edit_active_goal(
                        &node,
                        &observed,
                        &crate::goal::GoalEdit {
                            objective: Some("Stale edit".into()),
                            budget: None
                        }
                    )
                    .await
                    .unwrap()
                    .is_none(),
                    "{name}: edit crossed physical root"
                );
                assert!(
                    !crate::goal::update_goal_fields_if_status(
                        &node,
                        &observed,
                        observed.parsed_status().unwrap(),
                        r#"last_failure: "Stale controller write""#
                    )
                    .await
                    .unwrap(),
                    "{name}: controller crossed physical root"
                );
                crate::goal::refresh_goal_usage(&node, &observed)
                    .await
                    .unwrap();
                assert_eq!(
                    versions(&access).await,
                    before,
                    "{name}: retired owner mutation changed Goal history"
                );
            }
            let action = hook
                .on_tool_call(tool, None, "goal-context-call", args)
                .await;
            let terminated = matches!(&action, ToolCallHookAction::Terminate { .. });
            let reason = match action {
                ToolCallHookAction::Skip { reason } | ToolCallHookAction::Terminate { reason } => {
                    reason
                }
                action => panic!("{name}/{tool}: control escaped its accepted hook: {action:?}"),
            };
            let result: Value = serde_json::from_str(&reason).unwrap();
            if case["observation"]["kind"] == "unknown" {
                assert!(
                    terminated,
                    "unavailable attribution must stop the current provider loop"
                );
                let scope = crate::graphql::escape_graphql_string(&caller);
                let observed = access.execute(&format!(r#"{{AgentToolCall(filter: {{request_doc_id: {{_eq: "{scope}"}}}}) {{lifecycle_state tool_failure_class}}}}"#)).await.unwrap();
                let rows = observed["data"]["AgentToolCall"].as_array().unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0]["lifecycle_state"], "failed");
                assert_eq!(rows[0]["tool_failure_class"], "serviceUnavailable");
            }
            if tool == crate::goal::GET_GOAL_TOOL_NAME {
                match case["observation"]["kind"].as_str().unwrap() {
                    "unknown" => {
                        assert!(result["error"].is_string(), "{name}/{tool}: {result}");
                    }
                    "unassigned" => {
                        assert!(result["goal"].is_null(), "{name}/{tool}: {result}");
                        assert!(!result["error"].is_string(), "{name}/{tool}: {result}");
                    }
                    _ => {
                        let expected = case["observation"]["identity"]["goal"].as_u64().unwrap();
                        assert_eq!(
                            result["goal_id"],
                            goal_id(identity.did(), &session, expected),
                            "{name}/{tool}: {result}"
                        );
                        assert_eq!(
                            result["read_only"].as_bool().unwrap_or(false),
                            case["read_only"].as_bool().unwrap(),
                            "{name}/{tool}: {result}"
                        );
                        if case["read_only"] == true {
                            assert!(
                                result["assignment_context"].is_string(),
                                "{name}/{tool}: {result}"
                            );
                            assert_eq!(
                                result["tokens_used"], 317,
                                "{name}/{tool}: retained usage changed"
                            );
                            assert_eq!(
                                result["active_time_seconds"], 23,
                                "{name}/{tool}: retained clock advanced"
                            );
                        }
                    }
                }
                if case["can_update"] != true {
                    assert_eq!(
                        versions(&access).await,
                        before,
                        "{name}/{tool}: read changed Goal history"
                    );
                }
            } else {
                let allowed = case[if tool == crate::goal::CREATE_GOAL_TOOL_NAME {
                    "can_create"
                } else {
                    "can_update"
                }]
                .as_bool()
                .unwrap();
                assert_eq!(
                    result["accepted"].as_bool().unwrap_or(false),
                    allowed,
                    "{name}/{tool}: {result}"
                );
                if !allowed {
                    assert_eq!(
                        versions(&access).await,
                        before,
                        "{name}/{tool}: refused control wrote Goal history"
                    );
                }
                if allowed && tool == crate::goal::CREATE_GOAL_TOOL_NAME {
                    let goal = crate::goal::load_canonical_goal(&node, identity.did(), &session)
                        .await
                        .unwrap()
                        .unwrap();
                    let expected_root = case["observation"]["identity"]["root"]
                        .as_u64()
                        .map(|root| roots[&root].clone())
                        .unwrap_or(caller.clone());
                    assert_eq!(
                        goal.assignment_root_request_doc_id.as_deref(),
                        Some(expected_root.as_str()),
                        "{name}/{tool}: creation lost its physical caller"
                    );
                }
            }
            hook_execution_fixtures()
                .lock()
                .await
                .remove(&hook_execution_fixture_key(&hook, &request_id));
            node.shutdown().await;
        }
    }
}
