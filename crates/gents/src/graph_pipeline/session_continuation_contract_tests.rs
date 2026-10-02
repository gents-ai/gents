use super::*;
use workspace_lineage::{
    select_graph_session, session_target_eligible, SessionContinuationContext,
    SessionRootCandidate, SessionTargetEligibility, SessionTargetRoute,
};

fn number(value: &Value) -> String {
    value.as_u64().unwrap().to_string()
}
fn boolean(value: &Value) -> bool {
    value.as_bool().unwrap()
}

#[test]
fn session_selection_matches_executable_graph_owner() {
    let contract = gents_lean_contract::load_contract_snapshot::<Value>().unwrap();
    for case in contract["graph_session_continuation_cases"]
        .as_array()
        .unwrap()
    {
        let raw = &case["eligibility"];
        let eligibility = SessionTargetEligibility {
            source_is_task: boolean(&raw["source_is_task"]),
            target_exists: boolean(&raw["target_exists"]),
            target_is_task: boolean(&raw["target_is_task"]),
            route_count: raw["route_count"].as_u64().unwrap() as usize,
            route_kind: match raw["route_kind"].as_str().unwrap() {
                "selected_entry" => SessionTargetRoute::SelectedEntry,
                "grouped" => SessionTargetRoute::Grouped,
                "per_document" => SessionTargetRoute::PerDocument,
                _ => panic!("unknown generated route kind"),
            },
        };
        assert_eq!(
            session_target_eligible(&eligibility),
            boolean(&case["eligible"]),
            "{}",
            case["name"]
        );
        let raw = &case["context"];
        let context = SessionContinuationContext {
            owner: number(&case["owner"]),
            firing_node: number(&case["firing_node"]),
            target_node: number(&case["target_node"]),
            correlation: number(&raw["correlation"]),
            revision: number(&raw["revision"]),
            target_route: number(&raw["target_route"]),
            run_and_plan_verified: boolean(&raw["run_and_plan_verified"]),
            destination_route_verified: boolean(&raw["destination_route_verified"]),
        };
        let candidates = case["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|candidate| {
                let root = &candidate["root"];
                SessionRootCandidate {
                    root_doc_id: number(&root["doc_id"]),
                    session_id: number(&candidate["session"]),
                    owner: number(&candidate["owner"]),
                    correlation: number(&root["correlation"]),
                    revision: number(&root["revision"]),
                    target_route: number(&root["entry_route"]),
                    authenticated: boolean(&root["authenticated_target"]),
                }
            })
            .collect::<Vec<_>>();
        let actual = select_graph_session(&eligibility, &context, &candidates);
        if case["expected"].is_null() {
            assert!(actual.is_none(), "{}", case["name"]);
        } else {
            let actual = actual.expect("modeled session selection accepted");
            assert_eq!(actual.session_id, number(&case["expected"]["session"]));
            assert_eq!(actual.root_doc_id, number(&case["expected"]["root_doc"]));
            assert_eq!(actual.firing_node, number(&case["expected"]["firing_node"]));
        }
    }
}

#[test]
fn applied_assignment_heads_match_executable_graph_owner() {
    let contract = gents_lean_contract::load_contract_snapshot::<Value>().unwrap();
    for case in contract["graph_assignment_head_cases"].as_array().unwrap() {
        let heads = case["heads"]
            .as_array()
            .unwrap()
            .iter()
            .map(|head| logical_invocation::AssignmentHead {
                member: boolean(&head["member"]),
                authentic_root: boolean(&head["authentic_root"]),
                assignment_applied: boolean(&head["assignment_applied"]),
                authenticated_continuation: boolean(&head["authenticated_continuation"]),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            logical_invocation::assignment_owns_goal(
                boolean(&case["root_assignment_applied"]),
                &heads
            ),
            boolean(&case["expected"]),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn compiler_preserves_entry_and_group_selection_and_rejects_fanout() {
    use crate::graph_pipeline::{
        compile_graph, CompilerPolicy, GraphIntent, GraphSessionSelection, StageCapability,
    };
    let mut authored: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/packs/review_graph/pack_config.json"
    ))
    .unwrap();
    authored["graph_intents"][0]["agent_did"] = json!("did:test:session-compiler");
    let mut intent: GraphIntent =
        serde_json::from_value(authored["graph_intents"][0].clone()).unwrap();
    let capabilities = authored["graph_capabilities"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .map(|capability| {
            capability["agent_did"] = json!("did:test:session-compiler");
            capability["allowed_callers"] = json!(["did:test:session-compiler"]);
            serde_json::from_value::<StageCapability>(capability.clone()).unwrap()
        })
        .collect::<Vec<_>>();
    let contract = gents_lean_contract::load_contract_snapshot::<Value>().unwrap();
    for (target, name) in [
        ("recon", "selected_entry_parallel_is_singleton"),
        ("verify", "unique_group_parallel_is_singleton"),
        ("scan", "per_document_serial_is_not_singleton"),
    ] {
        intent
            .nodes
            .iter_mut()
            .find(|node| node.node_id == "triage")
            .unwrap()
            .session = Some(GraphSessionSelection {
            continue_node_id: target.into(),
        });
        let expected = contract["graph_session_continuation_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == name)
            .unwrap()["eligible"]
            .as_bool()
            .unwrap();
        let result = compile_graph(
            &intent,
            &capabilities,
            "did:test:session-compiler",
            &CompilerPolicy::default(),
        );
        assert_eq!(result.is_ok(), expected, "{target}: {result:?}");
        if let Ok(plan) = result {
            assert_eq!(
                plan.nodes
                    .iter()
                    .find(|node| node.node_id == "triage")
                    .unwrap()
                    .session
                    .as_ref()
                    .unwrap()
                    .continue_node_id,
                target
            );
        }
    }
}

#[tokio::test]
async fn selector_authentication_uses_real_signed_graph_receipts() {
    use crate::identity::AgentIdentity;
    let (node, run, _goal, identity, _temp) =
        super::super::logical_invocation_contract_tests::signed_invocation_fixture(5).await;
    let response = crate::graphql::graphql_with_transaction_retry(
        &node,
        &format!(
            "{{ AgentRequest {{ {} }} }}",
            crate::request_admission::SIGNED_REQUEST_FIELDS
        ),
        "load signed continuation root",
    )
    .await
    .unwrap();
    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest").unwrap();
    let root = &rows[0];
    let route = root.caused_by_trigger_id.as_deref().unwrap();
    let context = SessionContinuationContext {
        owner: identity.did().into(),
        firing_node: "followup".into(),
        target_node: "entry".into(),
        correlation: run.correlation.clone(),
        revision: run.revision_digest.clone(),
        target_route: route.into(),
        run_and_plan_verified: true,
        destination_route_verified: true,
    };
    let candidate = |row: &AgentRequestRow| SessionRootCandidate {
        root_doc_id: row.doc_id.clone().unwrap(),
        session_id: row.session_id.clone().unwrap(),
        owner: row.agent_did.clone().unwrap(),
        correlation: row.caused_by_correlation.clone().unwrap(),
        revision: context.revision.clone(),
        target_route: row.caused_by_trigger_id.clone().unwrap(),
        authenticated: logical_invocation::authentic_root(row, identity.did()),
    };
    let eligibility = SessionTargetEligibility {
        source_is_task: true,
        target_exists: true,
        target_is_task: true,
        route_count: 1,
        route_kind: SessionTargetRoute::SelectedEntry,
    };
    assert!(candidate(root).authenticated);
    let mut forged = root.clone();
    forged.session_id = Some("forged-session".into());
    assert!(!candidate(&forged).authenticated);
    let selected = select_graph_session(
        &eligibility,
        &context,
        &[candidate(root), candidate(&forged)],
    )
    .unwrap();
    assert_eq!(selected.session_id, root.session_id.as_deref().unwrap());
    assert_eq!(selected.firing_node, "followup");
    assert!(select_graph_session(&eligibility, &context, &[candidate(&forged)]).is_none());
}

async fn admit_session_graph_task(
    node: &Arc<EmbeddedNode>,
    plan: &GraphPlan,
    run: &super::super::GraphRunReceipt,
    identity: &crate::identity::KeyIdentity,
    graph_node: &str,
    collection: &str,
    source_doc_id: &str,
    session: Option<&str>,
) -> crate::lifecycle::EnqueuedAgentRequest {
    use crate::identity::AgentIdentity;
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    let route = planned_trigger_nodes(plan)
        .unwrap()
        .into_iter()
        .find(|(_, node)| node == graph_node)
        .unwrap()
        .0;
    let fire_identity = gents_protocol::trigger_delivery::FireIdentity {
        owner_did: identity.did().into(),
        trigger_id: route.clone(),
        source_collection: collection.into(),
        source_doc_id: source_doc_id.into(),
    };
    let fire_key = crate::lifecycle::task_fire_key(&fire_identity);
    let session_id = session
        .map(str::to_owned)
        .unwrap_or_else(|| fire_identity.session_id());
    let task = plan
        .nodes
        .iter()
        .find(|node| node.node_id == graph_node)
        .unwrap()
        .target
        .task_id()
        .unwrap();
    let fire = gents_protocol::trigger_delivery::TriggerFire {
        request_id: fire_identity.request_id(),
        identity: fire_identity,
        fire_key: fire_key.clone(),
        task_id: task.into(),
        session_id: session_id.clone(),
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
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    };
    let access = ConfigAccess::Local(node.clone());
    let triggers = access
        .execute(&format!(
            "{{Trigger(filter: {{trigger_id: {{_eq: \"{}\"}}}}) {{_docID}}}}",
            escape_graphql_string(&route)
        ))
        .await
        .unwrap();
    let mut create = AgentRequestCreate::base(
        RequestPurpose::Normal,
        &fire.request_id,
        identity.did(),
        identity.did(),
        "session-graph",
        &session_id,
        "run this graph stage",
        "scheduled",
        &fire.created_at,
        AgentRequestAdmissionRecord::runtime_automated_trigger(identity.did(), &route),
    );
    create.caused_by_trigger_id = Some(route);
    create.caused_by_trigger_doc_id = Some(
        triggers["data"]["Trigger"][0]["_docID"]
            .as_str()
            .unwrap()
            .into(),
    );
    create.caused_by_trigger_kind = Some("event".into());
    create.caused_by_source_doc_id = Some(source_doc_id.into());
    create.caused_by_correlation = Some(run.correlation.clone());
    crate::sign_agent_request_create(identity, &mut create)
        .await
        .unwrap();
    crate::lifecycle::write_task_delivery(&access, &fire, session.is_some(), &create)
        .await
        .unwrap()
        .request
}

#[tokio::test]
async fn native_same_behavior_graph_continues_session_with_distinct_stage_roots() {
    use crate::graph_pipeline::{compile_graph, CompilerPolicy, GraphIntent, StageCapability};
    use crate::identity::AgentIdentity;
    let identity = super::super::runtime::graph_test_identity();
    let owner = identity.did();
    let node = Arc::new(EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    for collection in [
        "ContinueInput",
        "ContinueStep",
        "ContinueSummary",
        "ContinueResult",
    ] {
        node.add_schema(&format!("type {collection} {{run_id: String @index}} "))
            .await
            .unwrap();
    }
    super::super::install_graph_test_tasks(
        &node,
        owner,
        "session-graph",
        &["recon-task", "verify-task", "triage-task"],
    )
    .await;
    let intent: GraphIntent = serde_json::from_value(json!({
        "agent_did": owner, "graph_id": "session-continuation-native",
        "nodes": [
            {"node_id":"recon","capability_id":"recon","capability_revision":"v1"},
            {"node_id":"verify","capability_id":"verify","capability_revision":"v1","session":{"continue":"recon"}},
            {"node_id":"triage","capability_id":"triage","capability_revision":"v1","session":{"continue":"recon"}}
        ],
        "edges": [
            {"from":{"node_id":"recon","port":"out"},"to":{"node_id":"verify","port":"in"},"concurrency":"serial"},
            {"from":{"node_id":"verify","port":"out"},"to":{"node_id":"triage","port":"in"},"concurrency":"serial"}
        ],
        "entries":[{"name":"input","collection":"ContinueInput","schema":"ContinueInput/v1","to":{"node_id":"recon","port":"in"}}],
        "results":[{"name":"result","from":{"node_id":"triage","port":"out"},"cardinality":{"kind":"exactly","count":1},"terminal":true}],
        "limits":{"max_nodes":3,"max_edges":2,"max_depth":3,"max_fan_out":1,"max_total_invocations":6,"max_runtime_secs":60}
    })).unwrap();
    let capabilities = [("recon","ContinueInput","ContinueStep"),("verify","ContinueStep","ContinueSummary"),("triage","ContinueSummary","ContinueResult")]
        .into_iter().map(|(name,input,output)| serde_json::from_value::<StageCapability>(json!({
            "agent_did":owner,"capability_id":name,"revision":"v1","target":{"kind":"task","task_id":format!("{name}-task")},
            "allowed_callers":[owner],
            "input_ports":[{"name":"in","collection":input,"schema":format!("{input}/v1"),"correlation_field":"run_id","cardinality":"one","required":true}],
            "output_ports":[{"name":"out","collection":output,"schema":format!("{output}/v1"),"correlation_field":"run_id","cardinality":"one"}]
        })).unwrap()).collect::<Vec<_>>();
    let plan = compile_graph(&intent, &capabilities, owner, &CompilerPolicy::default()).unwrap();
    super::super::materialize_graph_revision(&node, None, owner, &plan)
        .await
        .unwrap();
    super::super::activate_graph_revision(&node, None, owner, &plan.graph_id, &plan.digest, None)
        .await
        .unwrap();
    let run = super::super::start_graph_run(
        &node,
        None,
        owner,
        &plan.graph_id,
        None,
        "input",
        json!({}),
        super::super::EntryInputOrigin::Operator,
    )
    .await
    .unwrap();
    let first = admit_session_graph_task(
        &node,
        &plan,
        &run,
        &identity,
        "recon",
        "ContinueInput",
        &run.seed_doc_id,
        None,
    )
    .await;
    let response = crate::graphql::graphql_with_transaction_retry(
        &node,
        &format!(
            "{{AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}}) {{{}}}}}",
            escape_graphql_string(&first.doc_id),
            crate::watcher::AGENT_REQUEST_FIELDS
        ),
        "load first continuation request",
    )
    .await
    .unwrap();
    let request = crate::watcher::agent_request_from_mutation_response(&response, "AgentRequest")
        .unwrap()
        .unwrap();
    let mut lifecycle = crate::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        "session-graph",
        owner,
        request,
        60,
        crate::lifecycle::ExecutionOrigin::Scheduled,
        "test-backend",
    );
    let now = chrono::Utc::now();
    assert!(lifecycle
        .claim_pending_durable_with_inputs(|| now, || (now, "first-generation".into()))
        .await
        .unwrap()
        .was_claimed());
    ConfigAccess::write_local(&node,"test.complete_graph_entry",&format!("mutation {{update_AgentRequest(filter: {{_docID: {{_eq: \"{}\"}}}},input: {{lifecycle_state: \"completed\"}}) {{_docID}}}}",escape_graphql_string(&first.doc_id))).await.unwrap();
    let routes = planned_trigger_nodes(&plan).unwrap();
    for (stage, collection) in [("verify", "ContinueStep"), ("triage", "ContinueSummary")] {
        let trigger = &routes
            .iter()
            .find(|(_, node)| node.as_str() == stage)
            .unwrap()
            .0;
        let target = resolve_graph_session(node.as_ref(), trigger, Some(&run.correlation), owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(target, first.session_id);
        let created = ConfigAccess::write_local(
            &node,
            "test.graph_stage_output",
            &format!(
                "mutation {{create_{collection}(input: {{run_id: \"{}\"}}) {{_docID}}}}",
                escape_graphql_string(&run.correlation)
            ),
        )
        .await
        .unwrap();
        let source_id = crate::graphql::created_doc_id(&created, collection).unwrap();
        let next = admit_session_graph_task(
            &node,
            &plan,
            &run,
            &identity,
            stage,
            collection,
            &source_id,
            Some(&target),
        )
        .await;
        assert_eq!(next.session_id, first.session_id);
    }
    let view = load_graph_run_view(&node, owner, &run.run_id)
        .await
        .unwrap();
    assert_eq!(view.requests.len(), 3);
    for stage in ["recon", "verify", "triage"] {
        let requests = view
            .requests
            .iter()
            .filter(|request| request.node_id.as_deref() == Some(stage))
            .collect::<Vec<_>>();
        assert_eq!(requests.len(), 1, "{stage}");
        assert_eq!(
            requests[0].session_id.as_deref(),
            Some(first.session_id.as_str())
        );
    }
}
