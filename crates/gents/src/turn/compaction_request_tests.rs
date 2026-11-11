use std::sync::Arc;

use gents_inference::compaction::test_fixtures::*;
use gents_inference::compaction::*;
use gents_inference::llm::message::{AssistantContent, Message, Text, ToolCall, UserContent};
use gents_session::session;
use gents_store::test_support::first_content;

use super::prompt::{LayeredPromptBuilder, PromptBuilder};

struct CompactionFixture {
    db: crate::support::TestDb,
    _user: gents_store::identity::KeyIdentity,
    agent: Arc<dyn gents_store::identity::NodeIdentity>,
    lifecycle: gents_session::request::lifecycle::RequestLifecycle,
}

impl CompactionFixture {
    async fn new(name: &str, request_id: &str, session_id: &str) -> Self {
        use gents_protocol::request_admission::{
            AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
        };
        use gents_store::identity::NodeIdentity;
        let (db, user, client, binding) = crate::test_support::root::signed_test_node(name).await;
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            db.node_identity.did(),
            user.did(),
            agent.did(),
            session_id,
            "compaction",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let mut lifecycle =
            gents_toolcalls::call::lifecycle::admission_fixture::pending_client_fixture_request(
                &db.node,
                &client,
                db.node_identity.clone(),
                &user,
                agent.as_ref(),
                create,
            )
            .await
            .unwrap();
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            gents_session::request::lifecycle::ClaimOutcome::Claimed
        );
        Self {
            db,
            _user: user,
            agent,
            lifecycle,
        }
    }

    fn writer(&self) -> gents_session::request::streaming::DefraStreamWriter {
        gents_session::request::streaming::DefraStreamWriter::new_for_lifecycle(
            self.db.node.clone(),
            &self.lifecycle,
            std::time::Duration::from_millis(1),
        )
        .unwrap()
    }

    fn access(&self) -> gents_store::config_client::ConfigAccess {
        gents_store::config_client::ConfigAccess::local_managed_agent(
            self.db.node.clone(),
            identity::Did::new(self.agent.did()).unwrap(),
            self.agent.as_ref(),
        )
        .unwrap()
    }
}

#[tokio::test]
async fn the_summarizer_and_its_fallback_arm_distinct_capture_scopes() {
    use gents_inference::rendered_request::scope::{
        armed_labels, scope_request, test_scope, CaptureScopeKind,
    };
    use gents_inference::rendered_request::RenderedRequestCaptureSink;

    let model = ScriptedSummaryModel::new(vec![
        ScriptedSummaryModel::malformed_summary_turn(),
        ScriptedSummaryModel::malformed_summary_turn(),
        ScriptedSummaryModel::malformed_summary_turn(),
        ScriptedSummaryModel::malformed_summary_turn(),
        ScriptedSummaryModel::summary_turn(),
    ]);
    let mut config = scheduled_origin_config();
    config.on_rendered_request = Some(gents_inference::rendered_request::scope::ambient_arming_sink(
        CaptureScopeKind::Compaction,
    ));
    let compactor = ProviderReductionEngine::new(std::sync::Arc::new(model), config);

    let mut fixture = CompactionFixture::new(
        "compaction-scopes",
        "compaction-scopes-request",
        "compaction-scopes-session",
    )
    .await;
    let writer = fixture.writer();
    fixture
        .lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    let lifecycle = &fixture.lifecycle;

    let sink: RenderedRequestCaptureSink = std::sync::Arc::new(|_| Box::pin(async { Ok(()) }));
    let mut scope = test_scope(
        gents_inference::rendered_request::context_for_claimed_request(
            lifecycle.request(),
            lifecycle.request_commit_cid().unwrap(),
            "model".to_string(),
            None,
        ),
        sink,
    );
    Arc::get_mut(&mut scope)
        .unwrap()
        .set_auxiliary_output_sink(writer.auxiliary_output_sink(
            lifecycle.request().clone(),
            lifecycle.execution_generation().unwrap().to_owned(),
            GROUPED_PROFILE,
        ));

    let labels = scope_request(scope, async move {
        compactor
            .reduce(
                summary_worthy_messages(),
                6_000,
                &ReductionOptions {
                    keep_recent_tokens: 50,
                    mode: ReductionMode::Summarize,
                    summary_max_output_tokens: 512,
                    ..Default::default()
                },
                test_reduction_admission(),
            )
            .await
            .expect("the strict JSON fallback recovers");
        armed_labels()
    })
    .await;

    assert_eq!(
        labels.first().map(String::as_str),
        Some("compaction.1"),
        "the guided summarizer must arm its own scope: {labels:?}"
    );
    assert_eq!(
        labels.last().map(String::as_str),
        Some("compaction_fallback.1"),
        "the strict JSON fallback is a second provider call and must be a second \
         fact, not a rebinding of the guided attempt's: {labels:?}"
    );
    assert!(
        labels.iter().all(|label| label != "inference.1"),
        "the summarizer must never borrow the inference loop's scope: {labels:?}"
    );
}

#[tokio::test]
async fn integration_compaction_persists_entry_and_prompt_builder_uses_it() {
    let mut fixture = CompactionFixture::new(
        "compaction-integration",
        "request-compaction-test",
        "session-1",
    )
    .await;
    let node = fixture.db.node.clone();
    let node_did = fixture.db.node_identity.did().to_owned();
    let requester_did = fixture.lifecycle.request().requester_did.clone().unwrap();
    let agent_did = fixture.lifecycle.request().agent_did.clone();
    let access = fixture.access();

    let model = MockSummaryModel::new(
        &serde_json::json!({
            "goal": "Continue inspecting the source files.",
            "completed_work": ["The agent repeatedly inspected the source files."],
            "key_decisions": ["Use compaction to collapse older turns"],
            "next_actions": []
        })
        .to_string(),
    );
    let config = gents_loop::loop_stream::LoopConfig {
        replay: gents_loop::loop_stream::LoopReplayInput::default(),
        provider_input_counter: std::sync::Arc::new(
            gents_loop::provider_input::ProviderInputCounter::new(
                gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible,
                gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
                "test-model",
            ),
        ),
        preamble: Some("You are a helpful coding agent.".to_string()),
        context_message: None,
        temperature: None,
        max_tokens: None,
        aggregate_token_budget: None,
        additional_params: None,
        structured_output: None,
        tool_choice: None,
        configuration_guard: None,
        on_rendered_request: None,
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: gents_inference::resolved::DEFAULT_CONTEXT_WINDOW,
        compaction_threshold: gents_inference::resolved::DEFAULT_COMPACTION_THRESHOLD,
        context_calibration: Default::default(),
        retry_policy: gents_loop::completion_retry::CompletionRetryPolicy::scheduled_default(),
        provider_idle_timeout: None,
        deadline: None,
        max_turns: 0,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
    };
    let compactor = ProviderReductionEngine::new(std::sync::Arc::new(model), config);

    let writer = fixture.writer();
    fixture
        .lifecycle
        .begin_owned_execution(&writer)
        .await
        .unwrap();
    let lifecycle = &fixture.lifecycle;
    let request_doc_id = lifecycle.request().doc_id.clone();

    for turn in 0..55u32 {
        let user = Message::User {
            content: vec![UserContent::Text(Text {
                text: format!("Request {turn}: {}", "x".repeat(800)),
            })],
        };
        writer
            .publish_authored_message(lifecycle, &format!("input:{turn}"), &user)
            .await
            .unwrap();

        let assistant_tool_call = Message::Assistant {
            request_id: None,
            id: Some("provider-message".into()),
            content: vec![AssistantContent::ToolCall(ToolCall {
                raw_arguments: None,
                id: "call-1".into(),
                call_id: Some("call-1".into()),
                function: gents_inference::llm::message::ToolFunction {
                    name: "read".to_string(),
                    arguments: serde_json::from_str(r#"{"file_path": "/workspace/main.rs"}"#)
                        .unwrap(),
                },
                signature: None,
                additional_params: None,
            })],
        };
        writer
            .start_provider_attempt(
                &request_doc_id,
                turn as usize,
                0,
                format!("inference.{}", turn + 1).parse().unwrap(),
            )
            .await;
        let mut published = writer
            .publish_native_turn(lifecycle, turn as usize, 0, &assistant_tool_call)
            .await
            .unwrap();
        let accepted = published
            .accepted_tools
            .pop()
            .expect("canonical publication accepted the tool call");
        let deadline = lifecycle
            .claimed_deadline_at()
            .expect("claimed request deadline");
        let mut tool = gents_toolcalls::call::lifecycle::ToolCallLifecycle::from_accepted(
            node.clone(),
            node_did.to_string(),
            Some(requester_did.clone()),
            accepted,
            deadline,
            gents_protocol::tool_call::AwaitMode::Foreground,
        )
        .unwrap();
        if turn != 0 {
            tool.start_running().await.unwrap();
            tool.complete(&"file contents\n".repeat(50)).await.unwrap();
        }

        let assistant = text_msg(
            "assistant",
            &format!("Response {turn}: {}", "y".repeat(500)),
        );
        writer
            .start_provider_attempt(
                &request_doc_id,
                turn as usize,
                1,
                format!("inference.{}", turn + 1).parse().unwrap(),
            )
            .await;
        writer
            .publish_native_turn(lifecycle, turn as usize, 1, &assistant)
            .await
            .unwrap();
    }

    let history =
        session::load_history_with_access(&access, "session-1", &node_did, Some(&requester_did))
            .await
            .unwrap();
    assert!(
        !safe_to_reduce(&history),
        "an older unresolved call requires provider repair"
    );
    assert!(
        session_prefix_is_stable(&history),
        "the final published ordinary turn closes the prefix"
    );
    let durable_before = history.clone();
    let (provider_history, _) = provider_view(GROUPED_PROFILE, history);
    let result = compactor
        .reduce(
            provider_history,
            2000,
            &ReductionOptions {
                keep_recent_tokens: 200,
                mode: ReductionMode::Summarize,
                ..Default::default()
            },
            test_reduction_admission(),
        )
        .await
        .unwrap();

    let reduction = required_reduction(&result);
    let sequence_rows = durable_before
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, message)| ((index + 1) as u32, message))
        .collect::<Vec<_>>();
    let compacted_through_sequence =
        session_cursor_for_reduction(GROUPED_PROFILE, &sequence_rows, 0, reduction)
            .unwrap()
            .expect("exact reduction must resolve to a durable cursor");
    let state = session::load_prompt_compaction_state_with_access(
        &access,
        "session-1",
        &node_did,
        Some(&requester_did),
        None,
    )
    .await
    .unwrap();
    session::save_exact_compaction_entry(
        &access,
        session::NewExactSessionCompaction {
            agent_did: &agent_did,
            session_id: "session-1",
            node_did: &node_did,
            requester_did: Some(&requester_did),
            request_id: "request-compaction-test",
            request_doc_id: &request_doc_id,
            files_read: &result.files_read,
            files_modified: &result.files_modified,
            compacted_through_sequence,
            original_tokens: result.original_token_estimate,
            compacted_tokens: result.compacted_token_estimate,
            expected_generation: &state.generation,
        },
        reduction,
    )
    .await
    .unwrap();

    let entries =
        session::load_compaction_entries(&access, "session-1", &node_did, Some(&requester_did))
            .await
            .unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].summary.contains("inspected the source files"));

    let resumed_history =
        session::load_history_with_access(&access, "session-1", &node_did, Some(&requester_did))
            .await
            .unwrap();
    assert_eq!(
        durable_before, resumed_history,
        "compaction must not mutate the durable transcript the timeline is built from"
    );

    let state = session::load_prompt_compaction_state_with_access(
        &access,
        "session-1",
        &node_did,
        Some(&requester_did),
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        state.compacted_through_sequence,
        Some(compacted_through_sequence)
    );
    let next_request_history = session::load_sequenced_history_projection_on(
        &access,
        "session-1",
        &node_did,
        Some(&requester_did),
        None,
        state.compacted_through_sequence,
        None,
    )
    .await
    .unwrap();
    assert!(next_request_history
        .iter()
        .all(|row| row.sequence > compacted_through_sequence));
    let (resumed_history, _) = provider_view(
        GROUPED_PROFILE,
        next_request_history
            .into_iter()
            .map(|row| row.message)
            .collect(),
    );
    assert_eq!(resumed_history, result.provider_messages().unwrap());

    let prompt_builder = LayeredPromptBuilder::for_agent(
        "Be helpful.",
        "general",
        &["list_files", "read_file", "bash"],
        true,
        &[],
    );
    let summaries = entries
        .iter()
        .map(|entry| entry.summary.clone())
        .collect::<Vec<_>>();
    let built = prompt_builder
        .build(
            &resumed_history
                .iter()
                .cloned()
                .map(gents_loop::loop_stream::TaggedMessage::unassociated)
                .collect::<Vec<_>>(),
            &summaries,
        )
        .await
        .unwrap();

    if let Message::User { content } = &built.messages[0].message {
        if let UserContent::Text(text) = first_content(content) {
            assert!(text.text.contains("inspected the source files"));
            assert!(text
                .text
                .contains("Continuation checkpoints from earlier conversation"));
        } else {
            panic!("expected summary reminder text");
        }
    } else {
        panic!("expected summary reminder");
    }

    assert_eq!(
        built.messages[1..]
            .iter()
            .map(|row| &row.message)
            .collect::<Vec<_>>(),
        resumed_history.iter().collect::<Vec<_>>()
    );
}
