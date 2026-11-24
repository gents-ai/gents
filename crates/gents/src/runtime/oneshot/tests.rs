use super::*;
use gents_loop::test_support::scripted_provider::{Reply, ScriptedProvider, Step};
use rig::streaming::RawStreamingChoice;
use std::time::Duration;

async fn oneshot_fixture(
    name: &str,
) -> (
    gents_store::test_support::TestStore,
    gents_store::identity::KeyIdentity,
    gents_store::config_client::ConfigAccess,
    gents_store::config_client::ConfigAccess,
    gents_protocol::agent_identity::AgentIdentityBinding,
) {
    use gents_store::config_client::NodePermission;
    let signed_home = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store(name).await,
        name,
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, user, user_access) = signed_home.into_parts();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let admin = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        db.node_identity.did().parse().unwrap(),
    );
    let did = identity::Did::new(agent.did()).unwrap();
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
        NodePermission::DocumentDelete,
        NodePermission::DacRelationAdd,
        NodePermission::DacRelationDelete,
    ] {
        admin.grant_node_permission(&did, permission).await.unwrap();
    }
    let agent_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        db.node.clone(),
        did,
        agent.as_ref(),
    )
    .unwrap();
    (db, user, user_access, agent_access, binding)
}

/// Streams one text chunk, then empty chunks until `eof` is cancelled, then
/// ends without a terminal response.
fn partial_then_empty_provider(eof: tokio_util::sync::CancellationToken) -> ScriptedProvider {
    ScriptedProvider::responding(move |_| {
        Reply::Stream(vec![
            Step::Chunk(RawStreamingChoice::Message(
                "durable one-shot partial".into(),
            )),
            Step::Repeat {
                chunk: RawStreamingChoice::Message(String::new()),
                every: Duration::from_millis(10),
                until: Some(eof.clone()),
            },
        ])
    })
    .capturing_openai("scripted")
}

/// Empty chunks streamed after the partial text.
fn silent_deltas(model: &ScriptedProvider) -> usize {
    model
        .records()
        .iter()
        .map(|record| record.yielded.saturating_sub(1))
        .sum()
}

#[tokio::test]
async fn oneshot_renews_while_silent_then_preserves_partial_output_on_eof() {
    let (db, _user, _user_access, agent_access, binding) = oneshot_fixture("oneshot-lease").await;
    let node = db.node.clone();
    let mut behavior = crate::runtime::agent::PendingAgent::new("oneshot-lease")
        .with_identity_binding_for_test(binding)
        .build_with_identity_arc_for_test(db.node_identity.clone());
    Arc::make_mut(&mut behavior.agent).model_name = "scripted".to_owned();
    // Renewal comes due with half the lease left; that half is the margin for a
    // loaded host's poll delay and renewal commit, which a 1 s lease does not
    // cover (`tla/LeaseRenewalStarvation`, `MarginSpent`).
    Arc::make_mut(&mut behavior.agent).stream_liveness_timeout = Duration::from_secs(8);
    Arc::make_mut(&mut behavior.agent).deadline_duration = Duration::from_secs(60);
    let prompt = LayeredPromptBuilder::for_agent(
        &behavior.agent.system_prompt,
        &behavior.agent.agent_did,
        &[],
        false,
        &[],
    );
    let config = loop_config(
        &behavior.agent,
        prompt.preamble().to_owned(),
        0,
        gents_inference::rendered_request::CaptureScopeKind::OneShot,
    );
    let eof = tokio_util::sync::CancellationToken::new();
    let model = partial_then_empty_provider(eof.clone());
    let returned = tokio_util::sync::CancellationToken::new();
    let observe_renewal = async {
        let mut silent_lease = None;
        loop {
            tokio::select! {
                () = returned.cancelled() => return false,
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
            if silent_deltas(&model) == 0 {
                continue;
            }
            let snapshot = agent_access.execute("{ AgentRequest { lifecycle_state execution_generation execution_lease_expires_at } }").await.unwrap();
            let rows = snapshot["data"]["AgentRequest"].as_array().unwrap();
            let Some(row) = rows.first() else { continue };
            if row["lifecycle_state"] != "processing" {
                continue;
            }
            let generation = row["execution_generation"].as_str().unwrap();
            let expiry = chrono::DateTime::parse_from_rfc3339(
                row["execution_lease_expires_at"].as_str().unwrap(),
            )
            .unwrap();
            let (silent_generation, silent_expiry) =
                silent_lease.get_or_insert_with(|| (generation.to_owned(), expiry));
            assert_eq!(
                generation, silent_generation,
                "renewal keeps the owning generation"
            );
            if expiry > *silent_expiry {
                eof.cancel();
                return true;
            }
        }
    };
    let run = async {
        let result = run_oneshot_owned(
            node.clone(),
            &behavior.agent,
            OneshotInputs {
                tool_config: &behavior.tools,
                provider_family: None,
                prompt: "exercise explicit renewal during silence",
                prompt_builder: prompt,
                output_obligations: &[],
                tools: Arc::new(Vec::new()),
                background_tool_registry: BackgroundToolRegistry::default(),
                lsp_pool: gents_tools::native::lsp::LspPool::new(),
            },
            model.clone(),
            config,
        )
        .await;
        returned.cancel();
        result
    };
    let (result, renewed) = tokio::join!(run, observe_renewal);
    assert!(
        renewed,
        "one-shot must renew its lease while the provider is silent; it returned {result:?}"
    );
    assert!(
        result.is_err(),
        "a stream ending without an explicit terminal response cannot succeed"
    );
    assert!(
        silent_deltas(&model) > 0,
        "provider must send traffic after semantic progress stops; one-shot returned {result:?}"
    );
    let result = agent_access
        .execute(
            r#"{
        AgentRequest { _docID requester_did session_id lifecycle_state terminal_output }
        RenderedRequest { _docID requester_did session_id agent_did }
    }"#,
        )
        .await
        .unwrap();
    let data = &result["data"];
    assert_eq!(
        data["RenderedRequest"].as_array().unwrap().len(),
        1,
        "the fake provider request must be durably captured before streaming"
    );
    assert_eq!(data["AgentRequest"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["RenderedRequest"][0]["requester_did"],
        data["AgentRequest"][0]["requester_did"]
    );
    assert_eq!(
        data["RenderedRequest"][0]["session_id"],
        data["AgentRequest"][0]["session_id"]
    );
    assert_eq!(
        data["RenderedRequest"][0]["agent_did"],
        behavior.agent.agent_did
    );
    assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "failed");
    use gents_protocol::output::TerminalOutput;
    let request = &data["AgentRequest"][0];
    let doc_id = request["_docID"].as_str().unwrap();
    let selection: TerminalOutput =
        serde_json::from_value(request["terminal_output"].clone()).unwrap();
    let TerminalOutput::Message { message_doc_id } = selection else {
        panic!("durable text partial must be the request's exact terminal selection")
    };
    let requester_did = request["requester_did"].as_str();
    let (header, native) = gents_session::session::load_canonical_message(
        &agent_access,
        &message_doc_id,
        behavior.agent.node_did(),
        requester_did,
    )
    .await
    .unwrap();
    assert_eq!(header.request_doc_id.as_deref(), Some(doc_id));
    assert_eq!(
        header.outcome,
        gents_protocol::output::OutputOutcome::Partial
    );
    assert!(matches!(
        header.publication,
        gents_protocol::output::MessagePublication::RequestRecovery { .. }
    ));
    assert!(gents_protocol::transcript::present_message(&native)
        .body_markdown
        .contains("durable one-shot partial"));
    let repeated = RequestLifecycle::recover_all_with_identities(
        &node,
        behavior.agent.node_did(),
        &[behavior.agent.agent_identity.clone()],
    )
    .await
    .unwrap();
    assert_eq!(repeated.requests_recovered, 0);
    assert_eq!(repeated.responses_recovered, 0);
    node.shutdown().await.expect("node shutdown");
}

/// Finishes prematurely, then, once told the configured output obligation is
/// unmet while the request still processes, writes the one-shot result and
/// completes.
fn output_obligation_provider(
    access: gents_store::config_client::ConfigAccess,
) -> ScriptedProvider {
    ScriptedProvider::new([])
        .respond_async(move |dispatch| {
            let access = access.clone();
            async move {
                match dispatch.index {
                    0 => Reply::text("premature final"),
                    1 => {
                        let body = dispatch.body.expect("captured provider body");
                        assert!(body.to_string().contains("write_oneshot_result"));
                        assert!(body
                            .to_string()
                            .contains("configured output obligation is unmet"));
                        let state = access
                            .execute(
                                "{ AgentRequest { lifecycle_state } OneshotOutput { message } }",
                            )
                            .await
                            .unwrap();
                        let state = state["data"].clone();
                        assert_eq!(state["AgentRequest"][0]["lifecycle_state"], "processing");
                        assert!(state["OneshotOutput"].as_array().unwrap().is_empty());
                        Reply::turn(vec![
                            RawStreamingChoice::ToolCall(
                                rig::streaming::RawStreamingToolCall::new(
                                    "required-oneshot-write".into(),
                                    "write_oneshot_result".into(),
                                    serde_json::json!({"message": "durable output"}),
                                ),
                            ),
                            RawStreamingChoice::FinalResponse(()),
                        ])
                    }
                    2 => Reply::text("output complete"),
                    other => panic!("unexpected extra provider turn {other}"),
                }
            }
        })
        .capturing_openai("scripted")
}

#[tokio::test]
async fn oneshot_configured_output_gate_requires_real_write_and_respects_trigger_scope() {
    use gents_documents::{
        WriteToolDecl, WriteToolField, WriteToolOutputObligation, WriteToolOutputObligationScope,
    };
    use gents_tools::surface::{AgentToolSurfaceConfig, ToolCeiling, ToolRuntimeContext};

    for request_scoped in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let (db, _user, user_access, agent_access, binding) =
            oneshot_fixture("oneshot-output").await;
        let node = db.node.clone();
        node.add_collection("type OneshotOutput { message: String }")
            .await
            .unwrap();
        let mut behavior = crate::runtime::agent::PendingAgent::new("oneshot-output")
            .with_identity_binding_for_test(binding)
            .build_with_identity_arc_for_test(db.node_identity.clone());
        Arc::make_mut(&mut behavior.agent).model_name = "scripted".into();
        Arc::make_mut(&mut behavior.agent).stream_liveness_timeout = Duration::from_secs(60);
        Arc::make_mut(&mut behavior.agent).deadline_duration = Duration::from_secs(120);
        let declaration = WriteToolDecl {
            notification: None,
            tool_name: "write_oneshot_result".into(),
            collection: "OneshotOutput".into(),
            description: "Persist the required one-shot output.".into(),
            fields: vec![WriteToolField {
                name: "message".into(),
                required: true,
                fill: None,
            }],
            output_obligation: Some(WriteToolOutputObligation {
                scope: if request_scoped {
                    WriteToolOutputObligationScope::Request
                } else {
                    WriteToolOutputObligationScope::Trigger
                },
                minimum_writes: 1,
                expected_count_field: None,
            }),
        };
        let tool_doc: gents_documents::Tools = serde_json::from_value(serde_json::json!({
            "node_did":behavior.agent.node_did(), "tools_id":format!("{}:tools", behavior.agent.agent_did),
            "datastore":{"datastore_tool_surface_ids":["oneshot-surface"]},
        }))
        .unwrap();
        let surface_doc: gents_documents::DatastoreToolSurfaceDocument =
            serde_json::from_value(serde_json::json!({
                "node_did":behavior.agent.node_did(), "surface_id":"oneshot-surface",
                "entries":[gents_documents::SurfaceToolDecl::Create(declaration)],
            }))
            .unwrap();
        let documents = [
            (
                gents_store::collection::Collection::Tools,
                serde_json::to_value(&tool_doc).unwrap(),
            ),
            (
                gents_store::collection::Collection::DatastoreToolSurface,
                serde_json::to_value(&surface_doc).unwrap(),
            ),
        ]
        .into_iter()
        .map(
            |(collection, value)| gents_documents::DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            },
        )
        .collect();
        let plan = gents_documents::DesiredStateApplyPlan::new(documents).unwrap();
        user_access
            .transact_write("test.oneshot.select_surface", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        behavior.tools = Arc::new(
            AgentToolSurfaceConfig::from_tools_document_with_surfaces(
                &behavior.agent.agent_did,
                &tool_doc,
                &[surface_doc],
                &ToolCeiling::readwrite(dir.path()),
                Vec::new(),
            )
            .unwrap(),
        );
        let surface = behavior
            .tools
            .resolve(
                &node,
                behavior.agent.node_did(),
                &gents_tools::plugin_host::NoPluginHost,
            )
            .await
            .unwrap();
        let obligations = surface.output_obligations();
        assert_eq!(
            obligations.len(),
            1,
            "configured writer contract must survive surface resolution"
        );
        let runtime =
            ToolRuntimeContext::oneshot_with_node_did(node.clone(), behavior.agent.node_did())
                .for_agent(&behavior.agent);
        let tools = Arc::new(surface.build_tools(&runtime).await.unwrap());
        let prompt = LayeredPromptBuilder::new(&behavior.agent, &surface, &[]);
        let mut config = loop_config(
            &behavior.agent,
            prompt.preamble().to_owned(),
            tools.len(),
            gents_inference::rendered_request::CaptureScopeKind::OneShot,
        );
        config.max_turns = 4;
        let calls = output_obligation_provider(agent_access.clone());
        let result = run_oneshot_owned(
            node.clone(),
            &behavior.agent,
            OneshotInputs {
                tool_config: &behavior.tools,
                provider_family: None,
                prompt: "finish this request",
                prompt_builder: prompt,
                output_obligations: &obligations,
                tools,
                background_tool_registry: BackgroundToolRegistry::default(),
                lsp_pool: runtime.lsp_pool.clone(),
            },
            calls.clone(),
            config,
        )
        .await
        .unwrap();
        assert_eq!(calls.calls(), if request_scoped { 3 } else { 1 });
        assert_eq!(
            result.response_text,
            if request_scoped {
                "output complete"
            } else {
                "premature final"
            }
        );
        let persisted = agent_access.execute(r#"{
            AgentRequest { _docID lifecycle_state }
            OneshotOutput { message }
            AgentToolCall(filter: { tool_name: { _eq: "write_oneshot_result" } }) { lifecycle_state request_doc_id }
            RenderedRequest { _docID }
        }"#).await.unwrap();
        let data = &persisted["data"];
        assert_eq!(data["AgentRequest"].as_array().unwrap().len(), 1);
        assert_eq!(data["AgentRequest"][0]["lifecycle_state"], "completed");
        assert_eq!(
            data["OneshotOutput"].as_array().unwrap().len(),
            usize::from(request_scoped)
        );
        assert_eq!(
            data["AgentToolCall"].as_array().unwrap().len(),
            usize::from(request_scoped)
        );
        assert_eq!(
            data["RenderedRequest"].as_array().unwrap().len(),
            calls.calls()
        );
        if request_scoped {
            assert_eq!(data["OneshotOutput"][0]["message"], "durable output");
            assert_eq!(data["AgentToolCall"][0]["lifecycle_state"], "completed");
            assert_eq!(
                data["AgentToolCall"][0]["request_doc_id"],
                data["AgentRequest"][0]["_docID"]
            );
        }
        node.shutdown().await.expect("node shutdown");
    }
}

/// Runs `ls` through bash, then, once its real output reaches the provider,
/// finishes.
fn oneshot_bash_provider() -> ScriptedProvider {
    ScriptedProvider::responding(|dispatch| match dispatch.index {
        0 => Reply::turn(vec![
            RawStreamingChoice::ToolCall(rig::streaming::RawStreamingToolCall::new(
                "oneshot-bash".into(),
                "bash".into(),
                serde_json::json!({"command":"ls", "args":["."]}),
            )),
            RawStreamingChoice::FinalResponse(()),
        ]),
        1 => {
            let body = dispatch.body.as_ref().expect("captured provider body");
            assert!(
                body.to_string().contains("oneshot-visible.txt"),
                "one-shot provider must receive real bash output: {body}"
            );
            Reply::text("done")
        }
        other => panic!("unexpected one-shot turn {other}"),
    })
    .capturing_openai("scripted")
}

#[tokio::test]
async fn oneshot_bash_uses_owned_request_context() {
    let (db, _user, _user_access, agent_access, binding) = oneshot_fixture("oneshot-bash").await;
    let node = db.node.clone();
    let mut agent = crate::runtime::agent::PendingAgent::new("oneshot-bash")
        .with_identity_binding_for_test(binding)
        .build_with_identity_arc_for_test(db.node_identity.clone());
    Arc::make_mut(&mut agent.agent).model_name = "scripted".into();
    Arc::make_mut(&mut agent.agent).stream_liveness_timeout = Duration::from_secs(60);
    Arc::make_mut(&mut agent.agent).deadline_duration = Duration::from_secs(120);
    let root = tempfile::tempdir().unwrap();
    tokio::fs::write(root.path().join("oneshot-visible.txt"), "visible")
        .await
        .unwrap();
    let tools = Arc::new(
        gents_tools::native::ToolSet::builder()
            .read_root(root.path())
            .bash_read_only()
            .build()
            .build_native_tools()
            .unwrap(),
    );
    let prompt = LayeredPromptBuilder::for_agent(
        &agent.agent.system_prompt,
        &agent.agent.agent_did,
        &["bash"],
        false,
        &[],
    );
    let mut config = loop_config(
        &agent.agent,
        prompt.preamble().to_owned(),
        tools.len(),
        gents_inference::rendered_request::CaptureScopeKind::OneShot,
    );
    config.max_turns = 4;
    let runtime = ToolRuntimeContext::oneshot_with_node_did(node.clone(), agent.agent.node_did());
    let calls = oneshot_bash_provider();
    let result = run_oneshot_owned(
        node.clone(),
        &agent.agent,
        OneshotInputs {
            tool_config: &agent.tools,
            provider_family: None,
            prompt: "inspect the directory",
            prompt_builder: prompt,
            output_obligations: &[],
            tools,
            background_tool_registry: BackgroundToolRegistry::default(),
            lsp_pool: runtime.lsp_pool.clone(),
        },
        calls.clone(),
        config,
    )
    .await
    .unwrap();
    assert_eq!(result.response_text, "done");
    assert_eq!(calls.calls(), 2);
    let rows = agent_access
        .execute("{ AgentToolCall(filter: { tool_name: { _eq: \"bash\" } }) { lifecycle_state } }")
        .await
        .unwrap();
    assert_eq!(
        rows["data"]["AgentToolCall"][0]["lifecycle_state"],
        "completed"
    );
    node.shutdown().await.expect("node shutdown");
}
