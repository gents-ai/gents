use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::documents::{AgentContext, InferenceProfile};
use gents::request::interrupt::interrupt_request_by_doc_id_with_access;
use gents::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use gents::session::load_canonical_message;
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::escape_graphql_string;
use gents::{
    default_inference_profile_id_for_agent, Collection, DocumentRuntimeOptions, Gents,
    ReasoningEffort, ToolCeiling,
};
use gents_protocol::message::{AssistantContent, Message, Text};
use gents_protocol::output::live::{
    project_live, LiveObservation, LiveTarget, LiveView, OwnerLiveness,
};
use gents_protocol::output::reconstruction::ObservedSegment;
use gents_protocol::output::{
    MessageRole, OutputOutcome, OutputSource, OutputWriter, StreamPayload, TerminalOutput,
};
use gents_protocol::rendered_request::{CaptureScope, CaptureScopeKind};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;

use crate::support::agent_requests::RequestActors;
use crate::support::builder_runtime::submit_request;
use crate::support::interrupt::{
    wait_for_inference_call_state, wait_for_request_lifecycle_state, wait_for_runtime_ready,
    BootedAgent,
};
use crate::support::live_inference::{bind_target, live_target, InferenceTarget};
use crate::support::test_db;
use crate::support::TestDb;

const LIVE_SAMPLING_ID: &str = "live-interrupt:live-sampling";
const LIVE_CONTEXT_ID: &str = "live-interrupt:context";

const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(300);
const TERMINAL_TIMEOUT: Duration = Duration::from_secs(30);
const OBSERVATION_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MIN_VISIBLE_PREFIX_CHARS: usize = 40;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_LIVE_OPENAI=1 and pass --ignored"]
async fn live_interrupt_mid_stream_on_openai_compatible() -> Result<()> {
    assert!(
        std::env::var("GENTS_LIVE_OPENAI").as_deref() == Ok("1"),
        "set GENTS_LIVE_OPENAI=1 and pass --ignored to run the live interrupt smoke test"
    );

    let target = live_target();

    let db = test_db("live-openai-interrupt").await;
    let (agent, actors) = boot_interrupt_agent(&db, &target).await?;
    let reader = &actors.fixture.access;

    let request_id = "req-live-openai-interrupt";
    let session_id = "session-live-openai-interrupt";
    let request_doc_id = submit_request(
        &actors,
        request_id,
        session_id,
        "interactive",
        "Write a long numbered list from 1 to 200. Use one short sentence per item. Start immediately and keep streaming.",
    )
    .await;

    let (observed_row, before_interrupt, provider_scopes) =
        wait_for_visible_text_prefix(reader, &request_doc_id, MIN_VISIBLE_PREFIX_CHARS).await?;
    let node_did = observed_row
        .node_did
        .clone()
        .expect("observed request row carries node_did");
    let execution_generation = observed_row
        .execution_generation
        .clone()
        .expect("observed request row carries its execution owner");
    assert_eq!(
        observed_row.session_id.as_deref(),
        Some(session_id),
        "observed request row must stay in the fixture session"
    );
    assert!(
        !provider_scopes.is_empty(),
        "a visible TEXT prefix requires at least one provider Inference source"
    );
    for scope in &provider_scopes {
        assert_eq!(
            scope.kind,
            CaptureScopeKind::Inference,
            "the observed provider source must be the visible Inference capture scope"
        );
        assert!(scope.seq >= 1, "provider capture scopes allocate from 1");
    }

    // The requesting Agent interrupts its own request.
    interrupt_request_by_doc_id_with_access(
        &actors.agent_access,
        &request_doc_id,
        &node_did,
        observed_row.requester_did.as_deref(),
    )
    .await
    .expect("the requester's interrupt should latch interrupt_requested_at");

    wait_for_request_lifecycle_state(reader, &request_doc_id, "interrupted").await;
    let call = wait_for_inference_call_state(reader, request_id, "cancelled").await;
    assert_eq!(call.failure_reason.as_deref(), Some("Cancelled"));

    let row = wait_for_interrupted_terminal_row(reader, &request_doc_id).await?;
    assert_eq!(
        row.execution_generation.as_deref(),
        Some(execution_generation.as_str()),
        "interrupted request must keep the execution owner observed before the interrupt"
    );
    let terminal_output = row
        .terminal_output
        .clone()
        .expect("interrupted terminal row carries terminal_output");
    assert_terminal_partial(
        reader,
        &row,
        &request_doc_id,
        &terminal_output,
        &before_interrupt,
    )
    .await?;

    tokio::time::sleep(Duration::from_millis(750)).await;
    let settled_row = fetch_request_row(reader, &request_doc_id).await?;
    assert_eq!(
        settled_row.lifecycle_state, row.lifecycle_state,
        "interrupted lifecycle must stay settled"
    );
    assert_eq!(
        settled_row.terminal_output, row.terminal_output,
        "terminal_output selection must stay settled after the interrupted lifecycle persists"
    );
    assert_eq!(
        settled_row.execution_generation, row.execution_generation,
        "execution owner must stay settled after the interrupted lifecycle persists"
    );
    assert_terminal_partial(
        reader,
        &settled_row,
        &request_doc_id,
        &terminal_output,
        &before_interrupt,
    )
    .await?;

    agent.shutdown().await;
    Ok(())
}

async fn assert_terminal_partial(
    reader: &ConfigAccess,
    row: &AgentRequestRow,
    request_doc_id: &str,
    terminal_output: &TerminalOutput,
    before_interrupt: &str,
) -> Result<()> {
    match terminal_output {
        TerminalOutput::Message { message_doc_id } => {
            let (header, native) = load_canonical_message(
                reader,
                message_doc_id,
                row.node_did
                    .as_deref()
                    .context("terminal row omitted node_did")?,
                row.requester_did.as_deref(),
            )
            .await
            .context("reconstructing interrupted partial assistant header")?;
            anyhow::ensure!(header.role == MessageRole::Assistant);
            anyhow::ensure!(header.request_doc_id.as_deref() == Some(request_doc_id));
            anyhow::ensure!(Some(header.node_did.as_str()) == row.node_did.as_deref());
            anyhow::ensure!(header.requester_did.as_deref() == row.requester_did.as_deref());
            anyhow::ensure!(Some(header.session_id.as_str()) == row.session_id.as_deref());
            anyhow::ensure!(header.outcome == OutputOutcome::Partial);
            let Message::Assistant { content, .. } = native else {
                anyhow::bail!("interrupted partial is not an assistant message")
            };
            let text = content
                .iter()
                .filter_map(|item| match item {
                    AssistantContent::Text(Text { text }) => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>();
            anyhow::ensure!(text.starts_with(before_interrupt));
        }
        TerminalOutput::NoMessage => {
            let retained = observe_request(reader, request_doc_id).await?;
            anyhow::ensure!(retained.row.terminal_output == Some(TerminalOutput::NoMessage));
            anyhow::ensure!(retained.text.starts_with(before_interrupt));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct CanonicalObservation {
    row: AgentRequestRow,
    text: String,
    provider_scopes: Vec<CaptureScope>,
}

async fn fetch_request_row(reader: &ConfigAccess, request_doc_id: &str) -> Result<AgentRequestRow> {
    let physical = escape_graphql_string(request_doc_id);
    let response = reader
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 2) {{
                _docID request_id node_did requester_did session_id lifecycle_state
                execution_generation terminal_output terminalized_at
            }} }}"#
        ))
        .await
        .context("canonical AgentRequest row query")?;
    let rows = response["data"]
        .get("AgentRequest")
        .and_then(Value::as_array)
        .context("AgentRequest row query omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "exact request doc id must resolve to exactly one row"
    );
    let row: AgentRequestRow =
        serde_json::from_value(rows[0].clone()).context("decoding canonical AgentRequest row")?;
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(request_doc_id),
        "request row crossed its physical identity"
    );
    Ok(row)
}

async fn observe_request(
    reader: &ConfigAccess,
    request_doc_id: &str,
) -> Result<CanonicalObservation> {
    let row = fetch_request_row(reader, request_doc_id).await?;
    let node_did = row
        .node_did
        .as_deref()
        .context("request row omitted node_did")?;
    let session_id = row
        .session_id
        .as_deref()
        .context("request row omitted session_id")?;

    let physical = escape_graphql_string(request_doc_id);
    let scope =
        gents::session::session_scope_filter(node_did, session_id, row.requester_did.as_deref());
    let response = reader
        .execute(&format!(
            r#"{{
                AgentOutputSegment(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}}}}){{{AGENT_OUTPUT_SEGMENT_FIELDS}}}
                AgentMessage(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}}}}){{{AGENT_MESSAGE_FIELDS}}}
            }}"#
        ))
        .await
        .context("canonical output facts query")?;
    let data = &response["data"];
    let segments = data
        .get("AgentOutputSegment")
        .and_then(Value::as_array)
        .context("canonical output facts query omitted segments")?
        .iter()
        .map(decode_output_segment_row)
        .collect::<Result<Vec<_>>>()?;
    let headers = data
        .get("AgentMessage")
        .and_then(Value::as_array)
        .context("canonical output facts query omitted headers")?
        .iter()
        .map(decode_transcript_message_row)
        .collect::<Result<Vec<_>>>()?;

    let observed = segments
        .iter()
        .map(|segment| ObservedSegment {
            doc_id: &segment.doc_id,
            segment: &segment.segment,
        })
        .collect::<Vec<_>>();
    let messages = headers
        .iter()
        .map(|header| (header.doc_id.as_str(), &header.message))
        .collect::<Vec<_>>();

    let mut coordinates: Vec<(OutputSource, OutputWriter)> = Vec::new();
    let mut provider_scopes: Vec<CaptureScope> = Vec::new();
    for segment in &segments {
        let (OutputSource::ProviderTurn { scope, .. }, writer) =
            (&segment.segment.source, &segment.segment.writer)
        else {
            continue;
        };
        if scope.kind != CaptureScopeKind::Inference {
            continue;
        }
        if !coordinates
            .iter()
            .any(|(source, _)| source == &segment.segment.source)
        {
            coordinates.push((segment.segment.source.clone(), writer.clone()));
            provider_scopes.push(*scope);
        }
    }

    let mut text = String::new();
    for (source, writer) in &coordinates {
        let view = project_live(&LiveObservation {
            request_doc_id,
            session_id,
            target: LiveTarget {
                request_doc_id,
                source,
                writer,
                message_id: None,
            },
            messages: &messages,
            node_did,
            requester_did: row.requester_did.as_deref(),
            records: &observed,
            denied_headers: &[],
            denied_segments: &[],
            dependency_denials: &[],
            owner: OwnerLiveness {
                current_request: row
                    .execution_generation
                    .as_deref()
                    .map(|generation| (request_doc_id, generation)),
                live_tools: Vec::new(),
            },
            request_terminal: row
                .lifecycle_state
                .is_some_and(RequestLifecycleState::is_terminal),
            terminal_selection: row.terminal_output.clone(),
        });
        match view {
            LiveView::Live { streams }
            | LiveView::Settling { streams }
            | LiveView::RetainedPartial { streams } => {
                for stream in streams {
                    if stream.declaration.payload == StreamPayload::Text {
                        text.push_str(&stream.text);
                    }
                }
            }
            LiveView::Loading | LiveView::Absent | LiveView::Retracted => {}
            LiveView::Published { .. } => {
                anyhow::bail!("provider Inference turn published while the live interrupt polls mid-stream output")
            }
            LiveView::Denied => anyhow::bail!("canonical live output is denied"),
            LiveView::Conflicted => anyhow::bail!("canonical live output is conflicted"),
            LiveView::Invalid => anyhow::bail!("canonical live output is invalid"),
        }
    }

    Ok(CanonicalObservation {
        row,
        text,
        provider_scopes,
    })
}

async fn wait_for_visible_text_prefix(
    reader: &ConfigAccess,
    request_doc_id: &str,
    min_len: usize,
) -> Result<(AgentRequestRow, String, Vec<CaptureScope>)> {
    let deadline = tokio::time::Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        let observation = observe_request(reader, request_doc_id).await?;
        match observation.row.lifecycle_state {
            Some(state) if state.is_terminal() => panic!(
                "request reached terminal lifecycle {state} before the interrupt latched; \
                 observation={observation:?}"
            ),
            Some(RequestLifecycleState::Processing)
                if observation.row.execution_generation.is_none() =>
            {
                panic!("processing request omitted its execution generation")
            }
            _ => {}
        }
        if observation.text.chars().count() >= min_len {
            return Ok((
                observation.row,
                observation.text,
                observation.provider_scopes,
            ));
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a visible canonical TEXT prefix of {min_len} chars on \
             request {request_doc_id}; last text={:?}",
            observation.text
        );
        tokio::time::sleep(OBSERVATION_POLL_INTERVAL).await;
    }
}

async fn wait_for_interrupted_terminal_row(
    reader: &ConfigAccess,
    request_doc_id: &str,
) -> Result<AgentRequestRow> {
    let deadline = tokio::time::Instant::now() + TERMINAL_TIMEOUT;
    loop {
        let row = fetch_request_row(reader, request_doc_id).await?;
        match row.lifecycle_state {
            Some(RequestLifecycleState::Interrupted) => {
                anyhow::ensure!(
                    row.terminal_output.is_some(),
                    "interrupted terminal request omitted its terminal_output selection"
                );
                anyhow::ensure!(
                    row.terminalized_at
                        .as_deref()
                        .is_some_and(|value| !value.is_empty()),
                    "interrupted terminal request omitted terminalized_at"
                );
                return Ok(row);
            }
            Some(state) if state.is_terminal() => {
                panic!("interrupted request settled into unexpected terminal lifecycle {state}")
            }
            _ => {}
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for interrupted lifecycle on request {request_doc_id}; \
             last lifecycle={:?}",
            row.lifecycle_state
        );
        tokio::time::sleep(OBSERVATION_POLL_INTERVAL).await;
    }
}

async fn boot_interrupt_agent(
    db: &TestDb,
    target: &InferenceTarget,
) -> Result<(BootedAgent, RequestActors)> {
    let actors = bind_target(db, target).await;
    configure_live_interrupt(&actors, target).await;

    let agent = Gents::from_default_agent_documents(
        db.node.clone(),
        Arc::clone(&db.node_identity),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;

    let node_did = agent.node_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(&db.node, &node_did).await;

    Ok((BootedAgent::new(shutdown_tx, handle, node_did), actors))
}

/// The node's client key gives the bound Agent a high-effort live sampling
/// profile and a streaming system prompt.
async fn configure_live_interrupt(actors: &RequestActors, target: &InferenceTarget) {
    use gents::store::config_client::{
        apply_desired_state_plan, read_desired_state_record_in_txn as read,
        DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let node_did = actors.node_did();
    let agent_did = actors.agent_did();
    let profile_id = default_inference_profile_id_for_agent(agent_did);
    let sampling = target.live_sampling(node_did, LIVE_SAMPLING_ID);
    let profile = InferenceProfile {
        profile_id: profile_id.clone(),
        sampling_id: Some(LIVE_SAMPLING_ID.to_string()),
        reasoning_effort: Some(ReasoningEffort::High),
        ..target.profile(node_did)
    };
    let context = AgentContext {
        context_id: LIVE_CONTEXT_ID.to_string(),
        node_did: node_did.to_string(),
        display_name: None,
        description: None,
        system_prompt: Some(
            "Answer the user's request directly. Keep streaming until the response is complete."
                .to_string(),
        ),
        tools_id: None,
        compaction_id: None,
        skill_ids: Vec::new(),
        tags: Vec::new(),
    };
    actors
        .fixture
        .access
        .transact_write("test.live_interrupt_fixture", |txn| {
            let sampling = &sampling;
            let profile = &profile;
            let context = &context;
            Box::pin(async move {
                let (_, agent) = read(txn, Collection::Agent, node_did, agent_did)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("the live Agent is missing"))?;
                let mut agent: gents::documents::Agent = serde_json::from_value(agent)?;
                agent.context_id = Some(LIVE_CONTEXT_ID.to_string());
                let plan = DesiredStateApplyPlan::new(
                    [
                        (
                            Collection::InferenceSampling,
                            serde_json::to_value(sampling)?,
                        ),
                        (Collection::InferenceProfile, serde_json::to_value(profile)?),
                        (Collection::AgentContext, serde_json::to_value(context)?),
                        (Collection::Agent, serde_json::to_value(agent)?),
                    ]
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await.map(|_| ())
            })
        })
        .await
        .expect("configure the live interrupt Agent as the node's client key");
}
