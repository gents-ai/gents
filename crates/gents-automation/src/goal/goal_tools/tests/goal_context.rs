use super::*;
use gents_lean_vocab::families::goal_tool_context_contracts_context as wire;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use serde_json::Value;
use std::collections::BTreeMap;

fn goal_id(owner: &str, session: &str, id: u64) -> String {
    if id == 1 {
        gents_protocol::goal::deterministic_goal_id(owner, session)
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
        identity.did(),
        session,
        "Work on the caller assignment",
        "interactive",
        "2026-10-21T00:00:00Z",
        AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    request.input.goal_assignment = reference;
    gents_session::request::admission::sign_agent_request_create(identity, &mut request)
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
    gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap()
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

#[tokio::test(flavor = "multi_thread")]
async fn generated_goal_tool_context_uses_the_signed_callers_assignment() {
    crate::test_support::drive_cases(wire::cases::<String>(), drive_goal_tool_context).await;
}

async fn drive_goal_tool_context(modeled: <wire::Cases<String> as IntoIterator>::Item) {
    let case = &modeled.input;
    let mut observation = None;
    let mut read_only = None;
    let mut can_create = None;
    let mut can_update = None;
    for tool in [
        gents_protocol::goal_names::GET_GOAL_TOOL_NAME,
        gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME,
        gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME,
    ] {
        let name = case.name.as_str();
        let db = gents_inference::test_support::test_db(
            "goal-tool-context",
            crate::lifecycle_hooks::composition(),
        )
        .await;
        let node = db.node.clone();
        let identity = gents_store::test_support::held_runtime_identity(db.node_identity.did());
        let access = ConfigAccess::local(node.clone());
        let hook = DefraSessionHook::with_identity(
            node.clone(),
            identity.did(),
            FailurePolicy::FailClosed,
        )
        .with_goal_tool_authority(true, true)
        .with_goal_tools(Arc::new(crate::goal::goal_tools::GoalToolAnswers));
        assert!(matches!(
            hook.on_completion_call(&user_text_message("Work on this goal"), &[])
                .await,
            HookAction::Continue
        ));
        let session = hook.session_id().await.unwrap();
        // In this fixture the runtime identity is also the session's working
        // Agent; Goal history is written and retired as that creator.
        let agent_access = ensure_runtime_session(&node, &identity, &session)
            .await
            .unwrap();
        let session_doc_id = agent_access
            .execute(&format!(
                r#"{{AgentSession(filter: {{session_id: {{_eq: "{}"}}}}) {{_docID}}}}"#,
                gents_store::graphql::escape_graphql_string(&session)
            ))
            .await
            .unwrap()["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let entry = &case.entry;
        let entry_id = entry.doc;
        let mut root_goals = BTreeMap::new();
        for version in &case.versions {
            if let Some(root) = version.assignment.root {
                root_goals.entry(root).or_insert(version.assignment.goal);
            }
        }
        if let Some(root) = entry
            .reference
            .as_ref()
            .and_then(|reference| reference.root)
        {
            root_goals
                .entry(root)
                .or_insert(entry.reference.as_ref().unwrap().goal);
        }
        let mut roots = BTreeMap::new();
        for (root, goal) in root_goals {
            let reference = if root == entry_id && entry.reference.is_none() {
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
            let reference = entry
                .reference
                .as_ref()
                .map(|reference| GoalAssignmentInput {
                    goal_id: goal_id(identity.did(), &session, reference.goal),
                    root_request_doc_id: reference.root.map(|root| roots[&root].clone()),
                });
            request(
                &access,
                &identity,
                &session,
                entry_id,
                reference,
                entry.signature_valid,
            )
            .await
        };
        let mut documents = BTreeMap::<u64, String>::new();
        for version in &case.versions {
            let id = version.document;
            let assignment = &version.assignment;
            let goal_identity = assignment.goal;
            let creation_key = if goal_identity == 1 {
                gents_protocol::goal::deterministic_goal_creation_key(identity.did(), &session)
            } else {
                crate::goal::replacement_goal_identity(
                    identity.did(),
                    &session,
                    &format!("assignment-{goal_identity}"),
                )
                .creation_key
            };
            let input = json!({"goal_id":goal_id(identity.did(), &session, goal_identity), "creation_key": creation_key,
                "node_did":identity.did(), "session_id":session, "session_doc_id":session_doc_id,
                "agent_did":identity.did(), "creator_did":identity.did(), "requester_did":identity.did(),
                "parent_linked":true, "objective":"Original caller goal",
                "status":version.status.as_str(), "assignment_root_request_doc_id":assignment.root.map(|root| &roots[&root]),
                "tokens_used":317, "token_usage_baseline":0, "active_time_seconds":23,
                "continuation_sequence":0, "updated_at":"2026-10-21T00:00:00Z"});
            let doc = documents.get(&id).cloned();
            let created = agent_access.transact_write("test.goal_tool_history", |txn| {
                let doc=doc.clone();let input=input.clone();
                Box::pin(async move {
                    if let Some(doc)=doc {
                        txn.execute_with_variables("mutation($doc: ID!, $input: GoalMutationInputArg!) {update_Goal(docID: $doc, input: $input) {_docID}}", &json!({"doc":doc,"input":input})).await?;
                        Ok(doc)
                    } else {
                        let response=txn.execute_with_variables("mutation($input: GoalMutationInputArg!) {create_Goal(input: $input) {_docID}}",&json!({"input":input})).await?;
                        gents_store::graphql::created_doc_id(&response,"Goal")
                    }
                })
            }).await.unwrap();
            documents.insert(id, created);
        }
        let mut claims = BTreeMap::new();
        for version in &case.versions {
            claims.insert(version.assignment.goal, ());
        }
        for (id, ()) in claims {
            let creation_key = if id == 1 {
                gents_protocol::goal::deterministic_goal_creation_key(identity.did(), &session)
            } else {
                crate::goal::replacement_goal_identity(
                    identity.did(),
                    &session,
                    &format!("assignment-{id}"),
                )
                .creation_key
            };
            access.write("test.goal_tool_original_claim", &format!(r#"mutation {{create_GoalCreationClaim(input: {{creation_key: "{}", goal_id: "{}", node_did: "{}", session_id: "{}", objective: "Original caller goal", created_at: "2026-10-21T00:00:00Z"}}) {{_docID}}}}"#,
                gents_store::graphql::escape_graphql_string(&creation_key),
                gents_store::graphql::escape_graphql_string(&goal_id(identity.did(), &session, id)),
                gents_store::graphql::escape_graphql_string(identity.did()),gents_store::graphql::escape_graphql_string(&session))).await.unwrap();
        }
        for id in &case.retire_documents {
            agent_access
                .write(
                    "test.goal_tool_retire",
                    &format!(
                        r#"mutation {{delete_Goal(docID: "{}") {{_docID}}}}"#,
                        gents_store::graphql::escape_graphql_string(&documents[id])
                    ),
                )
                .await
                .unwrap();
        }
        access.write("test.goal_tool_terminal_roots", &format!(r#"mutation {{update_AgentRequest(filter: {{_docID: {{_neq: "{}"}}}}, input: {{lifecycle_state: "completed"}}) {{_docID}}}}"#,
            gents_store::graphql::escape_graphql_string(&caller))).await.unwrap();
        let request_id = format!("goal-tool-{entry_id}");
        claim_request_fixture(&node, &hook, &request_id, &session, entry.signature_valid).await;
        let args = match tool {
            gents_protocol::goal_names::GET_GOAL_TOOL_NAME => "{}",
            gents_protocol::goal_names::UPDATE_GOAL_TOOL_NAME => {
                r#"{"objective":"Changed caller goal"}"#
            }
            _ => r#"{"objective":"Original caller goal"}"#,
        };
        accept_hook_tool_call(&hook, "goal-context-call", tool, args, None).await;
        let before = versions(&access).await;
        if tool == gents_protocol::goal_names::GET_GOAL_TOOL_NAME {
            let context = crate::goal::load_goal_tool_context(
                &access,
                identity.did(),
                &session,
                &request_id,
                Some(&caller),
            )
            .await
            .map(|context| context.assignment)
            .unwrap_or(crate::goal::RequestAssignmentObservation::Unknown);
            let number = |doc: &str, map: &BTreeMap<u64, String>| {
                *map.iter().find(|(_, held)| held.as_str() == doc).unwrap().0
            };
            observation = Some(match &context {
                crate::goal::RequestAssignmentObservation::None => wire::ObservationWire {
                    kind: "unassigned".into(),
                    identity: None,
                },
                crate::goal::RequestAssignmentObservation::Unknown => wire::ObservationWire {
                    kind: "unknown".into(),
                    identity: None,
                },
                crate::goal::RequestAssignmentObservation::Assigned(goal) => {
                    wire::ObservationWire {
                        kind: "assigned".into(),
                        identity: Some(wire::Identity {
                            document: number(&goal.doc_id, &documents),
                            goal: case
                                .versions
                                .iter()
                                .map(|version| version.assignment.goal)
                                .find(|id| goal_id(identity.did(), &session, *id) == goal.goal_id)
                                .unwrap(),
                            root: number(
                                goal.assignment_root_request_doc_id.as_deref().unwrap(),
                                &roots,
                            ),
                        }),
                    }
                }
            });
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
        let observed = observation.as_ref().unwrap();
        if tool == gents_protocol::goal_names::GET_GOAL_TOOL_NAME
            && result["read_only"].as_bool().unwrap_or(false)
        {
            let context = crate::goal::load_goal_tool_context(
                &access,
                identity.did(),
                &session,
                &request_id,
                Some(&caller),
            )
            .await
            .unwrap();
            let crate::goal::RequestAssignmentObservation::Assigned(observed) = context.assignment
            else {
                panic!("{name}: retained assignment unavailable");
            };
            let working_agent = gents_session::session::load_session_agent_did(
                &node,
                &observed.node_did,
                &observed.session_id,
            )
            .await
            .unwrap();
            assert!(
                crate::operations::goal::set_as_working_agent(
                    &gents_store::config_client::ConfigAccess::local_as(
                        node.clone(),
                        working_agent
                    ),
                    &observed,
                    &gents_protocol::operations::goal::GoalSetInput {
                        objective: Some("Stale edit".into()),
                        ..Default::default()
                    },
                    None,
                    &request_id,
                )
                .await
                .is_err(),
                "{name}: edit crossed physical root"
            );
            assert!(
                !crate::goal::update_goal_fields_if_status_from_access(
                    &gents_store::config_client::ConfigAccess::local_as(
                        node.clone(),
                        identity::Did::new(identity.did()).unwrap()
                    ),
                    &observed,
                    observed.parsed_status().unwrap(),
                    r#"last_failure: "Stale controller write""#
                )
                .await
                .unwrap(),
                "{name}: controller crossed physical root"
            );
            crate::goal::refresh_goal_usage_from_access(
                &gents_store::config_client::ConfigAccess::local_as(
                    node.clone(),
                    identity::Did::new(identity.did()).unwrap(),
                ),
                &observed,
            )
            .await
            .unwrap();
            assert_eq!(
                versions(&access).await,
                before,
                "{name}: retired owner mutation changed Goal history"
            );
        }
        if observed.kind == "unknown" {
            assert!(
                terminated,
                "unavailable attribution must stop the current provider loop"
            );
            let scope = gents_store::graphql::escape_graphql_string(&caller);
            let observed = access.execute(&format!(r#"{{AgentToolCall(filter: {{request_doc_id: {{_eq: "{scope}"}}}}) {{lifecycle_state tool_failure_class}}}}"#)).await.unwrap();
            let rows = observed["data"]["AgentToolCall"].as_array().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["lifecycle_state"], "failed");
            assert_eq!(rows[0]["tool_failure_class"], "serviceUnavailable");
        }
        if tool == gents_protocol::goal_names::GET_GOAL_TOOL_NAME {
            match observed.kind.as_str() {
                "unknown" => {
                    assert!(result["error"].is_string(), "{name}/{tool}: {result}");
                }
                "unassigned" => {
                    assert!(result["goal"].is_null(), "{name}/{tool}: {result}");
                    assert!(!result["error"].is_string(), "{name}/{tool}: {result}");
                }
                _ => {
                    let observed_goal = observed.identity.as_ref().unwrap().goal;
                    assert_eq!(
                        result["goal_id"],
                        goal_id(identity.did(), &session, observed_goal),
                        "{name}/{tool}: {result}"
                    );
                    if result["read_only"].as_bool().unwrap_or(false) {
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
            read_only = Some(result["read_only"].as_bool().unwrap_or(false));
            if read_only == Some(true) || observed.kind != "assigned" {
                assert_eq!(
                    versions(&access).await,
                    before,
                    "{name}/{tool}: read changed Goal history"
                );
            }
        } else {
            let allowed = result["accepted"].as_bool().unwrap_or(false);
            if tool == gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME {
                can_create = Some(allowed);
            } else {
                can_update = Some(allowed);
            }
            if !allowed {
                assert_eq!(
                    versions(&access).await,
                    before,
                    "{name}/{tool}: refused control wrote Goal history"
                );
            }
            if allowed && tool == gents_protocol::goal_names::CREATE_GOAL_TOOL_NAME {
                let goal = crate::goal::load_canonical_goal(&node, identity.did(), &session)
                    .await
                    .unwrap()
                    .unwrap();
                let expected_root = observed
                    .identity
                    .as_ref()
                    .map(|identity| identity.root)
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
        node.shutdown().await.expect("shut down test node");
    }
    modeled.check(wire::ContextOutput {
        observation: observation.unwrap(),
        read_only: read_only.unwrap(),
        can_create: can_create.unwrap(),
        can_update: can_update.unwrap(),
    });
}
