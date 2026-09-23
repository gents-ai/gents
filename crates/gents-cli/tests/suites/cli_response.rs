use anyhow::{Context, Result};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload, ReasoningPart, SegmentRun,
    SourceClose, StreamDeclaration, StreamPayload, TerminalOutput, TranscriptMessage,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use gents_protocol::request_lifecycle::RequestLifecycleState;

use crate::support::graphql::graphql_mutation_with_variables;
use crate::support::*;

struct ResponseTestRuntime {
    _tempdir: tempfile::TempDir,
    home_dir: PathBuf,
    _mock_endpoint: MockModelEndpoint,
    _serve: ServeProcess,
    graphql: String,
    agent_did: String,
}

async fn start_response_runtime(label: &str) -> Result<ResponseTestRuntime> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-response-{label}-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let agent_name = format!("cli-response-{label}-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    Ok(ResponseTestRuntime {
        _tempdir: tempdir,
        home_dir,
        _mock_endpoint: mock_endpoint,
        _serve: serve,
        graphql,
        agent_did,
    })
}

struct MaterializedResponse<'a> {
    request_id: &'a str,
    session_id: &'a str,
    /// Text of the closed assistant Text stream; None means no text stream.
    message_content: Option<&'a str>,
    /// Text of the closed assistant Reasoning stream; empty means none.
    message_reasoning: &'a str,
    /// Request lifecycle state for the seeded terminal selection.
    lifecycle_state: RequestLifecycleState,
    /// `true` stamps the seeded header as the terminalization owner's
    /// selection; `false` leaves the terminal request with no selection yet
    /// (the canonical `Loading` observation).
    stamp_terminal_selection: bool,
}

/// Create the canonical request row for a fixture. Lifecycle is the only
/// request state; the terminal selection is stamped separately by the
/// terminalization owner.
async fn create_fixture_request(
    runtime: &ResponseTestRuntime,
    request_id: &str,
    session_id: &str,
    lifecycle_state: RequestLifecycleState,
) -> Result<String> {
    let now = chrono::Utc::now().to_rfc3339();
    let request = graphql_query(
        &runtime.graphql,
        &format!(
            r#"mutation{{create_AgentRequest(input:{{request_id:"{}",agent_did:"{}",requester_did:null,session_id:"{}",behavior_id:"response-test",content:"test request",created_at:"{}",lifecycle_state:"{}"}}){{_docID}}}}"#,
            escape_graphql_string(request_id),
            escape_graphql_string(&runtime.agent_did),
            escape_graphql_string(session_id),
            escape_graphql_string(&now),
            escape_graphql_string(lifecycle_state.as_str()),
        ),
    )
    .await?;
    gents_protocol::graphql::extract_mutation_doc_id(&request, "AgentRequest")
}

/// Stamp the terminalization owner's selection onto a terminal request so
/// `observe_request_output` resolves the referenced header and its segment
/// dependencies. The JSON column travels as a typed GraphQL variable,
/// preserving its object shape verbatim.
async fn stamp_terminal_message(
    runtime: &ResponseTestRuntime,
    request_doc_id: &str,
    message_doc_id: &str,
    lifecycle_state: RequestLifecycleState,
) -> Result<()> {
    use gents::config_client::ConfigAccess;

    let selection = serde_json::to_value(TerminalOutput::Message {
        message_doc_id: message_doc_id.to_owned(),
    })?;
    let now = chrono::Utc::now().to_rfc3339();
    graphql_mutation_with_variables(
            &ConfigAccess::Graphql(runtime.graphql.clone()),
            r#"mutation($request_doc_id: String!, $terminal_output: JSON, $lifecycle_state: String!, $now: String!) {
                update_AgentRequest(
                    filter: { _docID: { _eq: $request_doc_id } }
                    input: { terminal_output: $terminal_output, lifecycle_state: $lifecycle_state, terminalized_at: $now }
                ) { _docID }
            }"#,
            &serde_json::json!({
                "request_doc_id": request_doc_id,
                "terminal_output": selection,
                "lifecycle_state": lifecycle_state.as_str(),
                "now": now,
            }),
        )
        .await?;
    Ok(())
}

async fn insert_materialized_response(
    runtime: &ResponseTestRuntime,
    fixture: MaterializedResponse<'_>,
) -> Result<String> {
    use gents::config_client::ConfigAccess;
    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };

    let MaterializedResponse {
        request_id,
        session_id,
        message_content,
        message_reasoning,
        lifecycle_state,
        stamp_terminal_selection,
    } = fixture;
    let now = chrono::Utc::now().to_rfc3339();
    let access = ConfigAccess::Graphql(runtime.graphql.clone());

    // Canonical request row: lifecycle is the only request state; the terminal
    // selection (`TerminalOutput`) is stamped by the terminalization owner.
    let request_doc_id =
        create_fixture_request(runtime, request_id, session_id, lifecycle_state).await?;

    // Canonical closed source: one provider turn whose closing record carries
    // the text and reasoning streams. Payload bytes live only in the segment;
    // nothing here writes a second durable copy.
    let source = OutputSource::ProviderTurn {
        scope: CaptureScope {
            kind: CaptureScopeKind::Inference,
            seq: 1,
        },
        turn_index: 0,
        attempt: 0,
    };
    let text_stream = message_content.map(str::as_bytes);
    let reasoning_stream = (!message_reasoning.is_empty()).then(|| message_reasoning.as_bytes());
    let reasoning_stream_index = u32::from(text_stream.is_some());
    // One flush covers both streams; runs cover the payload exactly in
    // arrival order, with the opening run of each stream declaring it.
    let mut payload = String::new();
    let mut runs: Vec<SegmentRun> = Vec::new();
    if let Some(bytes) = text_stream {
        runs.push(SegmentRun {
            stream: 0,
            bytes: u32::try_from(bytes.len()).expect("fixture text stream fits u32"),
            declaration: Some(StreamDeclaration {
                block_index: u32::try_from(runs.len()).expect("fixture block index fits u32"),
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        });
        payload.push_str(message_content.expect("text stream implies content"));
    }
    if let Some(bytes) = reasoning_stream {
        runs.push(SegmentRun {
            stream: reasoning_stream_index,
            bytes: u32::try_from(bytes.len()).expect("fixture reasoning stream fits u32"),
            declaration: Some(StreamDeclaration {
                block_index: u32::try_from(runs.len()).expect("fixture block index fits u32"),
                part_index: 0,
                payload: StreamPayload::Reasoning,
            }),
        });
        payload.push_str(message_reasoning);
    }
    let has_data = !runs.is_empty();
    let stream_bytes = runs.iter().map(|run| u64::from(run.bytes)).collect();
    let segment = OutputSegment {
        agent_did: runtime.agent_did.clone(),
        requester_did: None,
        session_id: session_id.to_owned(),
        request_doc_id: request_doc_id.clone(),
        source,
        writer: OutputWriter::RequestExecution {
            execution_generation: "response-test-generation".into(),
        },
        ordinal: has_data.then_some(0),
        runs,
        payload,
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: u32::from(has_data),
            stream_bytes,
        }),
        created_at: now.clone(),
    };
    let segment_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_create_variables(&segment)?,
    )
    .await?;
    let close_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&segment_response, "AgentOutputSegment")?;

    // Canonical header: payloads are {close_doc_id, stream} references into the
    // closed source above; small structure stays inline.
    let mut blocks: Vec<MessageBlock> = Vec::new();
    if message_content.is_some() {
        blocks.push(MessageBlock::Text {
            text: PresentedPayload {
                output: PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: 0,
                },
                presentation: PayloadPresentation::Full,
            },
        });
    }
    if !message_reasoning.is_empty() {
        blocks.push(MessageBlock::Reasoning {
            id: None,
            parts: vec![ReasoningPart::Text {
                text: PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: reasoning_stream_index,
                },
                signature: None,
            }],
        });
    }
    let header = TranscriptMessage {
        message_key: gents::session::sequence_message_key(&runtime.agent_did, session_id, None, 1),
        session_id: session_id.to_owned(),
        agent_did: runtime.agent_did.clone(),
        requester_did: None,
        request_doc_id: Some(request_doc_id.clone()),
        publication: MessagePublication::RequestExecution {
            execution_generation: "response-test-generation".into(),
        },
        outcome: OutputOutcome::Complete,
        sequence: 1,
        role: MessageRole::Assistant,
        native_id: None,
        blocks,
        created_at: now.clone(),
    };
    let header_response = graphql_mutation_with_variables(
        &access,
        CREATE_AGENT_MESSAGE_MUTATION,
        &transcript_message_create_variables(&header)?,
    )
    .await?;
    let message_doc_id =
        gents_protocol::graphql::extract_mutation_doc_id(&header_response, "AgentMessage")?;

    // Stamp the terminalization owner's selection onto the terminal request so
    // `observe_request_output` resolves the exact header and its dependencies.
    if stamp_terminal_selection {
        stamp_terminal_message(runtime, &request_doc_id, &message_doc_id, lifecycle_state).await?;
    }
    Ok(message_doc_id)
}

fn response_show(runtime: &ResponseTestRuntime, request_id: &str) -> Result<Value> {
    run_cli_json(
        &runtime.home_dir,
        &[
            "response",
            "show",
            "--graphql",
            &runtime.graphql,
            request_id,
        ],
    )
}

fn response_wait(runtime: &ResponseTestRuntime, request_id: &str) -> Result<Value> {
    run_cli_json(
        &runtime.home_dir,
        &[
            "response",
            "wait",
            "--graphql",
            &runtime.graphql,
            "--timeout-secs",
            "5",
            "--poll-secs",
            "1",
            request_id,
        ],
    )
}

fn response_wait_failure(
    runtime: &ResponseTestRuntime,
    request_id: &str,
    timeout_secs: &str,
) -> Result<String> {
    run_cli_failure_stderr(
        &runtime.home_dir,
        &[
            "response",
            "wait",
            "--graphql",
            &runtime.graphql,
            "--timeout-secs",
            timeout_secs,
            "--poll-secs",
            "1",
            request_id,
        ],
    )
}

/// A terminal request with a selection the canonical reader rejects (header
/// scope mismatch, wrong role, or unreadable dependencies) surfaces the
/// `Invalid` observation: `show` prints the nonterminal `invalid` envelope and
/// `wait` rejects that observation before terminal materialization, without
/// timing out.
async fn assert_invalid_terminal_selection(
    runtime: &ResponseTestRuntime,
    request_id: &str,
) -> Result<()> {
    let shown = response_show(runtime, request_id)?;
    assert_eq!(
        shown.pointer("/output/kind").and_then(Value::as_str),
        Some("invalid"),
        "show must surface the invalid terminal selection as its own kind"
    );

    let wait_error = response_wait_failure(runtime, request_id, "5")?;
    assert!(
        wait_error.contains(&format!(
            "canonical output for request {request_id} is invalid"
        )),
        "wait must report the invalid canonical observation: {wait_error}"
    );
    assert!(wait_error.contains(request_id));
    assert!(!wait_error.contains("timed out"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_show_hydrates_materialized_content_like_response_wait() -> Result<()> {
    let runtime = start_response_runtime("hydrate").await?;
    let request_id = format!("response-hydrate-{}", Uuid::new_v4().simple());
    let session_id = format!("session-hydrate-{}", Uuid::new_v4().simple());
    let durable_content = format!("durable answer {}", Uuid::new_v4().simple());
    let durable_reasoning = format!("durable reasoning {}", Uuid::new_v4().simple());

    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &request_id,
            session_id: &session_id,
            message_content: Some(&durable_content),
            message_reasoning: &durable_reasoning,
            lifecycle_state: RequestLifecycleState::Completed,
            stamp_terminal_selection: true,
        },
    )
    .await?;

    let shown = response_show(&runtime, &request_id)?;
    let waited = run_cli_json(
        &runtime.home_dir,
        &[
            "response",
            "wait",
            "--graphql",
            &runtime.graphql,
            "--timeout-secs",
            "5",
            "--poll-secs",
            "1",
            &request_id,
        ],
    )?;
    let shown_envelope = &shown["output"];
    let waited_envelope = &waited["output"];
    assert_eq!(
        shown_envelope.get("kind").and_then(Value::as_str),
        Some("terminal_message")
    );
    // `response show` and `response wait` hydrate the same canonical terminal
    // message into an identical user-facing presentation.
    assert_eq!(shown_envelope, waited_envelope);
    let shown_presentation = &shown_envelope["presentation"];
    assert_eq!(
        shown_presentation
            .get("body_markdown")
            .and_then(Value::as_str),
        Some(durable_content.as_str())
    );
    assert_eq!(
        shown_presentation
            .get("reasoning_markdown")
            .and_then(Value::as_str),
        Some(durable_reasoning.as_str())
    );

    // The canonical request metadata travels in the same envelope.
    let shown_request = &shown["request"];
    assert_eq!(
        shown_request.get("request_id").and_then(Value::as_str),
        Some(request_id.as_str())
    );
    assert_eq!(
        shown_request.get("session_id").and_then(Value::as_str),
        Some(session_id.as_str())
    );
    assert_eq!(
        shown_request.get("lifecycle_state").and_then(Value::as_str),
        Some(RequestLifecycleState::Completed.as_str())
    );
    // The terminal selection points back at the seeded header, whose own
    // canonical identity matches the seeded request/session coordinates.
    assert_eq!(
        shown_envelope
            .pointer("/header/request_doc_id")
            .and_then(Value::as_str),
        shown_request.get("request_doc_id").and_then(Value::as_str),
        "header must be selected for the terminal request"
    );
    assert_eq!(
        shown_envelope
            .pointer("/header/session_id")
            .and_then(Value::as_str),
        Some(session_id.as_str())
    );
    assert_eq!(
        shown_envelope
            .pointer("/header/role")
            .and_then(Value::as_str),
        Some("assistant")
    );
    assert_eq!(
        shown_envelope
            .pointer("/header/outcome")
            .and_then(Value::as_str),
        Some("complete")
    );
    assert_eq!(
        shown_envelope
            .pointer("/message/role")
            .and_then(Value::as_str),
        Some("assistant")
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn response_show_diagnoses_missing_materialized_message() -> Result<()> {
    let runtime = start_response_runtime("missing").await?;

    // A terminal request with no selection yet is the canonical `Loading`
    // observation: `show` prints the nonterminal envelope and `wait` reports
    // the materialization timeout with the diagnostic hint. The seeded header
    // carries an empty text stream (an opening run of zero bytes); it is never
    // selected, so presentation never reads it.
    let loading_request_id = format!("response-loading-{}", Uuid::new_v4().simple());
    let loading_session_id = format!("session-loading-{}", Uuid::new_v4().simple());
    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &loading_request_id,
            session_id: &loading_session_id,
            message_content: Some(""),
            message_reasoning: "",
            lifecycle_state: RequestLifecycleState::Completed,
            stamp_terminal_selection: false,
        },
    )
    .await?;
    let loading = response_show(&runtime, &loading_request_id)?;
    assert_eq!(
        loading.pointer("/output/kind").and_then(Value::as_str),
        Some("loading"),
        "terminal request without a selection must observe loading"
    );
    assert_eq!(
        loading
            .pointer("/request/lifecycle_state")
            .and_then(Value::as_str),
        Some(RequestLifecycleState::Completed.as_str())
    );
    let loading_wait_error = response_wait_failure(&runtime, &loading_request_id, "1")?;
    assert!(
        loading_wait_error.contains("timed out waiting for materialized AgentMessage"),
        "wait must report the materialization timeout, got: {loading_wait_error}"
    );
    assert!(loading_wait_error.contains(&loading_request_id));

    // A text-only terminal header presents the text and leaves reasoning
    // absent (not empty).
    let text_request_id = format!("response-text-only-{}", Uuid::new_v4().simple());
    let text_session_id = format!("session-text-only-{}", Uuid::new_v4().simple());
    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &text_request_id,
            session_id: &text_session_id,
            message_content: Some("preserve this partial output"),
            message_reasoning: "",
            lifecycle_state: RequestLifecycleState::Completed,
            stamp_terminal_selection: true,
        },
    )
    .await?;
    let text_shown = response_show(&runtime, &text_request_id)?;
    assert_eq!(
        text_shown.pointer("/output/kind").and_then(Value::as_str),
        Some("terminal_message")
    );
    assert_eq!(
        text_shown
            .pointer("/output/presentation/body_markdown")
            .and_then(Value::as_str),
        Some("preserve this partial output")
    );
    assert!(
        text_shown
            .pointer("/output/presentation/reasoning_markdown")
            .is_none_or(Value::is_null),
        "text-only terminal output must not fabricate empty reasoning"
    );

    // A reasoning-only terminal header keeps the reasoning and presents an
    // empty body without inventing an unused text stream.
    let reasoning_request_id = format!("response-reasoning-{}", Uuid::new_v4().simple());
    let reasoning_session_id = format!("session-reasoning-{}", Uuid::new_v4().simple());
    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &reasoning_request_id,
            session_id: &reasoning_session_id,
            message_content: None,
            message_reasoning: "preserve this reasoning-only output",
            lifecycle_state: RequestLifecycleState::Completed,
            stamp_terminal_selection: true,
        },
    )
    .await?;
    let reasoning_wait = response_wait(&runtime, &reasoning_request_id)?;
    assert_eq!(
        reasoning_wait
            .pointer("/output/presentation/body_markdown")
            .and_then(Value::as_str),
        Some("")
    );
    assert_eq!(
        reasoning_wait
            .pointer("/output/presentation/reasoning_markdown")
            .and_then(Value::as_str),
        Some("preserve this reasoning-only output")
    );

    // A terminal request whose selection points at a header scoped to a
    // different request is rejected by the canonical reader (`Invalid`), never
    // presented as this request's answer.
    let scoped_request_id = format!("response-scoped-{}", Uuid::new_v4().simple());
    let scoped_session_id = format!("session-scoped-{}", Uuid::new_v4().simple());
    let scoped_message_doc_id = insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &scoped_request_id,
            session_id: &scoped_session_id,
            message_content: Some("belongs to the scoped request only"),
            message_reasoning: "",
            lifecycle_state: RequestLifecycleState::Completed,
            stamp_terminal_selection: true,
        },
    )
    .await?;
    let mismatched_request_id = format!("response-mismatched-{}", Uuid::new_v4().simple());
    let mismatched_session_id = format!("session-mismatched-{}", Uuid::new_v4().simple());
    let mismatched_doc_id = create_fixture_request(
        &runtime,
        &mismatched_request_id,
        &mismatched_session_id,
        RequestLifecycleState::Completed,
    )
    .await?;
    stamp_terminal_message(
        &runtime,
        &mismatched_doc_id,
        &scoped_message_doc_id,
        RequestLifecycleState::Completed,
    )
    .await?;
    assert_invalid_terminal_selection(&runtime, &mismatched_request_id).await?;

    // An interrupted terminal request keeps its lifecycle; with a completed
    // header the terminal selection still presents, while the request row
    // carries the interrupted lifecycle.
    let interrupted_request_id = format!("response-interrupted-{}", Uuid::new_v4().simple());
    let interrupted_session_id = format!("session-interrupted-{}", Uuid::new_v4().simple());
    insert_materialized_response(
        &runtime,
        MaterializedResponse {
            request_id: &interrupted_request_id,
            session_id: &interrupted_session_id,
            message_content: Some(""),
            message_reasoning: "",
            lifecycle_state: RequestLifecycleState::Interrupted,
            stamp_terminal_selection: true,
        },
    )
    .await?;
    let interrupted_show = response_show(&runtime, &interrupted_request_id)?;
    assert_eq!(
        interrupted_show
            .pointer("/request/lifecycle_state")
            .and_then(Value::as_str),
        Some(RequestLifecycleState::Interrupted.as_str())
    );
    assert_eq!(
        interrupted_show
            .pointer("/output/kind")
            .and_then(Value::as_str),
        Some("terminal_message")
    );
    assert!(interrupted_show
        .pointer("/output/presentation/body_markdown")
        .and_then(Value::as_str)
        .is_some_and(str::is_empty));
    let interrupted_wait = response_wait(&runtime, &interrupted_request_id)?;
    assert_eq!(
        interrupted_wait
            .pointer("/request/lifecycle_state")
            .and_then(Value::as_str),
        Some(RequestLifecycleState::Interrupted.as_str())
    );
    assert_eq!(
        interrupted_wait
            .pointer("/output/presentation/body_markdown")
            .and_then(Value::as_str),
        Some("")
    );

    Ok(())
}
