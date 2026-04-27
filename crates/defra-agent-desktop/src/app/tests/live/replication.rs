use super::*;
use defra_agent::{
    DefraSessionHook, DefraStreamWriter, FailurePolicy, RequestLifecycle, StreamWriter,
};

#[test]
fn desktop_app_p2p_replicates_chat_request_path_to_remote_core() -> Result<()> {
    let _live_guard = live_desktop_test_guard();
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        live_multi_server_core_options(),
    ))?;
    let remote_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote")),
        live_multi_server_core_options(),
    ))?;

    let remote_addr = runtime.block_on(async {
        let remote_addr =
            wait_for_connectable_iroh_addr(&remote_core, "request-path remote").await?;
        connect_peer_with_retry(
            &desktop_core,
            &remote_addr,
            remote_core.local_peer_id(),
            "request-path desktop -> remote",
        )
        .await?;
        set_replicator_with_retry(
            &desktop_core,
            &remote_addr,
            "request-path desktop -> remote replicator",
            vec![
                defra_agent_protocol::schemas::AGENT_CONVERSATION_NAME.to_string(),
                defra_agent_protocol::schemas::AGENT_SESSION_NAME.to_string(),
                defra_agent_protocol::schemas::AGENT_REQUEST_NAME.to_string(),
            ],
        )
        .await?;
        Ok::<_, anyhow::Error>(remote_addr)
    })?;

    let agent_did = format!("did:defra:p2p-repro-{}", uuid::Uuid::new_v4().simple());
    let conversation = runtime.block_on(desktop_core.create_conversation(&agent_did, None))?;
    let request = runtime.block_on(desktop_core.submit_request(
        &conversation.session_id,
        &agent_did,
        "replicate this request to the remote core",
        None,
    ))?;

    wait_for_value(
        "remote replicated AgentConversation",
        Duration::from_secs(60),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    &remote_core,
                    "AgentConversation",
                    "session_id",
                    &conversation.session_id,
                ))
                .ok()
                .filter(|has_row| *has_row)
                .map(|_| ())
        },
    )
    .with_context(|| format!("remote addr was {remote_addr}"))?;
    wait_for_value(
        "remote replicated AgentSession",
        Duration::from_secs(60),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    &remote_core,
                    "AgentSession",
                    "session_id",
                    &conversation.session_id,
                ))
                .ok()
                .filter(|has_row| *has_row)
                .map(|_| ())
        },
    )?;
    wait_for_value(
        "remote replicated AgentRequest",
        Duration::from_secs(60),
        || {
            runtime
                .block_on(query_has_row_by_unique_field(
                    &remote_core,
                    "AgentRequest",
                    "request_id",
                    &request.request_id,
                ))
                .ok()
                .filter(|has_row| *has_row)
                .map(|_| ())
        },
    )?;

    runtime.block_on(remote_core.shutdown())?;
    runtime.block_on(desktop_core.shutdown())?;
    Ok(())
}

#[test]
fn desktop_app_p2p_replicates_config_docs_to_multiple_remote_cores() -> Result<()> {
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        live_multi_server_core_options(),
    ))?;
    let alpha_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote-alpha")),
        live_multi_server_core_options(),
    ))?;
    let bravo_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote-bravo")),
        live_multi_server_core_options(),
    ))?;

    runtime.block_on(configure_live_test_replicators(
        &desktop_core,
        &alpha_core,
        "config alpha",
    ))?;
    runtime.block_on(configure_live_test_replicators(
        &desktop_core,
        &bravo_core,
        "config bravo",
    ))?;

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let alpha_agent_did = format!("did:defra:p2p-config-alpha-{suffix}");
    let bravo_agent_did = format!("did:defra:p2p-config-bravo-{suffix}");
    let alpha_docs =
        seed_desktop_origin_config_docs(&runtime, &desktop_core, "alpha", &alpha_agent_did)?;
    let bravo_docs =
        seed_desktop_origin_config_docs(&runtime, &desktop_core, "bravo", &bravo_agent_did)?;

    wait_for_remote_config_docs(
        &runtime,
        &alpha_core,
        "alpha remote owner config docs",
        &alpha_docs,
    )?;
    wait_for_remote_config_docs(
        &runtime,
        &bravo_core,
        "bravo remote owner config docs",
        &bravo_docs,
    )?;

    runtime.block_on(desktop_core.shutdown())?;
    runtime.block_on(alpha_core.shutdown())?;
    runtime.block_on(bravo_core.shutdown())?;
    Ok(())
}

fn seed_desktop_origin_config_docs(
    runtime: &tokio::runtime::Runtime,
    core: &ClientCore,
    label: &str,
    agent_did: &str,
) -> Result<LiveAgentDocs> {
    let behavior_id = format!("{agent_did}:default");
    let backend_id = format!("{label}-desktop-origin-backend");
    let tool_selection_id = format!("{behavior_id}:tools");
    let inference_profile_id = format!("{behavior_id}:profile");
    // `seed_desktop_origin_config_docs` only seeds the config documents
    // that the replication probe waits for (backend/profile/tools/
    // behavior); it does not write Task or Schedule rows. We still
    // produce stable Task/Schedule identifiers so callers depending on
    // `LiveAgentDocs` have the complete shape.
    let task_id = format!("{behavior_id}:task");
    let schedule_id = format!("{behavior_id}:schedule");

    runtime.block_on(async {
        core.save_backend(&InferenceBackendRow {
            backend_id: backend_id.clone(),
            name: Some(format!("{label} Desktop Origin Backend")),
            provider_kind: Some("openai-compatible".to_string()),
            endpoint: Some("http://127.0.0.1:65535/v1".to_string()),
            api_key: None,
            api_key_env_var: None,
            max_concurrent: Some(1),
            max_queue_depth: Some(10),
            enabled: Some(true),
            models: vec!["local-test-model".to_string()],
            last_probe: None,
            probe_status: Some("healthy".to_string()),
        })
        .await?;
        core.save_inference_profile(&InferenceProfileRow {
            profile_id: inference_profile_id.clone(),
            display_name: Some(format!("{label} Desktop Origin Profile")),
            context_window: Some(8192),
            max_output_tokens: Some(256),
            max_turns: Some(8),
            temperature: Some(0.0),
            stream_batch_ms: Some(50),
            deadline_duration_secs: Some(60),
        })
        .await?;
        core.save_tool_selection(&ToolSelectionRow {
            selection_id: tool_selection_id.clone(),
            agent_did: Some(agent_did.to_string()),
            display_name: Some(format!("{label} Desktop Origin Tools")),
            enable_file_tools: Some(true),
            file_tools_mode: Some("ReadOnly".to_string()),
            enable_bash: Some(false),
            bash_mode: Some("Off".to_string()),
            cli_tool_names: vec![],
            enable_meta_tools: Some(false),
            delegate_to: vec![],
        })
        .await?;
        core.save_behavior(&AgentBehaviorRow {
            behavior_id: behavior_id.clone(),
            agent_did: Some(agent_did.to_string()),
            display_name: Some(format!("{label} Desktop Origin Behavior")),
            system_prompt: Some(format!("{label} desktop-origin config replication probe")),
            backend_id: Some(backend_id.clone()),
            model_name: Some("local-test-model".to_string()),
            tool_selection_id: Some(tool_selection_id.clone()),
            inference_profile_id: Some(inference_profile_id.clone()),
            compaction_strategy: Some("StripThenSummarize".to_string()),
            compaction_threshold: Some(0.9),
            enabled: Some(true),
            created_at: Some(chrono::Utc::now().to_rfc3339()),
        })
        .await?;
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(LiveAgentDocs {
        behavior_id,
        backend_id,
        tool_selection_id,
        inference_profile_id,
        task_id,
        schedule_id,
    })
}

fn wait_for_remote_config_docs(
    runtime: &tokio::runtime::Runtime,
    remote_core: &ClientCore,
    label: &str,
    docs: &LiveAgentDocs,
) -> Result<()> {
    wait_for_value(label, Duration::from_secs(60), || {
        let has_rows = runtime
            .block_on(async {
                Ok::<_, anyhow::Error>(
                    query_has_row_by_unique_field(
                        remote_core,
                        "InferenceBackend",
                        "backend_id",
                        &docs.backend_id,
                    )
                    .await?
                        && query_has_row_by_unique_field(
                            remote_core,
                            "InferenceProfile",
                            "profile_id",
                            &docs.inference_profile_id,
                        )
                        .await?
                        && query_has_row_by_unique_field(
                            remote_core,
                            "ToolSelection",
                            "selection_id",
                            &docs.tool_selection_id,
                        )
                        .await?
                        && query_has_row_by_unique_field(
                            remote_core,
                            "AgentBehavior",
                            "behavior_id",
                            &docs.behavior_id,
                        )
                        .await?,
                )
            })
            .ok()?;
        has_rows.then_some(())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplicatedTurnSnapshot {
    request_status: Option<String>,
    request_lifecycle_state: Option<String>,
    response_status: Option<String>,
    response_progress_seq: Option<i64>,
    materialized_message_sequence: Option<i64>,
    response_content_len: usize,
    response_reasoning_len: usize,
    message_count: usize,
    tool_call_count: usize,
    completed_tool_call_count: usize,
    tool_result_count: usize,
    latest_request_id: Option<String>,
    conversation_status: Option<String>,
}

struct ProtocolTurnFixture {
    session_id: String,
    request_id: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct LifecycleRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    agent_did: String,
    #[serde(default)]
    behavior_id: Option<String>,
    session_id: String,
    content: String,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    top_p: Option<f64>,
    #[serde(default)]
    top_k: Option<i64>,
    #[serde(default)]
    max_tokens: Option<i64>,
    #[serde(default)]
    metadata: Option<String>,
    created_at: String,
}

#[test]
fn desktop_app_p2p_replicates_protocol_level_tool_heavy_turn_to_desktop_core() -> Result<()> {
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        live_multi_server_core_options(),
    ))?;
    let remote_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote")),
        live_multi_server_core_options(),
    ))?;

    runtime.block_on(configure_live_test_replicators(
        &desktop_core,
        &remote_core,
        "protocol-turn",
    ))?;

    let agent_did = format!(
        "did:defra:p2p-protocol-turn-{}",
        uuid::Uuid::new_v4().simple()
    );
    let fixture = runtime.block_on(write_protocol_level_tool_heavy_turn(
        &remote_core,
        "amy",
        &agent_did,
    ))?;

    let expected = runtime.block_on(fetch_replicated_turn_snapshot(
        &remote_core,
        &fixture.session_id,
        &fixture.request_id,
    ))?;

    wait_for_value(
        "desktop replicated protocol-level tool-heavy turn",
        Duration::from_secs(60),
        || {
            let desktop = runtime
                .block_on(fetch_replicated_turn_snapshot(
                    &desktop_core,
                    &fixture.session_id,
                    &fixture.request_id,
                ))
                .ok()?;
            (desktop == expected).then_some(desktop)
        },
    )?;

    let desktop = runtime.block_on(fetch_replicated_turn_snapshot(
        &desktop_core,
        &fixture.session_id,
        &fixture.request_id,
    ))?;
    assert_eq!(desktop, expected);

    runtime.block_on(remote_core.shutdown())?;
    runtime.block_on(desktop_core.shutdown())?;
    Ok(())
}

#[test]
#[ignore = "reproduces same-session followup replication/materialization stall"]
fn desktop_app_p2p_replicates_same_session_followup_tool_heavy_turn_to_desktop_core() -> Result<()>
{
    init_test_tracing();

    let runtime = test_runtime()?;
    let tempdir = tempfile::tempdir()?;
    let desktop_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("desktop")),
        live_multi_server_core_options(),
    ))?;
    let remote_core = runtime.block_on(ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path().join("remote")),
        live_multi_server_core_options(),
    ))?;

    runtime.block_on(configure_live_test_replicators(
        &desktop_core,
        &remote_core,
        "followup-turn",
    ))?;

    let agent_did = format!(
        "did:defra:p2p-followup-turn-{}",
        uuid::Uuid::new_v4().simple()
    );
    let first_turn = runtime.block_on(write_protocol_level_tool_heavy_turn(
        &remote_core,
        "amy",
        &agent_did,
    ))?;
    let first_expected = runtime.block_on(fetch_replicated_turn_snapshot(
        &remote_core,
        &first_turn.session_id,
        &first_turn.request_id,
    ))?;
    wait_for_value(
        "desktop replicated first same-session tool-heavy turn",
        Duration::from_secs(60),
        || {
            let desktop = runtime
                .block_on(fetch_replicated_turn_snapshot(
                    &desktop_core,
                    &first_turn.session_id,
                    &first_turn.request_id,
                ))
                .ok()?;
            (desktop == first_expected).then_some(desktop)
        },
    )?;

    let second_turn = runtime.block_on(write_followup_protocol_level_tool_heavy_turn(
        &remote_core,
        "amy",
        &agent_did,
        &first_turn.session_id,
        "Awesome breakdown, can you please tell me what you like about the architecture and point to files?",
    ))?;
    let second_expected = runtime.block_on(fetch_replicated_turn_snapshot(
        &remote_core,
        &second_turn.session_id,
        &second_turn.request_id,
    ))?;

    if let Err(error) = wait_for_value(
        "desktop replicated same-session followup tool-heavy turn",
        Duration::from_secs(60),
        || {
            let desktop = runtime
                .block_on(fetch_replicated_turn_snapshot(
                    &desktop_core,
                    &second_turn.session_id,
                    &second_turn.request_id,
                ))
                .ok()?;
            (desktop == second_expected).then_some(desktop)
        },
    ) {
        let desktop = runtime.block_on(fetch_replicated_turn_snapshot(
            &desktop_core,
            &second_turn.session_id,
            &second_turn.request_id,
        ))?;
        let remote = runtime.block_on(fetch_replicated_turn_snapshot(
            &remote_core,
            &second_turn.session_id,
            &second_turn.request_id,
        ))?;
        let desktop_health = desktop_core.p2p_health();
        let remote_health = remote_core.p2p_health();
        anyhow::bail!(
            "{error:#}; desktop_snapshot={desktop:?}; remote_snapshot={remote:?}; desktop_p2p_health={desktop_health:?}; remote_p2p_health={remote_health:?}"
        );
    }

    let desktop = runtime.block_on(fetch_replicated_turn_snapshot(
        &desktop_core,
        &second_turn.session_id,
        &second_turn.request_id,
    ))?;
    assert_eq!(desktop, second_expected);

    runtime.block_on(remote_core.shutdown())?;
    runtime.block_on(desktop_core.shutdown())?;
    Ok(())
}

async fn write_protocol_level_tool_heavy_turn(
    remote_core: &ClientCore,
    agent_name: &str,
    agent_did: &str,
) -> Result<ProtocolTurnFixture> {
    let prompt =
        "Hey amy can you tell me about the p2p communcation between the agent and the desktop in this app and the docuemnt based request model?";
    let backend_id = "protocol-turn-backend";
    let behavior_id = format!("{agent_did}:default");
    let node = remote_core.node_arc();

    let mut lifecycle = RequestLifecycle::materialize_claimed_with_execution_binding(
        node.clone(),
        agent_name,
        agent_did,
        prompt,
        180,
        defra_agent::lifecycle::ExecutionOrigin::Interactive,
        backend_id,
        defra_agent::lifecycle::TriggerLineage::default(),
    )
    .await?;
    lifecycle.begin_execution().await?;

    let session_id = lifecycle.request().session_id.clone();
    let request_id = lifecycle.request().request_id.clone();

    execute_tool_heavy_stream_for_request(
        node.clone(),
        agent_name,
        agent_did,
        &session_id,
        &request_id,
        &behavior_id,
        prompt,
        &mut lifecycle,
        64,
        3,
    )
    .await?;

    Ok(ProtocolTurnFixture {
        session_id,
        request_id,
    })
}

async fn write_followup_protocol_level_tool_heavy_turn(
    remote_core: &ClientCore,
    agent_name: &str,
    agent_did: &str,
    session_id: &str,
    prompt: &str,
) -> Result<ProtocolTurnFixture> {
    remote_core.refresh_store().await?;
    let submitted = remote_core
        .submit_request(session_id, agent_did, prompt, None)
        .await?;
    let request =
        load_agent_request_for_lifecycle(remote_core.node(), &submitted.request_id).await?;
    let behavior_id = submitted
        .behavior_id
        .clone()
        .unwrap_or_else(|| format!("{agent_did}:default"));
    let node = remote_core.node_arc();
    let mut lifecycle = RequestLifecycle::new_with_execution_binding(
        node.clone(),
        agent_name,
        agent_did,
        request,
        180,
        defra_agent::lifecycle::ExecutionOrigin::Interactive,
        "protocol-turn-backend",
    );
    match lifecycle.claim().await? {
        defra_agent::lifecycle::ClaimOutcome::Claimed => {}
        other => anyhow::bail!("expected followup request to claim cleanly, got {other:?}"),
    }
    lifecycle.begin_execution().await?;

    execute_tool_heavy_stream_for_request(
        node.clone(),
        agent_name,
        agent_did,
        session_id,
        &submitted.request_id,
        &behavior_id,
        prompt,
        &mut lifecycle,
        128,
        2,
    )
    .await?;

    Ok(ProtocolTurnFixture {
        session_id: session_id.to_string(),
        request_id: submitted.request_id,
    })
}

async fn execute_tool_heavy_stream_for_request(
    node: Arc<EmbeddedNode>,
    agent_name: &str,
    agent_did: &str,
    session_id: &str,
    request_id: &str,
    behavior_id: &str,
    prompt: &str,
    lifecycle: &mut RequestLifecycle,
    chunk_count: u32,
    tool_call_every: u32,
) -> Result<()> {
    let hook = DefraSessionHook::resume_with_identity_policy(
        node.clone(),
        session_id,
        agent_name,
        agent_did,
        FailurePolicy::FailClosed,
    )
    .await?;
    hook.set_active_request_id(Some(request_id.to_string()))
        .await;
    hook.persist_message(&Message::User {
        content: OneOrMany::one(UserContent::Text(Text {
            text: prompt.to_string(),
        })),
    })
    .await?;

    let writer = DefraStreamWriter::new(node.clone(), agent_did, Duration::ZERO);
    let response_doc_id = writer.begin(session_id, request_id, behavior_id).await?;
    lifecycle.set_response_doc_id(&response_doc_id);

    let mut response_text = String::new();
    let mut response_reasoning = String::new();
    for chunk_index in 0..chunk_count {
        let chunk = format!(
            "chunk-{chunk_index:02} The desktop mirrors the remote agent entirely over replicated branchable documents, and the P2P transport carries the request, streaming response, tool calls, and materialized transcript state together. "
        );
        response_text.push_str(&chunk);
        writer.write_tokens(&response_doc_id, &chunk).await?;

        if chunk_index % 8 == 0 {
            response_reasoning.push_str(&format!(
                "reason-{chunk_index:02} tracing replication edges and materialized transcript state. "
            ));
            writer
                .write_reasoning(
                    &response_doc_id,
                    &format!(
                        "reason-{chunk_index:02} tracing replication edges and materialized transcript state. "
                    ),
                )
                .await?;
            lifecycle.advance().await?;
        }

        if chunk_index % tool_call_every == 0 {
            persist_branchable_tool_call_round_trip(
                node.as_ref(),
                request_id,
                session_id,
                2,
                chunk_index / tool_call_every,
            )
            .await?;
            create_branchable_tool_result(
                node.as_ref(),
                agent_did,
                session_id,
                chunk_index / tool_call_every,
            )
            .await?;
        }
    }

    writer.flush_pending(&response_doc_id).await?;
    writer
        .finalize(
            &response_doc_id,
            defra_agent::streaming::StreamStatus::Complete,
        )
        .await?;
    lifecycle.complete().await?;

    let assistant_sequence = hook
        .persist_message(&Message::Assistant {
            id: Some(format!("assistant-{request_id}")),
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: response_text.clone(),
            })),
        })
        .await?;
    hook.mark_current_response_materialized(assistant_sequence)
        .await?;

    tracing::info!(
        session_id = %session_id,
        request_id = %request_id,
        response_len = response_text.len(),
        reasoning_len = response_reasoning.len(),
        "wrote protocol-level tool-heavy turn to remote core"
    );

    Ok(())
}

async fn load_agent_request_for_lifecycle(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<defra_agent::AgentRequest> {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                _docID
                request_id
                agent_did
                behavior_id
                session_id
                content
                temperature
                top_p
                top_k
                max_tokens
                metadata
                created_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("loading lifecycle request failed: {:?}", response.errors);
    }
    let row: LifecycleRequestRow = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .cloned()
        .with_context(|| format!("missing AgentRequest row for {request_id}"))
        .and_then(|value| {
            serde_json::from_value(value).context("decoding lifecycle request row")
        })?;

    Ok(defra_agent::AgentRequest {
        doc_id: row.doc_id,
        request_id: row.request_id,
        agent_did: row.agent_did,
        behavior_id: row.behavior_id,
        session_id: row.session_id,
        content: row.content,
        temperature: row.temperature,
        top_p: row.top_p,
        top_k: row.top_k,
        max_tokens: row.max_tokens,
        metadata: row.metadata,
        created_at: row.created_at,
    })
}

async fn fetch_replicated_turn_snapshot(
    core: &ClientCore,
    session_id: &str,
    request_id: &str,
) -> Result<ReplicatedTurnSnapshot> {
    core.refresh_store().await?;
    let snapshot = core.store().snapshot();
    let request = snapshot
        .request_row(request_id)
        .with_context(|| format!("missing request row for {request_id}"))?;
    let response = snapshot
        .latest_response_for_request(request_id)
        .with_context(|| format!("missing response row for {request_id}"))?;
    let conversation = snapshot
        .conversations
        .iter()
        .find(|row| row.session_id == session_id)
        .with_context(|| format!("missing conversation row for {session_id}"))?;
    let transcript = snapshot.transcript(session_id);
    let completed_tool_call_count = transcript
        .tool_calls
        .iter()
        .filter(|row| {
            row.completed_at
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
        .count();

    Ok(ReplicatedTurnSnapshot {
        request_status: request.status.clone(),
        request_lifecycle_state: request.lifecycle_state.clone(),
        response_status: response.status.clone(),
        response_progress_seq: response.progress_seq,
        materialized_message_sequence: response.materialized_message_sequence,
        response_content_len: response.content.as_deref().map_or(0, str::len),
        response_reasoning_len: response.reasoning.as_deref().map_or(0, str::len),
        message_count: transcript.messages.len(),
        tool_call_count: transcript.tool_calls.len(),
        completed_tool_call_count,
        tool_result_count: transcript.tool_results.len(),
        latest_request_id: conversation.latest_request_id.clone(),
        conversation_status: conversation.status.clone(),
    })
}

async fn persist_branchable_tool_call_round_trip(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    message_sequence: u32,
    tool_index: u32,
) -> Result<()> {
    let tool_call_id = format!("{request_id}-call-{tool_index:02}");
    let tool_call_key = format!("{session_id}:{tool_call_id}");
    let args = escape_graphql_string(&format!(
        "{{\"path\":\"crates/defra-agent-desktop/src/client/core/materialization.rs\",\"tool_index\":{tool_index}}}"
    ));
    let result = escape_graphql_string(&format!(
        "tool-result-{tool_index:02}: the desktop relies on replicated request, response, and transcript documents instead of an HTTP polling bridge."
    ));
    let now = chrono::Utc::now().to_rfc3339();
    let create_mutation = format!(
        r#"mutation {{
            upsert_AgentToolCall(
                filter: {{ tool_call_key: {{ _eq: "{tool_call_key}" }} }},
                add: {{
                    tool_call_key: "{tool_call_key}",
                    session_id: "{session_id}",
                    message_sequence: {message_sequence},
                    tool_name: "read_file",
                    tool_call_id: "{tool_call_id}",
                    args: "{args}",
                    result: "",
                    status: "called",
                    started_at: "{now}"
                }},
                update: {{
                    status: "called"
                }}
            ) {{ _docID }}
        }}"#
    );
    let create_response = node.execute(&create_mutation).await;
    if create_response.has_errors() {
        anyhow::bail!(
            "persisting tool call create failed: {:?}",
            create_response.errors
        );
    }

    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ tool_call_key: {{ _eq: "{tool_call_key}" }} }},
                limit: 1
            ) {{
                _docID
                started_at
            }}
        }}"#
    );
    let query_response = node.execute(&query).await;
    if query_response.has_errors() {
        anyhow::bail!(
            "loading tool call document failed: {:?}",
            query_response.errors
        );
    }
    let tool_call_row = query_response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .with_context(|| format!("missing AgentToolCall row for {tool_call_key}"))?;
    let doc_id = tool_call_row
        .get("_docID")
        .and_then(Value::as_str)
        .with_context(|| format!("missing AgentToolCall _docID for {tool_call_key}"))?;
    let started_at = tool_call_row
        .get("started_at")
        .and_then(Value::as_str)
        .with_context(|| format!("missing AgentToolCall started_at for {tool_call_key}"))?;
    let escaped_started_at = escape_graphql_string(started_at);

    let complete_mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                input: {{
                    started_at: "{escaped_started_at}",
                    result: "{result}",
                    status: "completed",
                    completed_at: "{now}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let complete_response = node.execute(&complete_mutation).await;
    if complete_response.has_errors() {
        anyhow::bail!(
            "persisting tool call completion failed: {:?}",
            complete_response.errors
        );
    }
    Ok(())
}

async fn create_branchable_tool_result(
    node: &EmbeddedNode,
    agent_did: &str,
    session_id: &str,
    tool_index: u32,
) -> Result<()> {
    let output_text = escape_graphql_string(&format!(
        "tool-output-{tool_index:02}: {}\n{}\n{}",
        "The document model keeps full tool output in replicated rows so the desktop can reconstruct state without talking to the remote over HTTP.".repeat(4),
        "This payload is intentionally chunky to stress pushlog delivery while the streaming AgentResponse is still receiving cumulative content updates.".repeat(4),
        "The local reproduction wants the remote to juggle response rewrites, tool call updates, and spill-style result documents in one turn.".repeat(4),
    ));
    let mutation = format!(
        r#"mutation {{
            create_AgentToolResult(input: {{
                agent_did: "{agent_did}",
                session_id: "{session_id}",
                tool_name: "read_file",
                tool_input: "materialization.rs",
                output_text: "{output_text}",
                truncated: true,
                truncation_metadata: "{{\"source\":\"protocol-replication-test\",\"tool_index\":{tool_index}}}",
                conversation_doc_id: "",
                created_at: "{}"
            }}) {{ _docID }}
        }}"#,
        chrono::Utc::now().to_rfc3339(),
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        anyhow::bail!("creating tool result failed: {:?}", response.errors);
    }
    Ok(())
}
