//! Live end-to-end interrupt test against an OpenAI-compatible streaming backend.
//!
//! Normal test runs skip this file because it depends on a reachable live
//! inference service (the GLM-5.3 Flash deployment on workstation-1). To run
//! it locally:
//!
//! ```bash
//! GENTS_LIVE_OPENAI=1 \
//! GENTS_LIVE_OPENAI_ENDPOINT=http://workstation-1:8000/v1 \
//! GENTS_LIVE_OPENAI_MODEL=GLM-5.3-Flash-NVFP4 \
//! cargo test -p gents --test e2e_live -- live_interrupt_mid_stream_on_openai_compatible -- --ignored --nocapture
//! ```
//!
//! The backend fixture follows the typed `subagent_delegation_live.rs` pattern:
//! an actual agent-DID-scoped `InferenceBackend` (unauthenticated
//! OpenAI-compatible ChatCompletions endpoint) plus an `InferenceProfile`
//! selecting the model with high reasoning effort, an `InferenceSampling`
//! document (temperature 1.0, top_p 0.95), an `AgentContext`, and the
//! `AgentBehavior` the runtime request binds to — all applied through
//! `apply_fixture_documents` and booted via
//! `Gents::from_default_behavior_documents`. The canonical profile output
//! budget (32,768 tokens) is left in place so the stream is still mid-flight
//! when the interrupt is latched.
//!
//! Observation and assertions are canonical (#1571): the test polls the
//! visible provider Inference source through
//! `gents_protocol::output::live::project_live` under the request's exact
//! current execution owner until a TEXT (not reasoning) prefix is visible,
//! interrupts through the public physical request cancellation API
//! (`interrupt_request_by_doc_id`), waits for the interrupted lifecycle plus
//! the cancelled inference call, then reads the terminal request's
//! `terminal_output` selection. A selected message is reconstructed through
//! `gents::session::load_canonical_message_from_node`; `NoMessage` is checked
//! through the protocol-owned retained-partial view. No
//! `AgentResponse` rows are read anywhere in this file.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::document_config::{
    AgentBehavior, AgentContext, BackendAuth, InferenceBackend, InferenceProfile, InferenceSampling,
};
use gents::graphql::escape_graphql_string;
use gents::session::canonical_rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use gents::session::load_canonical_message_from_node;
use gents::{
    default_inference_profile_id_for_behavior, ensure_agent_principal, interrupt_request_by_doc_id,
    AgentIdentity, BackendProviderKind, Collection, DocumentRuntimeOptions, Gents, OpenAiWireApi,
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

use crate::support::fixtures::test_identity;
use crate::support::interrupt::{
    create_runtime_request, wait_for_inference_call_state, wait_for_request_lifecycle_state,
    wait_for_runtime_ready, BootedAgent,
};
use crate::support::test_db;
use crate::support::TestDb;

const DEFAULT_LIVE_ENDPOINT: &str = "http://workstation-1:8000/v1";
const DEFAULT_LIVE_MODEL: &str = "GLM-5.3-Flash-NVFP4";
const LIVE_BACKEND_ID: &str = "backend-live-openai-interrupt";
const LIVE_BEHAVIOR_ID: &str = "live-interrupt";
const LIVE_SAMPLING_ID: &str = "live-interrupt:live-sampling";
const LIVE_CONTEXT_ID: &str = "live-interrupt:context";

/// Live streams at human reading speed; a 40-char TEXT prefix appears long
/// before the 32,768-token canonical output budget is reached.
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

    let endpoint = std::env::var("GENTS_LIVE_OPENAI_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_LIVE_ENDPOINT.to_string());
    let model =
        std::env::var("GENTS_LIVE_OPENAI_MODEL").unwrap_or_else(|_| DEFAULT_LIVE_MODEL.to_string());

    let db = test_db("live-openai-interrupt").await;
    let agent = boot_live_agent(&db, &endpoint, &model).await?;

    let request_id = "req-live-openai-interrupt";
    let session_id = "session-live-openai-interrupt";
    let request_doc_id = create_runtime_request(
        db.node.as_ref(),
        agent.agent_did.as_str(),
        LIVE_BEHAVIOR_ID,
        request_id,
        session_id,
        "Write a long numbered list from 1 to 200. Use one short sentence per item. Start immediately and keep streaming.",
    )
    .await;

    // Observe canonically: poll the visible provider Inference source through
    // the protocol live projection with the request's exact current execution
    // owner until a TEXT (not reasoning) prefix is visible. Any conflicted or
    // invalid canonical state fails immediately inside the observation.
    let (observed_row, before_interrupt, provider_scopes) =
        wait_for_visible_text_prefix(db.node.as_ref(), &request_doc_id, MIN_VISIBLE_PREFIX_CHARS)
            .await?;
    let agent_did = observed_row
        .agent_did
        .clone()
        .expect("observed request row carries agent_did");
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

    // Physical request cancellation through the public interrupt owner, bound
    // to the exact physical request and principal scope observed above.
    interrupt_request_by_doc_id(
        db.node.as_ref(),
        &request_doc_id,
        &agent_did,
        observed_row.requester_did.as_deref(),
    )
    .await
    .expect("interrupt_request_by_doc_id should latch interrupt_requested_at");

    wait_for_request_lifecycle_state(db.node.as_ref(), &request_doc_id, "interrupted").await;
    let call = wait_for_inference_call_state(db.node.as_ref(), request_id, "cancelled").await;
    assert_eq!(call.failure_reason.as_deref(), Some("Cancelled"));

    // The interrupted terminal row must keep the exact execution owner observed
    // before the interrupt. It may publish a partial header or retain the
    // partial source with the canonical `NoMessage` selection.
    let row = wait_for_interrupted_terminal_row(db.node.as_ref(), &request_doc_id).await?;
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
        db.node.as_ref(),
        &row,
        &request_doc_id,
        &terminal_output,
        &before_interrupt,
    )
    .await?;

    // Stability: after the interrupted lifecycle is persisted, the terminal
    // selection and its exact reconstruction never move.
    tokio::time::sleep(Duration::from_millis(750)).await;
    let settled_row = fetch_request_row(db.node.as_ref(), &request_doc_id).await?;
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
        db.node.as_ref(),
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
    node: &EmbeddedNode,
    row: &AgentRequestRow,
    request_doc_id: &str,
    terminal_output: &TerminalOutput,
    before_interrupt: &str,
) -> Result<()> {
    match terminal_output {
        TerminalOutput::Message { message_doc_id } => {
            let (header, native) = load_canonical_message_from_node(
                node,
                message_doc_id,
                row.agent_did
                    .as_deref()
                    .context("terminal row omitted agent_did")?,
                row.requester_did.as_deref(),
            )
            .await
            .context("reconstructing interrupted partial assistant header")?;
            anyhow::ensure!(header.role == MessageRole::Assistant);
            anyhow::ensure!(header.request_doc_id.as_deref() == Some(request_doc_id));
            anyhow::ensure!(Some(header.agent_did.as_str()) == row.agent_did.as_deref());
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
            let retained = observe_request(node, request_doc_id).await?;
            anyhow::ensure!(retained.row.terminal_output == Some(TerminalOutput::NoMessage));
            anyhow::ensure!(retained.text.starts_with(before_interrupt));
        }
    }
    Ok(())
}

/// One canonical observation round: the exact physical request row plus the
/// TEXT body projected from the request's visible provider Inference sources.
#[derive(Debug)]
struct CanonicalObservation {
    row: AgentRequestRow,
    /// Concatenated TEXT (never reasoning) stream bytes from the live
    /// projection of every visible provider Inference source.
    text: String,
    /// The capture scopes of the observed provider Inference coordinates.
    provider_scopes: Vec<CaptureScope>,
}

async fn fetch_request_row(node: &EmbeddedNode, request_doc_id: &str) -> Result<AgentRequestRow> {
    let physical = escape_graphql_string(request_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }} }}, limit: 2) {{
                _docID request_id agent_did requester_did session_id lifecycle_state
                execution_generation terminal_output terminalized_at
            }} }}"#
        ))
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "canonical AgentRequest row query failed: {:?}",
        response.errors
    );
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
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

/// Read the exact request row plus its canonical output facts and project the
/// visible provider Inference sources through the protocol live owner. Every
/// conflicted/invalid/denied classification fails instead of retrying.
async fn observe_request(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<CanonicalObservation> {
    let row = fetch_request_row(node, request_doc_id).await?;
    let agent_did = row
        .agent_did
        .as_deref()
        .context("request row omitted agent_did")?;
    let session_id = row
        .session_id
        .as_deref()
        .context("request row omitted session_id")?;

    let physical = escape_graphql_string(request_doc_id);
    let scope =
        gents::session::session_scope_filter(agent_did, session_id, row.requester_did.as_deref());
    let response = node
        .execute(&format!(
            r#"{{
                AgentOutputSegment(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}}}}){{{AGENT_OUTPUT_SEGMENT_FIELDS}}}
                AgentMessage(filter:{{{scope},request_doc_id:{{_eq:"{physical}"}}}}){{{AGENT_MESSAGE_FIELDS}}}
            }}"#
        ))
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "canonical output facts query failed: {:?}",
        response.errors
    );
    let data = response
        .data
        .as_ref()
        .context("canonical output facts query omitted data")?;
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

    // The unique provider Inference coordinates visible in this request. Other
    // capture scopes (title generation, compaction) are not the observed
    // source and are ignored, exactly like the shared CLI projection.
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
            agent_did,
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
            // Not reconstructable yet, or a superseded/retracted attempt:
            // keep polling; the next round observes the current owner.
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

/// Poll the canonical observation until a TEXT (not reasoning) prefix of at
/// least `min_len` chars is visible under the request's exact current owner.
async fn wait_for_visible_text_prefix(
    node: &EmbeddedNode,
    request_doc_id: &str,
    min_len: usize,
) -> Result<(AgentRequestRow, String, Vec<CaptureScope>)> {
    let deadline = tokio::time::Instant::now() + OBSERVATION_TIMEOUT;
    loop {
        let observation = observe_request(node, request_doc_id).await?;
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

/// Poll for the interrupted terminal row with its terminalization facts.
async fn wait_for_interrupted_terminal_row(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<AgentRequestRow> {
    let deadline = tokio::time::Instant::now() + TERMINAL_TIMEOUT;
    loop {
        let row = fetch_request_row(node, request_doc_id).await?;
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

async fn boot_live_agent(db: &TestDb, endpoint: &str, model: &str) -> Result<BootedAgent> {
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("live-openai-interrupt"));
    let agent_did = identity.did().to_string();

    // Install the actual agent-DID-scoped live fixture documents the runtime
    // reconciler resolves: backend, profile, sampling, context, behavior, and
    // the principal selecting the behavior as default.
    upsert_live_backend(db.node.as_ref(), &agent_did, endpoint, model).await;

    let agent = Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        DocumentRuntimeOptions {
            tool_ceiling: ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await?;

    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;

    Ok(BootedAgent::new(shutdown_tx, handle, agent_did))
}

/// Install the typed live backend fixture documents (the
/// `subagent_delegation_live.rs` pattern): an actual agent-DID-scoped
/// `InferenceBackend` plus the profile/sampling/context/behavior documents the
/// default behavior resolves through, with the principal's
/// `default_behavior_id` bound to `LIVE_BEHAVIOR_ID`. Backend auth is
/// unauthenticated (local vLLM); the profile selects the model with high
/// reasoning effort.
async fn upsert_live_backend(node: &EmbeddedNode, agent_did: &str, endpoint: &str, model: &str) {
    let mut principal = ensure_agent_principal(node, agent_did)
        .await
        .expect("ensure live interrupt fixture principal");

    let backend = InferenceBackend {
        agent_did: agent_did.to_string(),
        backend_id: LIVE_BACKEND_ID.to_string(),
        name: LIVE_BACKEND_ID.to_string(),
        provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: Some(OpenAiWireApi::ChatCompletions),
        endpoint: endpoint.to_string(),
        auth: BackendAuth::Unauthenticated,
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        max_concurrent: Some(4),
        max_queue_depth: Some(100),
        enabled: true,
        tags: Vec::new(),
    };
    // Sampling matches the live GLM-5.3 deployment settings; `max_tokens`
    // rides the profile's canonical output budget (32,768), leaving the
    // stream long enough to interrupt mid-flight.
    let sampling = InferenceSampling {
        agent_did: agent_did.to_string(),
        sampling_id: LIVE_SAMPLING_ID.to_string(),
        display_name: Some("live high-thinking interrupt sampling".to_string()),
        temperature: Some(1.0),
        top_p: Some(0.95),
        ..Default::default()
    };
    let profile = InferenceProfile {
        agent_did: agent_did.to_string(),
        profile_id: default_inference_profile_id_for_behavior(LIVE_BEHAVIOR_ID),
        backend_id: LIVE_BACKEND_ID.to_string(),
        model_name: model.to_string(),
        sampling_id: Some(LIVE_SAMPLING_ID.to_string()),
        reasoning_effort: Some(ReasoningEffort::High),
        ..Default::default()
    };
    let context = AgentContext {
        context_id: LIVE_CONTEXT_ID.to_string(),
        agent_did: agent_did.to_string(),
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
    let behavior = AgentBehavior {
        behavior_id: LIVE_BEHAVIOR_ID.to_string(),
        agent_did: agent_did.to_string(),
        display_name: Some(LIVE_BEHAVIOR_ID.to_string()),
        description: None,
        context_id: Some(LIVE_CONTEXT_ID.to_string()),
        inference_profile_id: default_inference_profile_id_for_behavior(LIVE_BEHAVIOR_ID),
        enabled: true,
        tags: Vec::new(),
        created_at: Some("2026-06-02T00:00:00Z".to_string()),
    };
    principal.default_behavior_id = Some(LIVE_BEHAVIOR_ID.to_string());
    let documents = vec![
        (
            Collection::InferenceSampling,
            serde_json::to_value(sampling).expect("serialize live interrupt sampling"),
        ),
        (
            Collection::InferenceProfile,
            serde_json::to_value(profile).expect("serialize live interrupt profile"),
        ),
        (
            Collection::AgentContext,
            serde_json::to_value(context).expect("serialize live interrupt context"),
        ),
        (
            Collection::AgentBehavior,
            serde_json::to_value(behavior).expect("serialize live interrupt behavior"),
        ),
        (
            Collection::InferenceBackend,
            serde_json::to_value(backend).expect("serialize live interrupt backend"),
        ),
        (
            Collection::AgentPrincipal,
            serde_json::to_value(principal).expect("serialize live interrupt principal"),
        ),
    ];
    apply_fixture_documents(node, documents).await;
}

async fn apply_fixture_documents(
    node: &EmbeddedNode,
    documents: Vec<(Collection, serde_json::Value)>,
) {
    use gents::config_client::{
        apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };

    let plan = DesiredStateApplyPlan::new(
        documents
            .into_iter()
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect(),
    )
    .expect("build live interrupt fixture plan");
    ConfigAccess::transact_local(node, None, "test.live_interrupt_fixture", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    })
    .await
    .expect("apply live interrupt fixture documents");
}
