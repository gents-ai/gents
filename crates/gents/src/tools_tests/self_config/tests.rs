use super::fixture::SelfConfigFixture;
use super::*;
use gents_protocol::tool_names::CONFIG_TOOL_NAME;
use gents_toolcalls::test_support::self_config::{
    call_config_tool, config, node_identity, take_agent_tool,
};
use gents_tools::builtin::self_config::test_support::{
    agent_inspect, agent_request, backend_request, protect_working_agent, tools_request,
};

#[derive(Clone)]
struct RootReadModel {
    path: String,
    turns: Arc<std::sync::atomic::AtomicUsize>,
    provider_inputs: Arc<std::sync::Mutex<Vec<String>>>,
}

#[allow(refining_impl_trait)]
impl rig::completion::CompletionModel for RootReadModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        unreachable!("the acceptance test constructs its deterministic model directly")
    }

    async fn completion(
        &self,
        _request: rig::completion::CompletionRequest,
    ) -> Result<rig::completion::CompletionResponse<()>, rig::completion::CompletionError> {
        Err(rig::completion::CompletionError::ProviderError(
            "non-streaming completion is unused".into(),
        ))
    }

    async fn stream(
        &self,
        request: rig::completion::CompletionRequest,
    ) -> Result<rig::streaming::StreamingCompletionResponse<()>, rig::completion::CompletionError>
    {
        use std::sync::atomic::Ordering;

        gents_inference::test_support::capture_scripted_provider_request(&request, "scripted")
            .await?;
        if !request.tools.iter().any(|tool| tool.name == "read_file") {
            let stream: rig::streaming::StreamingResult<()> = Box::pin(futures::stream::iter([
                Ok(rig::streaming::RawStreamingChoice::Message(
                    "workspace-root-check".into(),
                )),
                Ok(rig::streaming::RawStreamingChoice::FinalResponse(())),
            ]));
            return Ok(rig::streaming::StreamingCompletionResponse::stream(stream));
        }
        self.provider_inputs
            .lock()
            .expect("provider input capture")
            .push(serde_json::to_string(&request.chat_history).expect("serialize provider input"));
        let choices = if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
            vec![
                rig::streaming::RawStreamingChoice::ToolCall(
                    rig::streaming::RawStreamingToolCall::new(
                        "root-read".into(),
                        "read_file".into(),
                        json!({"path": self.path}),
                    ),
                ),
                rig::streaming::RawStreamingChoice::FinalResponse(()),
            ]
        } else {
            vec![
                rig::streaming::RawStreamingChoice::Message("root check complete".into()),
                rig::streaming::RawStreamingChoice::FinalResponse(()),
            ]
        };
        let stream: rig::streaming::StreamingResult<()> =
            Box::pin(futures::stream::iter(choices.into_iter().map(Ok)));
        Ok(rig::streaming::StreamingCompletionResponse::stream(stream))
    }
}

#[tokio::test]
async fn config_tool_access_follows_the_modeled_managed_route() {
    let fixture = gents_automation::test_support::self_config_fixture("config-tool-route").await;
    let runtime = fixture.db.node_identity.clone();
    let foreign = tokio::task::spawn_blocking(|| node_identity("foreign-runtime"))
        .await
        .unwrap();
    let sibling = gents_store::identity::resolve_agent_identity(
        runtime.as_ref(),
        &gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap(),
    )
    .unwrap();
    let foreign_agent = gents_store::identity::resolve_agent_identity(
        foreign.as_ref(),
        &gents_store::identity::provision_agent_identity(foreign.as_ref()).unwrap(),
    )
    .unwrap();
    let user = tokio::task::spawn_blocking(|| node_identity("ordinary-user"))
        .await
        .unwrap();
    let mut tampered = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
    tampered.agent_did = user.did().to_owned();
    let invalid_binding =
        gents_store::identity::resolve_agent_identity(runtime.as_ref(), &tampered);
    assert!(invalid_binding.is_err());
    let claimed_root =
        gents_store::identity::RegisteredIdentity::from_registered_did(runtime.did(), None)
            .unwrap();
    let spoofed_root = gents_store::identity::resolve_agent_identity(
        &claimed_root,
        &gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap(),
    );
    assert!(spoofed_root.is_err());
    let cases = tokio::task::spawn_blocking(
        gents_lean_vocab::families::agent_block_signing_config_tool::cases::<String>,
    )
    .await
    .unwrap();
    for case in &cases {
        let name = case.input.name.as_str();
        let (node_did, actor, signer) = match name {
            "managed-agent" => (
                fixture.node_did().to_owned(),
                fixture.agent_did().to_owned(),
                fixture.agent.managed_agent_signer(),
            ),
            "missing-key" => (
                fixture.node_did().to_owned(),
                fixture.agent_did().to_owned(),
                None,
            ),
            "sibling-key" => (
                fixture.node_did().to_owned(),
                fixture.agent_did().to_owned(),
                sibling.managed_agent_signer(),
            ),
            "invalid-binding" => (
                fixture.node_did().to_owned(),
                fixture.agent_did().to_owned(),
                invalid_binding
                    .as_ref()
                    .ok()
                    .and_then(|identity| identity.managed_agent_signer()),
            ),
            "spoofed-root" => (
                fixture.node_did().to_owned(),
                fixture.agent_did().to_owned(),
                spoofed_root
                    .as_ref()
                    .ok()
                    .and_then(|identity| identity.managed_agent_signer()),
            ),
            "foreign-runtime" => (
                fixture.node_did().to_owned(),
                foreign_agent.did().to_owned(),
                foreign_agent.managed_agent_signer(),
            ),
            "empty-actor" => (
                fixture.node_did().to_owned(),
                String::new(),
                fixture.agent.managed_agent_signer(),
            ),
            "ordinary-node" => (
                fixture.node_did().to_owned(),
                fixture.node_did().to_owned(),
                None,
            ),
            "ordinary-default-node" => (fixture.node_did().to_owned(), String::new(), None),
            "unconfigured-explicit-actor" => (String::new(), fixture.agent_did().to_owned(), None),
            "ordinary-user-key-is-not-adopted" => (
                fixture.node_did().to_owned(),
                user.did().to_owned(),
                user.managed_agent_signer(),
            ),
            other => panic!("the config tool scenario has no fixture for {other}"),
        };
        let access = SelfConfigCore::new(fixture.db.node.clone(), node_did, actor)
            .and_then(|core| core.with_agent_signer(signer).access());
        let route = match &access {
            Ok(access) => {
                assert!(access.has_managed_agent_signer(), "{name}");
                "agent"
            }
            Err(_) => "refuse",
        };
        if name == "missing-key" {
            let error = access.err().expect("a missing signer is refused");
            assert!(
                matches!(
                    error.downcast_ref(),
                    Some(gents_store::config_client::ConfigClientError::ManagedAgentSignerRequired)
                ),
                "{error:#}"
            );
        }
        case.check(route.to_owned());
    }
}

#[tokio::test]
async fn agent_create_commits_and_resolves_after_restart() {
    let fixture = gents_automation::test_support::self_config_fixture("agent-create").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    let root = fixture.admitted_workspace_root().await;
    let node = fixture.db.node.clone();
    let node_did = fixture.node_did().to_owned();
    let profile_id = format!("{}:inference", fixture.agent_did());
    let tools = fixture.tools(&["node"]);
    let rejected_preview = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "preview".into(),
            "create".into(),
            "--display-name".into(),
            "Research Assistant".into(),
            "--preset".into(),
            "write".into(),
            "--profile".into(),
            profile_id.clone(),
        ],
    )
    .await
    .expect("invalid preview is an inspectable no-write result");
    let rejected_preview: Value = serde_json::from_str(&rejected_preview).unwrap();
    assert_eq!(rejected_preview["committed"], false);
    assert_eq!(rejected_preview["admitted"], false);
    assert_eq!(rejected_preview["refusal"], "invalid_input");
    assert!(rejected_preview["message"]
        .as_str()
        .unwrap()
        .contains("fresh agents require system_prompt"));
    assert_eq!(
        rejected_preview["next_call"],
        json!({"argv":["agent","create","--help"]})
    );
    let admitted_preview = call_config_tool(
        &tools,
        vec![
            "agent".into(),
            "preview".into(),
            "create".into(),
            "--display-name".into(),
            "Research Assistant".into(),
            "--description".into(),
            "Researches a focused question".into(),
            "--system-prompt".into(),
            "Research the question and cite evidence.".into(),
            "--preset".into(),
            "write".into(),
            "--root".into(),
            root.clone(),
            "--profile".into(),
            profile_id.clone(),
        ],
    )
    .await
    .expect("complete preview succeeds");
    let admitted_preview: Value = serde_json::from_str(&admitted_preview).unwrap();
    assert_eq!(admitted_preview["committed"], false);
    assert_eq!(admitted_preview["admitted"], true);
    assert_eq!(admitted_preview["operation"], "create");
    assert_eq!(admitted_preview["make_default"], false);
    assert_eq!(
        admitted_preview["agent"]["display_name"],
        "Research Assistant"
    );
    assert_eq!(
        admitted_preview["agent"]["inference_profile_id"],
        profile_id
    );
    assert_eq!(
        admitted_preview["agent"]["context_id"],
        format!(
            "{}:context",
            admitted_preview["agent_did"].as_str().unwrap()
        )
    );
    assert_eq!(
        gents_documents::list_agents(&node, &node_did)
            .await
            .unwrap()
            .len(),
        1
    );
    let tool = take_agent_tool(tools);

    let args = json!({"argv": [
        "agent", "create",
        "--display-name", "Research Assistant",
        "--description", "Researches a focused question",
        "--system-prompt", "Research the question and cite evidence.",
        "--preset", "write",
        "--root", root,
        "--profile", profile_id
    ]})
    .to_string();

    let output = tool.call(args).await.expect("direct agent create commits");
    let agents = gents_documents::list_agents(&node, &node_did)
        .await
        .expect("list agents");
    let created: Vec<_> = agents
        .iter()
        .filter(|agent| agent.agent_did != fixture.agent_did())
        .collect();
    assert_eq!(created.len(), 1, "exactly one new agent materialized");
    assert_eq!(
        created[0].display_name,
        Some("Research Assistant".to_string())
    );
    let context_id = gents_store::graphql::escape_graphql_string(
        created[0]
            .context_id
            .as_deref()
            .expect("created agent has context"),
    );
    let response = fixture.user_access.execute(&format!(
        r#"{{ AgentContext(filter: {{ context_id: {{ _eq: "{context_id}" }} }}) {{ description system_prompt }} }}"#,
    )).await.unwrap();
    let context = &response["data"]["AgentContext"][0];
    assert_eq!(
        context["system_prompt"],
        "Research the question and cite evidence."
    );
    assert_eq!(context["description"], "Researches a focused question");

    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["committed"], true);
    assert_eq!(output["agent_did"], created[0].agent_did);
    let effective: Value = serde_json::from_str(
        &agent_inspect(&fixture.core(), &created[0].agent_did)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        effective["effective_config"]["context"]["system_prompt"],
        "Research the question and cite evidence."
    );
    assert_eq!(
        effective["effective_config"]["tool_grants"]["configured"],
        json!({"lsp": false, "network_mode": "inherit"})
    );
    // The creating Agent owns the documents it created, so it configures them
    // without a further grant (the User cannot add `writer` to them).
    let grant_tools = fixture.tools(&["node", "tools"]);
    let selected = call_config_tool(
        &grant_tools,
        vec![
            "tools".into(),
            "edit".into(),
            "--agent".into(),
            created[0].agent_did.clone(),
            "--set".into(),
            r#"integrations={"lsp":{}}"#.into(),
            "--set".into(),
            format!(r#"host={{"root":{root:?},"files":{{"mode":"ReadOnly"}},"bash":{{"mode":"ReadOnly","network_mode":"disabled"}}}}"#),
            "--allow-drop".into(),
            "host".into(),
        ],
    )
    .await
    .expect("explicit second operation grants sibling tools");
    let applied: Value = serde_json::from_str(&selected).unwrap();
    assert_eq!(applied["committed"], true);
    let selected = call_config_tool(
        &grant_tools,
        vec!["get".into(), "--agent".into(), created[0].agent_did.clone()],
    )
    .await
    .expect("inspect the selected sibling after the canonical tools patch");
    let selected: Value = serde_json::from_str(&selected).unwrap();
    assert_eq!(
        selected["tool_grants"]["configured"],
        json!({"lsp": true, "network_mode": "disabled"})
    );
    assert_eq!(
        selected["runtime_effective"]["effective"]["network_mode"],
        "disabled"
    );

    fixture
        .update_signed_configuration(
            gents_store::collection::Collection::DefaultAgentSelection,
            fixture.node_did(),
            json!({"default_agent_did": created[0].agent_did}),
        )
        .await;
    let snapshot = crate::runtime::agent::resolve_document_runtime_snapshot(
        &node,
        &crate::runtime::agent::DocumentResolveContext::for_tests(
            fixture.db.node_identity.clone(),
            gents_tools::surface::ToolCeiling::readonly(),
            Default::default(),
        ),
    )
    .await
    .expect("restart-style runtime resolution accepts the materialized agent");
    assert_eq!(snapshot.default_agent_did, created[0].agent_did);
    let runtime_agent = snapshot
        .agent_with_tools(&created[0].agent_did)
        .unwrap_or_else(|| {
            panic!(
                "new default is runnable after re-resolution: {:?}",
                snapshot.unavailable_agents
            )
        });
    assert_eq!(
        runtime_agent.agent.system_prompt,
        "Research the question and cite evidence."
    );
    let names = runtime_agent
        .tools
        .resolve(&node, &node_did, &gents_tools::plugin_host::NoPluginHost)
        .await
        .expect("new agent tool surface resolves after restart")
        .tool_names();
    assert!(names.iter().any(|name| name == "lsp"), "{names:?}");
    assert!(
        !names.iter().any(|name| name == CONFIG_TOOL_NAME),
        "{names:?}"
    );
}

#[tokio::test]
async fn descendant_root_preview_apply_reconcile_reaches_fresh_request_file_tools() {
    let fixture =
        gents_automation::test_support::self_config_fixture("descendant-root-self-config").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_node_reader().await;
    fixture.grant_agent_catalog().await;
    let node = fixture.db.node.clone();
    let identity = fixture.db.node_identity.clone();
    let owner = fixture.node_did().to_owned();
    let seed_agent = fixture.agent_did().to_owned();
    let operator = fixture.user_access();

    let operator_root = tempfile::tempdir().expect("operator root");
    let selected_root = operator_root.path().join("projects").join("mandrake");
    std::fs::create_dir_all(&selected_root).expect("selected descendant root");
    std::fs::write(
        selected_root.join("marker.txt"),
        "fresh request sees descendant\n",
    )
    .expect("marker file");
    let sibling_root = operator_root.path().join("projects").join("other");
    std::fs::create_dir_all(&sibling_root).expect("sibling root");
    let sibling_marker = sibling_root.join("not-authorized.txt");
    std::fs::write(
        &sibling_marker,
        "operator ceiling must not widen selection\n",
    )
    .expect("sibling marker");
    let original_transcript = operator_root.path().join("original-transcript.txt");
    std::fs::write(&original_transcript, "private transcript fixture\n")
        .expect("original transcript fixture");

    let selected_text = selected_root.to_string_lossy().into_owned();
    gents_tools::surface::set_workspace_root_with_readers(
        operator,
        &owner,
        &selected_text,
        None,
        true,
        std::slice::from_ref(&seed_agent),
    )
    .await
    .expect("publish selected WorkspaceRoot");

    let mut tool_config = config(&["node"]);
    tool_config.agent_did = seed_agent.clone();
    tool_config.process_ceiling = gents_tools::surface::SelfConfigProcessCeiling {
        file_mode: gents_documents::FileToolMode::ReadOnly,
        bash_mode: gents_documents::BashMode::Off,
        root: Some(operator_root.path().to_path_buf()),
    };
    let agent_tools = fixture.tools_with_config(&tool_config);
    let profile_id = format!("{seed_agent}:inference");
    let command = |preview: bool| {
        let mut argv = vec!["agent".to_string()];
        if preview {
            argv.push("preview".to_string());
        }
        argv.extend([
            "create".to_string(),
            "--display-name".to_string(),
            "Mandrake descendant".to_string(),
            "--system-prompt".to_string(),
            "Use only the selected workspace.".to_string(),
            "--root".to_string(),
            selected_text.clone(),
            "--preset".to_string(),
            "readonly".to_string(),
            "--profile".to_string(),
            profile_id.clone(),
        ]);
        argv
    };
    let preview: Value = serde_json::from_str(
        &call_config_tool(&agent_tools, command(true))
            .await
            .expect("agent preview accepts the published descendant"),
    )
    .expect("preview json");
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["admitted"], true);
    assert_eq!(
        gents_documents::list_agents(&node, &owner)
            .await
            .unwrap()
            .len(),
        1
    );
    let output = call_config_tool(&agent_tools, command(false))
        .await
        .expect("direct create commits");
    let output: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(output["committed"], true);
    let agents = gents_documents::list_agents(&node, &owner)
        .await
        .expect("list materialized agents");
    let created = agents
        .iter()
        .find(|agent| agent.agent_did != seed_agent)
        .expect("agent reconciler materialized one agent");
    let created_signer = gents_store::identity::resolve_agent_identity(
        identity.as_ref(),
        &created.identity_binding(),
    )
    .expect("created Agent is derived from the held runtime");
    let created_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        ::identity::Did::new(&created.agent_did).unwrap(),
        created_signer.as_ref(),
    )
    .unwrap();
    fixture
        .update_signed_configuration(
            gents_store::collection::Collection::DefaultAgentSelection,
            &owner,
            json!({"default_agent_did": created.agent_did}),
        )
        .await;
    gents_tools::surface::grant_workspace_root_reader(operator, &selected_text, &created.agent_did)
        .await
        .expect("User grants the created Agent access to the selected root");
    let runtime_view =
        crate::runtime::agent::document_view::load_document_runtime_view(&node, &owner)
            .await
            .expect("fresh runtime view after agent publication");
    let context_id = created.context_id.as_ref().expect("created context");
    let tools_id = runtime_view.contexts[context_id]
        .value
        .tools_id
        .as_ref()
        .expect("created tools");
    let canonical_selected =
        std::fs::canonicalize(&selected_root).expect("canonical selected root");
    assert_eq!(
        runtime_view.tools[tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(canonical_selected.to_string_lossy().as_ref()),
        "agent apply must persist the canonical selected root, not the operator ceiling"
    );

    let snapshot = crate::runtime::agent::resolve_document_runtime_snapshot(
        &node,
        &crate::runtime::agent::DocumentResolveContext::for_tests(
            identity.clone(),
            gents_tools::surface::ToolCeiling::readonly_at(operator_root.path()),
            Default::default(),
        ),
    )
    .await
    .expect("fresh request runtime snapshot");
    let runtime_agent = snapshot
        .agent_with_tools(&created.agent_did)
        .expect("fresh request agent is runnable");
    let surface = Arc::new(
        runtime_agent
            .tools
            .resolve(&node, &owner, &gents_tools::plugin_host::NoPluginHost)
            .await
            .expect("resolve fresh-request tool surface"),
    );
    let runtime = gents_tools::surface::ToolRuntimeContext::oneshot_with_node_did(
        node.clone(),
        owner.clone(),
    );

    let cases = [
        (
            "selected descendant",
            "marker.txt".to_string(),
            "fresh request sees descendant",
            None,
        ),
        (
            "sibling under operator ceiling",
            sibling_marker.to_string_lossy().into_owned(),
            "outside the allowed tool root",
            Some("operator ceiling must not widen selection"),
        ),
        (
            "original transcript path",
            original_transcript.to_string_lossy().into_owned(),
            "outside the allowed tool root",
            Some("private transcript fixture"),
        ),
    ];
    for (label, path, expected_result, forbidden_content) in cases {
        let request_id = uuid::Uuid::new_v4().to_string();
        let request = gents_session::request::lifecycle::RequestSpec::new(
            gents_protocol::request_admission::RequestPurpose::Normal,
            gents_session::request::lifecycle::RequestIdentity {
                requester_did: Some(created.agent_did.clone()),
                request_id: request_id.clone(),
                node_did: owner.clone(),
                agent_did: created.agent_did.clone(),
                session_id: uuid::Uuid::new_v4().to_string(),
                content: "Read the requested workspace path.".to_string(),
                execution_origin: gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                &created.agent_did,
            ),
        );
        let request = gents_session::request::lifecycle::build_signed_client_request(
            &created_access,
            request,
            gents_session::request::lifecycle::RequestSigner::Identity(created_signer.as_ref()),
        )
        .await
        .expect("reserve and sign a fresh request for the reconciled Agent");
        gents_session::request::lifecycle::submit_client_request(&created_access, &request)
            .await
            .expect("publish the fresh request and its session");
        let response = created_access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{request_id: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                escape_graphql_string(&request_id),
            ))
            .await
            .unwrap();
        let rows = response["data"]["AgentRequest"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        let request_doc_id = rows[0]["_docID"].as_str().unwrap().to_owned();
        let request = gents_session::request::admission::load_request_for_admission_test_as(
            &node,
            &request_doc_id,
            &created.agent_did,
        )
        .await
        .expect("load the fresh request through the admission representation");
        assert_eq!(request.agent_did, created.agent_did);
        let turns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider_inputs = Arc::new(std::sync::Mutex::new(Vec::new()));

        crate::runtime::agent::process_owned_request_with_model_for_test(
            node.clone(),
            runtime_agent.agent.clone(),
            surface.clone(),
            &runtime,
            RootReadModel {
                path,
                turns: turns.clone(),
                provider_inputs: provider_inputs.clone(),
            },
            request,
        )
        .await
        .expect("run the persisted request through the production owned loop");
        let escaped_request_doc = gents_store::graphql::escape_graphql_string(&request_doc_id);
        let observed = operator
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{_docID: {{_eq:"{escaped_request_doc}"}}}}) {{lifecycle_state failure_reason terminal_output}} }}"#
            ))
            .await;
        let data = &observed.expect("owned-loop observations")["data"];
        assert!(
            turns.load(std::sync::atomic::Ordering::SeqCst) >= 2,
            "{label}: provider did not receive the tool-result turn; request: {}",
            data["AgentRequest"][0]
        );
        assert!(
            provider_inputs
                .lock()
                .expect("provider inputs")
                .iter()
                .any(|input| input.contains(expected_result)),
            "{label}: provider did not receive the expected tool result/denial: {:?}",
            provider_inputs.lock().expect("provider inputs")
        );
        if let Some(forbidden_content) = forbidden_content {
            assert!(
                provider_inputs
                    .lock()
                    .expect("provider inputs")
                    .iter()
                    .all(|input| !input.contains(forbidden_content)),
                "{label}: denied file contents reached provider input: {:?}",
                provider_inputs.lock().expect("provider inputs")
            );
        }

        assert_eq!(
            data["AgentRequest"][0]["lifecycle_state"], "completed",
            "{label}: fresh owned request did not complete"
        );
    }
    let persisted_after_requests =
        crate::runtime::agent::document_view::load_document_runtime_view(&node, &owner)
            .await
            .expect("reload persisted Tools after fresh owned requests");
    assert_eq!(
        persisted_after_requests.tools[tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.root.as_deref()),
        Some(canonical_selected.to_string_lossy().as_ref()),
        "fresh request acceptance must not widen persisted/effective root"
    );
}

#[tokio::test]
async fn backend_self_config_protects_raw_keys_in_writes_reads_and_diffs() {
    for case in gents_lean_vocab::families::contracts_self_config_case_raw_key_scenario::cases() {
        raw_key_scenario(&case.input, |accepted| case.check(Some(accepted))).await;
    }
}

async fn raw_key_scenario(
    model: &gents_lean_vocab::families::contracts_self_config_case_raw_key_scenario::SelfConfigCaseInput,
    report: impl FnOnce(bool),
) {
    let fixture = gents_automation::test_support::self_config_fixture("self-config-secret").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_selected_backend_writer().await;
    let core = fixture.core();
    let raw = vec![(
        "auth".into(),
        Some(json!({"kind":"api_key","key":"do-not-expose"})),
    )];
    assert!(core.preview(backend_request(raw.clone())).await.is_err());
    assert!(core.apply(backend_request(raw)).await.is_err());
    let stored: Value = serde_json::from_str(
        &model
            .doc
            .iter()
            .find(|field| field.field == "auth")
            .unwrap()
            .value,
    )
    .unwrap();
    let raw_key: Value = serde_json::from_str(
        model
            .patch
            .iter()
            .find(|field| field.field == "auth")
            .unwrap()
            .value
            .as_deref()
            .unwrap(),
    )
    .unwrap();
    let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
        "node_did": fixture.node_did(), "backend_id": format!("{}:backend", fixture.agent_did()),
        "name": "Test inference", "provider_kind": "OpenAiCompatible",
        "endpoint": "http://127.0.0.1:1/v1", "auth": stored,
    }))
    .unwrap();
    let secret = gents_documents::backend_credential::BackendCredential {
        credential_id: stored["id"].as_str().unwrap().to_owned(),
        node_did: fixture.node_did().to_owned(),
        provider_kind: backend.provider_kind.as_str().to_owned(),
        endpoint: backend.endpoint.clone(),
        version: stored["version"].as_str().unwrap().to_owned(),
        secret: gents_documents::backend_credential::BackendCredentialSecret::ApiKey {
            key: raw_key["key"].as_str().unwrap().to_owned(),
        },
    };
    fixture
        .user_access()
        .transact_write("test.raw_key.credential", |txn| {
            let secret = &secret;
            Box::pin(async move {
                gents_documents::backend_credential::put_backend_credential_in_txn(txn, secret)
                    .await
            })
        })
        .await
        .unwrap();
    gents_documents::write_inference_backend_document(
        fixture.user_access(),
        &backend,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let previewed = core
        .preview(backend_request(vec![(
            "auth".into(),
            Some(raw_key.clone()),
        )]))
        .await
        .is_ok();
    let applied = core
        .apply(backend_request(vec![(
            "auth".into(),
            Some(raw_key.clone()),
        )]))
        .await
        .is_ok();
    assert_eq!(
        previewed, applied,
        "preview and apply decide the same patch"
    );
    assert!(core
        .apply(backend_request(vec![(
            "auth".into(),
            Some(json!({"kind":"api_key","key":"replacement-secret"}))
        )]))
        .await
        .is_err());
    let read = core
        .read_effective_config(&BTreeSet::new(), false, true)
        .await
        .unwrap();
    assert!(!read.to_string().contains("operator-secret"));
    let patch = vec![(
        "auth".into(),
        Some(json!({"kind":"environment","variable":"GENTS_TEST_KEY_REFERENCE"})),
    )];
    let preview = core.preview(backend_request(patch)).await.unwrap();
    assert!(!serde_json::to_string(&preview)
        .unwrap()
        .contains("operator-secret"));
    report(applied);
}

#[tokio::test]
async fn backend_choice_uses_runtime_visible_default_in_agent_transaction() {
    let fixture =
        gents_automation::test_support::self_config_fixture("account-default-choice").await;
    fixture.grant_node_reader().await;
    let owner = fixture.node_did();
    let credential = gents_inference::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: "model-default-credential".into(),
        node_did: owner.into(),
        provider: gents_inference::clients::xai_grok::XAI_OAUTH_PROVIDER.into(),
        access_token: "private-access".into(),
        refresh_token: "private-refresh".into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: Some("g2".into()),
        connected_at: Some(chrono::Utc::now()),
        provider_account_key: None,
        label: None,
    };
    gents_inference::auth::credential::upsert_oauth_credential_on(
        fixture.user_access(),
        &credential,
    )
    .await
    .unwrap();
    let model = gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .unwrap();
    let core = fixture.core();
    let access = core.access().unwrap();
    let mut disabled = false;
    for (index, case) in model.runtime_default_choice_cases.iter().enumerate() {
        if case["visible_default"].is_null() && !disabled {
            gents_inference::auth::credential::set_account_enabled(
                fixture.user_access(),
                owner,
                &credential.credential_id,
                false,
            )
            .await
            .unwrap();
            disabled = true;
        }
        let current_id = format!("guard-current-{index}");
        let next_id = format!("guard-next-{index}");
        for (id, kind, account) in [
            (&current_id, &case["current_kind"], &case["current_ref"]),
            (&next_id, &case["next_kind"], &case["next_ref"]),
        ] {
            let backend: gents_documents::InferenceBackend = serde_json::from_value(json!({
                "node_did": owner, "backend_id": id, "name": id,
                "provider_kind": kind, "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind":"node_oauth", "account_ref": account},
            }))
            .unwrap();
            gents_documents::write_inference_backend_document(
                fixture.user_access(),
                &backend,
                &gents_inference::resolved::ProviderProfilePolicy,
            )
            .await
            .unwrap();
        }
        let result = access
            .transact_readonly("test.account_choice.runtime_default", |txn| {
                let (current_id, next_id) = (&current_id, &next_id);
                Box::pin(async move {
                    gents_inference::operations::admit_backend_choice_in_txn(
                        txn,
                        owner,
                        Some(current_id),
                        next_id,
                    )
                    .await
                })
            })
            .await;
        assert_eq!(
            result.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{}: {result:?}",
            case["name"]
        );
        if let Err(error) = result {
            assert!(!format!("{error:#}").contains("g2"));
        }
    }
}

/// Lean `InferenceOperations.profileWriteDecision`: the account choice of a
/// profile edit is decided alike through the Agent that selects the profile
/// and by the profile's own ID. The cases are the ones this fixture's
/// backends stand in: the Agent's profile is on `chat-original`.

#[tokio::test]
async fn a_profile_edit_is_fenced_alike_through_its_agent_and_by_its_exact_id() {
    let (fixture, _core) = account_choice_core("profile-routes").await;
    let tools = fixture.tools(&["profile"]);
    let tool = tools
        .iter()
        .find(|tool| tool.name() == gents_protocol::tool_names::CONFIG_TOOL_NAME)
        .expect("config registered");
    let profile_id = format!("{}:inference", fixture.agent_did());
    let represented = [
        ("chat-original", "same", true),
        ("chat-b", "same_provider", false),
        ("grok-original", "other_provider", true),
    ];
    let family = &gents_lean_vocab::lean_contract_snapshot().families
        ["inference_operations_profile_write_decision"];
    let mut replayed = 0;
    for case in &family.cases {
        let input = &case.input;
        if input["actor"] != "agent" || input["next"] != "oauth_account" {
            continue;
        }
        let Some((backend, _, _)) = represented.iter().find(|(_, current, is_default)| {
            input["current"] == *current && input["is_default"] == *is_default
        }) else {
            continue;
        };
        let argv = match input["route"].as_str() {
            Some("anchored") => json!(["profile", "preview"]),
            Some("exact_id") => json!(["profile", "preview", "update", profile_id]),
            other => panic!("unknown route {other:?}"),
        };
        let text = tool
            .call(json!({"argv": argv, "set": {"backend_id": backend}}).to_string())
            .await
            .unwrap_or_else(|error| panic!("{input}: {error:#}"));
        let answer: Value = serde_json::from_str(&text).unwrap();
        let decision = answer.get("refusal").cloned().unwrap_or(json!("admitted"));
        assert_eq!(decision, case.expected, "{input}: {answer}");
        replayed += 1;
    }
    assert_eq!(replayed, 6);
}

#[tokio::test]
async fn modeled_job_policy_updates_follow_native_tools_authorization() {
    for case in gents_lean_vocab::families::contracts_self_config_job_guard_native::cases() {
        let candidate_integrations =
            serde_json::to_value(&case.input.candidate_integrations).expect("integrations encode");
        let stored_integrations = case
            .input
            .stored_integrations
            .as_ref()
            .map(|stored| serde_json::to_value(stored).expect("integrations encode"));
        let fixture = gents_automation::test_support::self_config_fixture(&case.input.name).await;
        let tools_id = format!("{}:tools", fixture.agent_did());
        let node_did = fixture.node_did().to_owned();
        fixture
            .update_signed_configuration(
                gents_store::collection::Collection::Tools,
                &tools_id,
                json!({"integrations": stored_integrations}),
            )
            .await;
        if case.input.native_authorized {
            fixture.grant_selected_configuration().await;
        }
        let (doc_id, before) = fixture
            .user_access()
            .transact_readonly("test.job_policy.tools_before", |txn| {
                let tools_id = &tools_id;
                let node_did = &node_did;
                Box::pin(async move {
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Tools,
                        node_did,
                        tools_id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("selected Tools exists");
        let actor = identity::Did::new(fixture.agent_did()).unwrap();
        let decisions = gents_store::config_client::ConfigAccess::local_as(
            fixture.db.node.clone(),
            identity::Did::new(&node_did).unwrap(),
        )
        .transact_readonly("test.job_policy.native_update", |txn| {
            let actor = actor.clone();
            let doc_id = doc_id.clone();
            Box::pin(async move {
                txn.observe_document_access(
                    &actor,
                    "Tools",
                    &[doc_id],
                    defra_node::DocumentAccessMode::Named {
                        permission: "update".into(),
                        node_permission: gents_store::config_client::NodePermission::DocumentUpdate,
                    },
                )
                .await
            })
        })
        .await
        .unwrap();
        assert_eq!(
            decisions == [defra_node::DocumentAccessDecision::Allowed],
            case.input.native_authorized,
            "{}: native Tools update",
            case.input.name
        );
        assert!(!fixture
            .db
            .node
            .nac_manager()
            .is_admin(&actor)
            .await
            .unwrap());
        let mut patch = vec![("integrations".into(), Some(candidate_integrations.clone()))];
        if case.input.add_schema_tool {
            let mut built_ins = before
                .get("built_ins")
                .filter(|value| value.is_object())
                .cloned()
                .unwrap_or_else(|| json!({}));
            assert_ne!(built_ins["enable_schema_tool"], true);
            built_ins["enable_schema_tool"] = json!(true);
            patch.push(("built_ins".into(), Some(built_ins)));
        }
        let core = fixture.core();
        let result = core.apply(tools_request(&core, patch)).await;
        if let Err(error) = &result {
            if !case.input.native_authorized {
                let denied = gents_protocol::error_chain::find_map_source(error, |cause| {
                    cause
                    .downcast_ref::<gents_store::config_client::ConfigClientError>()
                    .map(|error| {
                        error.is_permission_denied()
                            || error.is_missing_or_denied_write()
                            || matches!(error, gents_store::config_client::ConfigClientError::ConfigurationMutationNotApplied { .. })
                    })
                });
                assert_eq!(denied, Some(true), "{}: {error:#}", case.input.name);
            }
        }
        let (_, after) = fixture
            .user_access()
            .transact_readonly("test.job_policy.tools_after", |txn| {
                let tools_id = &tools_id;
                let node_did = &node_did;
                Box::pin(async move {
                    gents_documents::read_desired_state_record_in_txn(
                        txn,
                        gents_store::collection::Collection::Tools,
                        node_did,
                        tools_id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .expect("selected Tools remains visible");
        let expected = if result.is_ok() {
            &candidate_integrations["plugins"][0]["job"]["policy"]["dynamic"]
        } else {
            &before["integrations"]["plugins"][0]["job"]["policy"]["dynamic"]
        };
        assert_eq!(
            &after["integrations"]["plugins"][0]["job"]["policy"]["dynamic"], expected,
            "{}",
            case.input.name
        );
        if case.input.add_schema_tool {
            assert_eq!(
                after["built_ins"], before["built_ins"],
                "{}",
                case.input.name
            );
            assert!(!fixture
                .db
                .node
                .nac_manager()
                .check_permission(
                    &actor,
                    gents_store::config_client::NodePermission::CollectionPatch,
                )
                .await
                .unwrap());
        }
        case.check(result.is_ok());
        fixture.db.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn model_reselection_and_existing_sibling_grants_use_invoker_bounds() {
    let fixture =
        gents_automation::test_support::self_config_fixture("reselection-grant-bound").await;
    fixture.grant_selected_configuration().await;
    fixture.grant_agent_relation_delete().await;
    fixture.grant_node_reader().await;
    let sibling = fixture.install_sibling("sibling").await;
    let granted = fixture.install_sibling("granted").await;
    fixture.grant_sibling_configuration(&sibling).await;
    fixture.grant_pack_install_to(&granted).await;
    // Selecting the granted sibling's Context needs its selection management
    // and re-keys its pack-installation source, which the Engineer holds.
    fixture.grant_sibling_configuration(&granted).await;
    fixture.grant_engineer_tool_sources().await;
    let context = format!("{granted}:context");
    let core = SelfConfigCore::new(
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        sibling.clone(),
    )
    .unwrap()
    .with_lockout_agent_did(fixture.agent_did().to_owned())
    .with_agent_signer(fixture.agent.managed_agent_signer())
    .with_runtime_identity(Some(fixture.db.node_identity.clone()))
    .with_node_binding(Some(fixture.node_binding.clone()));
    for preview in [true, false] {
        let request = protect_working_agent(
            agent_request(&core, vec![("context_id".into(), Some(json!(context)))]),
            false,
            true,
        );
        let result = if preview {
            core.preview(request).await
        } else {
            core.apply(request).await
        };
        assert!(format!("{:#}", result.unwrap_err()).contains("cannot be self-granted"));
    }
    fixture.grant_pack_install().await;
    let holder = core.with_held_grants(ops::OperatorGrants {
        pack_install: true,
        policy: false,
        pack_authority: false,
    });
    holder
        .apply(protect_working_agent(
            agent_request(&holder, vec![("context_id".into(), Some(json!(context)))]),
            false,
            true,
        ))
        .await
        .unwrap();
    fixture
        .user_access
        .transact_write("test.grant_sibling_self_config", |txn| {
            let owner = fixture.node_did();
            let sibling = &sibling;
            Box::pin(async move {
                gents_documents::grant_selected_self_configuration(txn, owner, sibling).await
            })
        })
        .await
        .unwrap();
    let sibling_doc = gents_documents::load_agent(&fixture.db.node, fixture.node_did(), &sibling)
        .await
        .unwrap()
        .unwrap();
    let sibling_signer = gents_store::identity::resolve_agent_identity(
        fixture.db.node_identity.as_ref(),
        &sibling_doc.identity_binding(),
    )
    .unwrap();
    let no_grant = SelfConfigCore::new(
        fixture.db.node.clone(),
        fixture.node_did().to_owned(),
        sibling,
    )
    .unwrap()
    .with_agent_signer(sibling_signer.managed_agent_signer())
    .with_runtime_identity(Some(fixture.db.node_identity.clone()))
    .with_node_binding(Some(fixture.node_binding.clone()));
    // The own-only grant leaves the sibling with the Agent baseline and no NAC
    // relation operation, so the held-bounded rule refuses each new tool
    // source; an edit that projects no new source needs no relation authority.
    let household = &gents_lean_vocab::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration model");
    let own_edit = household
        .own_tool_edit_cases
        .iter()
        .find(|case| case["name"] == "own-only-goal")
        .expect("own-only goal edit model")["accepted"]
        .as_bool()
        .unwrap();
    let goal = no_grant
        .apply(tools_request(
            &no_grant,
            vec![("built_ins".into(), Some(json!({"enable_goal_tools":true})))],
        ))
        .await;
    assert_eq!(goal.is_ok(), own_edit, "{goal:?}");
    if let Err(error) = goal {
        assert!(
            format!("{error:#}").contains("source projection requires an authorized actor"),
            "{error:#}"
        );
    }
    // Self-config and pack install are already selected; preview adds no source.
    let preview = no_grant
        .apply(tools_request(
            &no_grant,
            vec![(
                "self_config".into(),
                Some(json!({"enable_self_config":true,"enable_pack_install":true,"self_config_preview":true})),
            )],
        ))
        .await;
    for case in gents_lean_vocab::families::household_tool_source_change_preview::cases() {
        assert_eq!(case.input.step, "preview", "unmodeled step");
        case.check(preview.is_ok());
    }
}

use gents_automation::test_support::self_config::*;

#[tokio::test]
async fn agent_management_clears_optional_fields_and_refuses_ignored_tool_modifiers() {
    let fixture = SelfConfigFixture::new("agent-clear").await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let access = fixture.user_access();
    let user = identity::Did::new(access.actor_did().unwrap()).unwrap();
    let binding = gents_store::identity::provision_agent_identity_authorized(
        node,
        fixture.db.node_identity.as_ref(),
        &user,
    )
    .await
    .unwrap();
    let profile = format!("{}:inference", fixture.agent_did());
    let created: Value = serde_json::from_str(
        &configure_agent(
            access,
            owner,
            &ConfigureAgentParams {
                action: "create".into(),
                identity_binding: Some(binding),
                display_name: StringUpdate::Set("Clearable".into()),
                description: StringUpdate::Set("Original description".into()),
                system_prompt: StringUpdate::Set("Original instructions".into()),
                profile_id: StringUpdate::Set(profile.clone()),
                ..Default::default()
            },
            &Default::default(),
        )
        .await
        .unwrap(),
    )
    .unwrap();
    let id = created["agent_did"].as_str().unwrap().to_owned();
    let edit = ConfigureAgentParams {
        action: "edit".into(),
        agent_did: Some(id.clone()),
        display_name: StringUpdate::Clear,
        description: StringUpdate::Clear,
        system_prompt: StringUpdate::Clear,
        ..Default::default()
    };
    configure_agent(access, owner, &edit, &Default::default())
        .await
        .unwrap();
    let stored = gents_documents::list_agents(node, owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == id)
        .unwrap();
    assert_eq!(stored.display_name, None);
    assert_eq!(stored.description, None);
    let context_id = gents_store::graphql::escape_graphql_string(
        stored
            .context_id
            .as_deref()
            .expect("created agent has a Context"),
    );
    let context = access.execute(&format!(
        r#"{{ AgentContext(filter: {{ context_id: {{ _eq: "{context_id}" }} }}) {{ system_prompt }} }}"#,
    )).await.unwrap();
    assert!(context["data"]["AgentContext"][0]["system_prompt"].is_null());
    assert_eq!(stored.inference_profile_id, profile);
    let invalid = ConfigureAgentParams {
        root: StringUpdate::Set("/ignored".into()),
        ..edit
    };
    let error = configure_agent(access, owner, &invalid, &Default::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("tools update"), "{error:#}");
    let after = gents_documents::list_agents(node, owner)
        .await
        .unwrap()
        .into_iter()
        .find(|agent| agent.agent_did == id)
        .unwrap();
    assert_eq!(stored, after);
}

#[tokio::test]
async fn public_agent_management_uses_provisioned_binding_and_client_access() {
    let fixture = SelfConfigFixture::new("public-agent-client").await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let access = fixture.user_access();
    let caller = identity::Did::new(access.actor_did().unwrap()).unwrap();
    let binding = gents_store::identity::provision_agent_identity_authorized(
        node,
        fixture.db.node_identity.as_ref(),
        &caller,
    )
    .await
    .unwrap();
    let mut args = ConfigureAgentParams {
        action: "preview".into(),
        operation: Some("create".into()),
        identity_binding: Some(binding.clone()),
        display_name: StringUpdate::Set("Public agent".into()),
        system_prompt: StringUpdate::Set("Complete the requested work.".into()),
        profile_id: StringUpdate::Set(format!("{}:inference", fixture.agent_did())),
        ..Default::default()
    };
    let preview: Value = serde_json::from_str(
        &configure_agent(access, owner, &args, &Default::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["admitted"], true);
    assert_eq!(preview["committed"], false);
    assert_eq!(
        gents_documents::list_agents(node, owner)
            .await
            .unwrap()
            .len(),
        1
    );
    args.action = "create".into();
    args.operation = None;
    args.identity_binding = None;
    let error = configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("runtime-provisioned identity binding"));
    args.identity_binding = Some(binding.clone());
    let committed: Value = serde_json::from_str(
        &configure_agent(access, owner, &args, &Default::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(committed["committed"], true);
    assert_eq!(committed["agent_did"], binding.agent_did);
    assert_eq!(committed["agent_did"], preview["agent_did"]);
    let stored = gents_documents::load_agent(node, owner, &binding.agent_did)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.identity_binding(), binding);
    let signer = gents_store::identity::resolve_agent_identity(
        fixture.db.node_identity.as_ref(),
        &stored.identity_binding(),
    )
    .unwrap();
    assert_eq!(signer.did(), stored.agent_did);
}

#[tokio::test]
async fn public_agent_edit_preserves_disabled_state_and_rejects_clone_options() {
    let fixture = SelfConfigFixture::new("disabled-agent-edit").await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let target = fixture.install_sibling("target").await;
    let access = fixture.user_access();
    let mut stored = gents_documents::load_agent(node, owner, &target)
        .await
        .unwrap()
        .unwrap();
    stored.enabled = false;
    gents_documents::write_agent_document(
        access,
        &stored,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let mut args = ConfigureAgentParams {
        action: "preview".into(),
        operation: Some("edit".into()),
        agent_did: Some(target.clone()),
        display_name: StringUpdate::Set("Edited while disabled".into()),
        ..Default::default()
    };
    let preview: Value = serde_json::from_str(
        &configure_agent(access, owner, &args, &Default::default())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(preview["committed"], false);
    assert_eq!(preview["agent"]["enabled"], false);
    assert_eq!(
        gents_documents::load_agent(node, owner, &target)
            .await
            .unwrap(),
        Some(stored.clone())
    );
    args.action = "edit".into();
    args.operation = None;
    args.clone_from = Some(target.clone());
    let error = configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("does not accept clone_from"));
    assert_eq!(
        gents_documents::load_agent(node, owner, &target)
            .await
            .unwrap(),
        Some(stored.clone())
    );
    args.clone_from = None;
    configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap();
    stored.display_name = Some("Edited while disabled".into());
    assert_eq!(
        gents_documents::load_agent(node, owner, &target)
            .await
            .unwrap(),
        Some(stored.clone())
    );
    args.profile_id = StringUpdate::Set("missing-profile".into());
    assert!(configure_agent(access, owner, &args, &Default::default())
        .await
        .is_err());
    assert_eq!(
        gents_documents::load_agent(node, owner, &target)
            .await
            .unwrap(),
        Some(stored.clone())
    );
    args.profile_id = StringUpdate::Omitted;
    args.make_default = true;
    assert!(configure_agent(access, owner, &args, &Default::default())
        .await
        .is_err());
    assert_eq!(
        gents_documents::load_agent(node, owner, &target)
            .await
            .unwrap(),
        Some(stored)
    );
}

#[tokio::test]
async fn public_prompt_edits_require_an_existing_unshared_context_without_partial_writes() {
    let fixture = SelfConfigFixture::new("prompt-context-ownership").await;
    fixture.grant_user_relation_delete().await;
    let node = &fixture.db.node;
    let owner = fixture.node_did();
    let missing_id = fixture.install_sibling("missing").await;
    let shared_id = fixture.install_sibling("shared").await;
    let sibling_id = fixture.install_sibling("sibling").await;
    let private_id = fixture.install_sibling("private").await;
    let access = fixture.user_access();
    let mut missing = gents_documents::load_agent(node, owner, &missing_id)
        .await
        .unwrap()
        .unwrap();
    missing.context_id = None;
    gents_documents::write_agent_document(
        access,
        &missing,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let shared = gents_documents::load_agent(node, owner, &shared_id)
        .await
        .unwrap()
        .unwrap();
    let mut sibling = gents_documents::load_agent(node, owner, &sibling_id)
        .await
        .unwrap()
        .unwrap();
    sibling.context_id = shared.context_id;
    gents_documents::write_agent_document(
        access,
        &sibling,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .unwrap();
    let query = "query { Agent(order: {agent_did: ASC}) { agent_did display_name context_id inference_profile_id enabled } AgentContext(order: {context_id: ASC}) { context_id display_name system_prompt tools_id } }";
    let before = access.execute(query).await.unwrap();
    for (target, expected) in [
        (missing_id.as_str(), "agent has no context"),
        (shared_id.as_str(), "unshared context"),
    ] {
        for preview in [true, false] {
            let args = ConfigureAgentParams {
                action: if preview { "preview" } else { "edit" }.into(),
                operation: preview.then(|| "edit".into()),
                agent_did: Some(target.into()),
                display_name: StringUpdate::Set("Must not be published".into()),
                system_prompt: StringUpdate::Set("Replacement instructions".into()),
                ..Default::default()
            };
            let error = configure_agent(access, owner, &args, &Default::default())
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert_eq!(access.execute(query).await.unwrap(), before);
        }
    }
    let private = gents_documents::load_agent(node, owner, &private_id)
        .await
        .unwrap()
        .unwrap();
    let args = ConfigureAgentParams {
        action: "edit".into(),
        agent_did: Some(private_id),
        system_prompt: StringUpdate::Set("Private replacement instructions".into()),
        ..Default::default()
    };
    configure_agent(access, owner, &args, &Default::default())
        .await
        .unwrap();
    let after = access.execute(query).await.unwrap();
    assert_eq!(after["data"]["Agent"], before["data"]["Agent"]);
    let mut expected_contexts = before["data"]["AgentContext"].clone();
    let changed = expected_contexts
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|context| context["context_id"].as_str() == private.context_id.as_deref())
        .unwrap();
    changed["system_prompt"] = json!("Private replacement instructions");
    assert_eq!(after["data"]["AgentContext"], expected_contexts);
}
