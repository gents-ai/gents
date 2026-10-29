use super::*;

struct RuntimeControlFixture {
    _dir: tempfile::TempDir,
    service: AcpService,
    _backend: MockStreamingBackend,
    shutdown: tokio::sync::watch::Sender<bool>,
    runtime: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

impl RuntimeControlFixture {
    async fn stop_runtime_parts(
        shutdown: &tokio::sync::watch::Sender<bool>,
        runtime: &mut Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    ) {
        let _ = shutdown.send(true);
        if let Some(runtime) = runtime.take() {
            runtime.await.unwrap().unwrap();
        }
    }

    async fn shutdown(mut self) {
        Self::stop_runtime_parts(&self.shutdown, &mut self.runtime).await;
    }
}

async fn runtime_control_fixture(name: &str, plans: Vec<StreamPlan>) -> RuntimeControlFixture {
    runtime_control_fixture_with_plan_builder(name, |_| plans).await
}

async fn runtime_control_fixture_with_plan_builder(
    name: &str,
    plans: impl FnOnce(&str) -> Vec<StreamPlan>,
) -> RuntimeControlFixture {
    let dir = tempfile::tempdir().unwrap();
    let identity =
        Arc::new(gents::KeyIdentity::load_or_create(dir.path().join("agent.key"), None).unwrap());
    let node_did = gents::NodeIdentity::did(identity.as_ref()).to_string();
    let agent_id = gents::default_agent_id_for_node(&node_did);
    let node = Arc::new(
        EmbeddedNode::builder()
            .data_path(dir.path().join("node"))
            .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
            .with_node_identity_did(&node_did)
            .build()
            .await
            .unwrap(),
    );
    gents::ensure_runtime_schemas(node.as_ref()).await.unwrap();
    let backend =
        MockStreamingBackend::start_with_plans("grok-control-model", plans(&agent_id)).unwrap();
    configure_runtime_agent(
        &node,
        &node_did,
        &agent_id,
        &format!("grok-control-{name}"),
        backend.endpoint(),
        "grok-control-model",
        true,
    )
    .await;
    let graphql = spawn_mock_graphql(node.clone()).await;
    let config = AcpServiceConfig {
        node: node.clone(),
        node_did: Arc::from(node_did.as_str()),
        agent_id: Arc::from(agent_id.as_str()),
        current_model: bound_model(),
        grok_home: None,
        plugin_home: std::path::PathBuf::new(),
    };
    let turns = Arc::new(TurnManager::new(
        node.clone(),
        crate::commands::grok_shim::turn::TurnManagerConfig {
            actor: identity::Did::new(node_did.clone()).expect("fixture creator DID"),
            node_did: node_did.clone(),
            agent_id: agent_id.clone(),
            graphql,
        },
    ));
    let runtime_identity: Arc<dyn gents::NodeIdentity> = identity;
    let agent = gents::Gents::from_default_agent_documents(
        node.clone(),
        runtime_identity,
        gents::DocumentRuntimeOptions {
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let projections = Arc::new(
        ProjectionEngine::new(
            node.clone(),
            crate::commands::grok_shim::projection::BoundModelContext::new(
                "grok-control-model".into(),
                "Grok control model".into(),
                262_144,
            ),
        )
        .with_background_executions(agent.background_execution_registry()),
    );
    let service = AcpService::new(config, turns, projections);
    let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let runtime = tokio::spawn(agent.run(shutdown_rx));
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let did = escape_graphql_string(&node_did);
            let response = node.execute(&format!(r#"{{ NodeRuntime(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{reconcile_phase}} NodeReadiness(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{node_did snapshot_json updated_at}} }}"#)).await;
            ensure_no_errors(&response, "runtime control readiness").unwrap();
            let data = response.data.as_ref().unwrap();
            let status = data["NodeRuntime"].as_array().unwrap();
            let readiness = data["NodeReadiness"].as_array().unwrap();
            let snapshot = readiness.first().and_then(|row| {
                let row = serde_json::from_value::<gents_protocol::row::NodeReadinessRow>(row.clone()).ok()?;
                gents_protocol::row::decode_node_readiness_snapshot(&row, &node_did).ok()
            });
            if status.len() == 1
                && status[0]["reconcile_phase"] == "idle"
                && readiness.len() == 1
                && snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.process_state.accepts_work()
                        && snapshot.active_generation >= 1
                        && snapshot.agents.iter().any(|agent| {
                            agent.agent_id == agent_id
                                && agent.state == gents_protocol::row::AgentReadinessState::Ready
                        })
                })
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("runtime control fixture readiness timeout");
    RuntimeControlFixture {
        _dir: dir,
        service,
        _backend: backend,
        shutdown,
        runtime: Some(runtime),
    }
}

async fn wait_for_running_bash_handle(node: &EmbeddedNode, session_id: &str) -> String {
    let session_id = escape_graphql_string(session_id);
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let response = node.execute(&format!(r#"{{ AgentToolCall(filter: {{session_id: {{_eq: "{session_id}"}}, tool_name: {{_eq: "bash"}}, lifecycle_state: {{_eq: "running"}}}}, limit: 2) {{tool_call_id}} }}"#)).await;
            ensure_no_errors(&response, "running bash handle").unwrap();
            let rows = response.data.as_ref().unwrap()["AgentToolCall"].as_array().unwrap();
            if rows.len() == 1 {
                return rows[0]["tool_call_id"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.expect("running bash handle timeout")
}

async fn wait_for_child_request(node: &Arc<EmbeddedNode>) -> gents_protocol::row::AgentRequestRow {
    let observed = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            let response = node.execute(r#"{ AgentRequest {_docID request_id node_did requester_did session_id caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id} }"#).await;
            ensure_no_errors(&response, "child request").unwrap();
            let rows = response.data.as_ref().unwrap()["AgentRequest"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| {
                    row["caused_by_parent_request_doc_id"]
                        .as_str()
                        .is_some_and(|id| !id.trim().is_empty())
                        && row["caused_by_parent_tool_call_doc_id"]
                            .as_str()
                            .is_some_and(|id| !id.trim().is_empty())
                })
                .collect::<Vec<_>>();
            assert!(rows.len() <= 1, "ambiguous physical child requests: {rows:?}");
            if rows.len() == 1 {
                return serde_json::from_value((*rows[0]).clone()).unwrap();
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    match observed {
        Ok(child) => child,
        Err(_) => {
            let diagnostic = node
                .execute(r#"{ AgentToolCall(filter: {tool_name: {_eq: "agent_new"}}) {_docID request_id request_doc_id tool_call_id lifecycle_state tool_failure_class await_mode} }"#)
                .await;
            let presentation = diagnostic
                .data
                .as_ref()
                .and_then(|data| data["AgentToolCall"].as_array())
                .and_then(|rows| rows.first())
                .and_then(|row| Some((row["_docID"].as_str()?, row["request_doc_id"].as_str()?)));
            let presentation = if let Some((tool_doc_id, request_doc_id)) = presentation {
                let request = node
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{node_did requester_did session_id}} }}"#,
                        escape_graphql_string(request_doc_id)
                    ))
                    .await;
                let scope = request
                    .data
                    .as_ref()
                    .and_then(|data| data["AgentRequest"].as_array())
                    .and_then(|rows| rows.first());
                if let Some(scope) = scope {
                    gents::tool_call_lifecycle::load_tool_call_presentation(
                        &gents::ConfigAccess::local(node.clone()),
                        tool_doc_id,
                        scope["node_did"].as_str().unwrap_or_default(),
                        scope["session_id"].as_str().unwrap_or_default(),
                        scope["requester_did"].as_str(),
                    )
                    .await
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|error| format!("presentation error: {error:#}"))
                } else {
                    "parent request scope unavailable".into()
                }
            } else {
                "agent_new call unavailable".into()
            };
            panic!(
                "child request timeout; durable agent_new observations: data={:?} errors={:?}; canonical presentation={presentation}",
                diagnostic.data, diagnostic.errors,
            );
        }
    }
}

async fn exact_request_by_doc_id(
    node: &EmbeddedNode,
    doc_id: &str,
) -> gents_protocol::row::AgentRequestRow {
    let doc_id = escape_graphql_string(doc_id);
    let response = node.execute(&format!(r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{doc_id}"}}}}, limit: 2) {{_docID request_id node_did requester_did session_id}} }}"#)).await;
    ensure_no_errors(&response, "physical parent request").unwrap();
    let rows = response.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert_eq!(rows.len(), 1, "physical parent request must be exact");
    serde_json::from_value(rows[0].clone()).unwrap()
}

#[tokio::test]
async fn native_task_kill_is_scoped_idempotent_and_uses_stock_envelope() {
    let prompt = "start task process";
    let fixture = runtime_control_fixture(
        "task-kill",
        vec![StreamPlan::current_authored_user(
            prompt,
            vec![
                StreamResponse::streams(
                    prompt,
                    vec![StreamChunk::tool_call(
                        "task-meta",
                        "spawn_process",
                        r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
                    )],
                ),
                StreamResponse::completes(prompt, ["task started"]),
            ],
        )],
    )
    .await;
    let service = &fixture.service;
    service
        .handle_acp_payload(&request_payload(
            "session/new",
            json!({"_meta":{"sessionId":"task-session"}}),
        ))
        .await;
    let completed = service
        .handle_acp_payload(&request_payload(
            "session/prompt",
            json!({"sessionId":"task-session","prompt":[{"type":"text","text":prompt}]}),
        ))
        .await;
    let completed_response = parse_response(completed.response.as_deref().unwrap());
    assert!(
        completed_response.get("error").is_none(),
        "runtime task prompt failed: {completed_response}"
    );
    let task_id = wait_for_running_bash_handle(service.config.node.as_ref(), "task-session").await;
    for params in [
        json!({"sessionId":"task-session"}),
        json!({"sessionId":"task-session","taskId":""}),
        json!({"sessionId":"task-session","taskId":task_id,"source":"invalid"}),
    ] {
        let response = service
            .handle_acp_payload(&request_payload("x.ai/task/kill", params))
            .await;
        assert_eq!(
            parse_response(response.response.as_deref().unwrap())["error"]["code"],
            -32602
        );
    }
    for (session, expected) in [
        ("foreign", "not_found"),
        ("task-session", "killed"),
        ("task-session", "already_exited"),
    ] {
        let response = service
            .handle_acp_payload(&request_payload(
                "x.ai/task/kill",
                json!({"sessionId":session, "taskId":task_id, "source":"clientUi"}),
            ))
            .await;
        let value = parse_response(response.response.as_deref().unwrap());
        assert_eq!(
            value["result"]["result"],
            json!({"taskId":task_id,"outcome":expected}),
            "{value}"
        );
    }
    let response = service
        .config
        .node
        .execute(&format!(
            r#"{{ AgentToolCall(filter: {{tool_call_id: {{_eq: "{}"}}}}) {{lifecycle_state}} }}"#,
            escape_graphql_string(&task_id)
        ))
        .await;
    assert_eq!(
        response.data.unwrap()["AgentToolCall"][0]["lifecycle_state"],
        "cancelled"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn native_child_controls_and_usage_follow_physical_lineage() {
    let parent_prompt = "spawn child worker";
    let child_prompt = "run child process";
    let unlinked_prompt = "run unlinked process";
    let child_args = serde_json::json!({
        "agent": "__BEHAVIOR__",
        "prompt": child_prompt
    });
    let mut fixture = runtime_control_fixture_with_plan_builder("child-controls", |agent_id| {
        vec![
            StreamPlan::current_authored_user(
                parent_prompt,
                vec![
                    StreamResponse::streams(
                        parent_prompt,
                        vec![StreamChunk::tool_call(
                            "spawn-child-meta",
                            "agent_new",
                            child_args.to_string().replace("__BEHAVIOR__", agent_id),
                        )],
                    ),
                    StreamResponse::completes(parent_prompt, ["child started"]),
                ],
            ),
            StreamPlan::current_authored_user(
                child_prompt,
                vec![
                    StreamResponse::streams(
                        child_prompt,
                        vec![StreamChunk::tool_call(
                            "child-process-meta",
                            "spawn_process",
                            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
                        )],
                    ),
                    StreamResponse::completes(child_prompt, ["child process started"]),
                ],
            ),
            StreamPlan::current_authored_user(
                unlinked_prompt,
                vec![
                    StreamResponse::streams(
                        unlinked_prompt,
                        vec![StreamChunk::tool_call(
                            "unlinked-process-meta",
                            "spawn_process",
                            r#"{"tool_name":"bash","args":{"command":"sleep","args":["60"]}}"#,
                        )],
                    ),
                    StreamResponse::completes(unlinked_prompt, ["unlinked process started"]),
                ],
            ),
        ]
    })
    .await;
    let service = &fixture.service;
    for (session, prompt) in [
        ("parent-session", parent_prompt),
        ("unlinked-session", unlinked_prompt),
    ] {
        service
            .handle_acp_payload(&request_payload(
                "session/new",
                json!({"_meta":{"sessionId":session}}),
            ))
            .await;
        let completed = service
            .handle_acp_payload(&request_payload(
                "session/prompt",
                json!({"sessionId":session,"prompt":[{"type":"text","text":prompt}]}),
            ))
            .await;
        let completed_response = parse_response(completed.response.as_deref().unwrap());
        assert!(
            completed_response.get("error").is_none(),
            "runtime control prompt failed for {session}: {completed_response}"
        );
    }
    let node = &service.config.node;
    let did = gents::graphql::escape_graphql_string(&service.config.node_did);
    let child = wait_for_child_request(node).await;
    let child_session = child.session_id.as_deref().unwrap();
    let child_process = wait_for_running_bash_handle(node.as_ref(), child_session).await;
    for (session, outcome) in [
        ("unlinked-session", "not_found"),
        ("parent-session", "killed"),
        (child_session, "already_exited"),
    ] {
        let response = service
            .handle_acp_payload(&request_payload(
                "x.ai/task/kill",
                json!({
                    "sessionId": session, "taskId": child_process, "source": "teardown"
                }),
            ))
            .await;
        let response = parse_response(response.response.as_deref().unwrap());
        assert_eq!(
            response["result"]["result"]["outcome"], outcome,
            "{response}"
        );
    }
    let parent = exact_request_by_doc_id(
        node.as_ref(),
        child.caused_by_parent_request_doc_id.as_deref().unwrap(),
    )
    .await;
    RuntimeControlFixture::stop_runtime_parts(&fixture.shutdown, &mut fixture.runtime).await;
    let baseline = service
        .handle_acp_payload(&request_payload(
            "x.ai/session/usage",
            json!({"sessionId":"parent-session"}),
        ))
        .await;
    let baseline = parse_response(baseline.response.as_deref().unwrap());
    assert!(baseline.get("error").is_none(), "{baseline}");
    let baseline = &baseline["result"]["usage"];
    let baseline_input = baseline["inputTokens"].as_u64().unwrap();
    let baseline_output = baseline["outputTokens"].as_u64().unwrap();
    let baseline_cached = baseline["cachedReadTokens"].as_u64().unwrap();
    let baseline_calls = baseline["modelCalls"].as_u64().unwrap();
    let foreign = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{purpose: "normal", 
            request_id: "foreign-usage", session_id: "{}", node_did: "{did}", requester_did: "did:test:foreign", lifecycle_state: "completed"
        }}) {{_docID}} }}"#, escape_graphql_string(child_session))).await;
    ensure_no_errors(&foreign, "foreign usage fixture").unwrap();
    for (request, input, output, cached) in [
        (parent.request_id.as_str(), 100, 10, 50),
        (child.request_id.as_str(), 200, 20, 100),
        ("foreign-usage", 9999, 999, 0),
    ] {
        let owner = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{request}"}}}}) {{_docID}} }}"#
            ))
            .await;
        ensure_no_errors(&owner, "usage physical identity").unwrap();
        let doc = owner.data.as_ref().unwrap()["AgentRequest"][0]["_docID"]
            .as_str()
            .unwrap();
        let call = node.execute(&format!(r#"mutation {{ create_InferenceCall(input: {{
                call_id: "usage-{request}", request_id: "parent", request_doc_id: "{}", node_did: "{did}",
                call_seq: 1, call_kind: "inference", prompt_tokens: {input}, completion_tokens: {output}, cached_input_tokens: {cached}
            }}) {{_docID}} }}"#, escape_graphql_string(doc))).await;
        ensure_no_errors(&call, "usage call fixture").unwrap();
    }
    let usage = service
        .handle_acp_payload(&request_payload(
            "x.ai/session/usage",
            json!({"sessionId":"parent-session"}),
        ))
        .await;
    let response = parse_response(usage.response.as_deref().unwrap());
    assert!(response.get("error").is_none(), "{response}");
    assert_eq!(
        response["result"]["usage"]["inputTokens"],
        baseline_input + 300
    );
    assert_eq!(
        response["result"]["usage"]["outputTokens"],
        baseline_output + 30
    );
    assert_eq!(
        response["result"]["usage"]["cachedReadTokens"],
        baseline_cached + 150
    );
    assert_eq!(
        response["result"]["usage"]["modelCalls"],
        baseline_calls + 2
    );
    assert_eq!(response["result"]["usage"]["usageIsIncomplete"], true);
    assert!(response["result"]["usage"].get("costUsdTicks").is_none());
    fixture.shutdown().await;
}
