//! Live enrollment e2e: a throwaway `gents server` plus a fresh desktop
//! `ClientCore`. This is the production phone/desktop pairing path — HTTP
//! `/status` offer, operator approve, two-leg client route — not the
//! hand-installed replicators used by the live desktop fixture.

#[path = "cli_enrollment/contract.rs"]
mod contract;
mod support;
use support::*;

use std::fs;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use gents::default_behavior_id_for_agent;
use gents::session::canonical_rows::{OutputSegmentRow, TranscriptMessageRow};
use gents_desktop_core::client::canonical_output::{
    project_canonical_message, CanonicalMessageProjection,
};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use gents_protocol::output::{MessageRole, TerminalOutput};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::{sleep, Instant};
use uuid::Uuid;

static ENROLLMENT_E2E_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn enrollment_e2e_lock() -> &'static Mutex<()> {
    ENROLLMENT_E2E_LOCK.get_or_init(|| Mutex::new(()))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_enrollment_from_fresh_desktop_replicates_chat_without_agent_principal() -> Result<()>
{
    let _guard = enrollment_e2e_lock().lock().await;

    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("agent-home");
    let desktop_home = tempdir.path().join("desktop-home");
    fs::create_dir_all(&home_dir)?;
    let home_arg = home_dir
        .to_str()
        .ok_or_else(|| anyhow!("agent home path is not UTF-8"))?;

    let (model_endpoint, model_name) = resolve_live_inference().await?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let agent_name = format!("cli-enroll-{}", Uuid::new_v4().simple());
    // The nonce only makes the runtime request findable by prompt. Reply
    // content is model-chosen: a live model may decline to echo a token, so
    // replication is asserted against the runtime's own terminal output.
    let prompt = format!(
        "Enrollment check {}: in one short sentence, say hello to the newly paired desktop.",
        Uuid::new_v4().simple()
    );

    let init = run_init_json(
        &home_dir,
        &[
            "--home",
            home_arg,
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--max-concurrent",
            "2",
            "--max-queue-depth",
            "4",
            "--inference-url",
            &model_endpoint,
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let default_behavior_id = default_behavior_id_for_agent(&agent_did);

    let (mut serve, _readiness) =
        spawn_server_with_ready_json(&home_dir, port, &["--home", home_arg], &[])?;
    wait_for_port(port, &mut serve)?;

    serve
        .capturing(async {
            wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

            let status_url = format!("http://127.0.0.1:{port}");
            let (status, _) = wait_for_enrollment_token(&status_url).await?;
            assert_eq!(
                status.get("agent_name").and_then(Value::as_str),
                Some(agent_name.as_str())
            );
            assert_eq!(
                status.get("agent_did").and_then(Value::as_str),
                Some(agent_did.as_str())
            );

            let core = ClientCore::start_with_paths_and_options(
                DesktopPaths::from_root(&desktop_home),
                ClientCoreOptions::local_only(),
            )
            .await
            .context("starting fresh desktop ClientCore")?;

            let pending = core
                .request_status_enrollment_with_label(&status, Some(&agent_name))
                .await
                .context("requesting status enrollment from /status offer")?;
            assert_eq!(pending.state, "pending_approval");
            assert_eq!(pending.owner_agent, agent_did);

            let active = core
                .active_status_enrollment_requests()
                .await
                .context("loading pending enrollment requests")?;
            assert!(
                active.iter().any(|request| {
                    request.request_id == pending.request_id
                        && request.state == "pending_approval"
                }),
                "fresh desktop should surface the unpaired request as pending; got {active:?}"
            );

            // Pushing the persisted request again is idempotent: the runtime
            // still sees one request and approves it.
            core.resend_status_enrollment(&pending.request_id)
                .await
                .context("resending the persisted enrollment request")?;
            assert_eq!(
                core.active_status_enrollment_requests()
                    .await?
                    .iter()
                    .filter(|request| request.owner_agent == agent_did)
                    .count(),
                1,
                "a resend never authors another request"
            );
            wait_for_runtime_enrollment_request(&graphql, &pending.request_id).await?;
            run_cli_json(
                &home_dir,
                &[
                    "p2p",
                    "enrollment",
                    "approve",
                    &pending.request_id,
                    "--home",
                    home_arg,
                ],
            )
            .context("approving enrollment request")?;

            let enrolled = wait_for_chat_ready_enrollment(&core, &agent_did).await?;
            assert_ne!(
                enrolled.label.as_str(),
                "Enrolled Agent",
                "enrollment should seed the advertised agent name, not a placeholder"
            );
            assert_eq!(enrolled.label, agent_name);
            assert_eq!(enrolled.agent_did, agent_did);

            let principals = query_collection_dids(core.node(), "AgentPrincipal").await?;
            assert!(
                principals.is_empty(),
                "AgentPrincipal must stay on the runtime node; client gossiped {principals:?}"
            );
            assert!(
                core.store().snapshot().agent_principals.is_empty(),
                "client store must not materialize a gossiped AgentPrincipal"
            );

            wait_for_client_behavior_readiness(&core, &agent_did).await?;

            let session_id = Uuid::new_v4().to_string();
            core.submit_request(
                &session_id,
                &agent_did,
                &prompt,
                Some(&default_behavior_id),
            )
            .await
            .context("submitting chat request from the enrolled desktop")?;

            let (request_id, runtime_session, _) =
                wait_for_runtime_agent_request(&graphql, core.node(), &agent_did, &prompt).await?;
            let runtime_reply = wait_for_complete_agent_response(
                &graphql,
                &request_id,
                &runtime_session,
                Duration::from_secs(240),
            )
            .await?;
            wait_for_client_complete_response(
                &core,
                &session_id,
                &agent_did,
                &request_id,
                &runtime_reply,
            )
            .await?;

            let runtime_principals = graphql_query(
                &graphql,
                r#"{ AgentPrincipal { agent_did display_name } }"#,
            )
            .await?;
            let runtime_rows = runtime_principals
                .pointer("/data/AgentPrincipal")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            assert!(
                runtime_rows.iter().any(|row| {
                    row.get("agent_did").and_then(Value::as_str) == Some(agent_did.as_str())
                }),
                "runtime must keep AgentPrincipal locally: {runtime_principals}"
            );

            let remaining = core
                .active_status_enrollment_requests()
                .await
                .context("loading enrollment requests after pairing")?;
            assert!(
                remaining.is_empty(),
                "accepted pairing must clear the pending enrollment queue so another agent can be added; got {remaining:?}"
            );

            core.shutdown().await?;
            Ok(())
        })
        .await
}

async fn resolve_live_inference() -> Result<(String, String)> {
    let endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let models_url = format!("{}/models", endpoint.trim_end_matches('/'));
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?
        .get(&models_url)
        .send()
        .await
        .with_context(|| {
            format!("live inference at {endpoint} is unreachable; set GENTS_CLI_E2E_MODEL_ENDPOINT")
        })?;
    anyhow::ensure!(
        response.status().is_success(),
        "live inference GET {models_url} returned {}",
        response.status()
    );
    let body: Value = response
        .json()
        .await
        .context("decoding live inference /models")?;
    let advertised = body
        .pointer("/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !advertised.is_empty(),
        "live inference at {endpoint} advertised no models: {body}"
    );
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME").unwrap_or_else(|_| {
        advertised
            .iter()
            .find(|name| name.as_str() == "GLM-5.3-Flash-NVFP4")
            .cloned()
            .unwrap_or_else(|| advertised[0].clone())
    });
    anyhow::ensure!(
        advertised.iter().any(|name| name == &model_name),
        "live inference at {endpoint} does not serve {model_name}; advertised {advertised:?}"
    );
    Ok((endpoint, model_name))
}

async fn wait_for_enrollment_token(status_url: &str) -> Result<(Value, String)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match fetch_runtime_connection_payload(status_url).await {
            Ok(status) => {
                let token = status
                    .pointer("/enrollment/token")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|token| !token.is_empty())
                    .map(str::to_owned);
                if let Some(token) = token {
                    return Ok((status, token));
                }
                if Instant::now() >= deadline {
                    bail!("timed out waiting for /status enrollment token; last={status}");
                }
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for /status enrollment token: {error:#}");
                }
            }
        }
        sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_runtime_enrollment_request(graphql: &str, request_id: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let response =
            graphql_query(graphql, r#"{ NetworkEnrollmentRequest { request_id } }"#).await?;
        let found = response
            .pointer("/data/NetworkEnrollmentRequest")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .any(|row| row.get("request_id").and_then(Value::as_str) == Some(request_id));
        if found {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for runtime to observe enrollment request {request_id}; last={response}"
            );
        }
        sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_for_runtime_agent_request(
    graphql: &str,
    client_node: &gents::defra_node::EmbeddedNode,
    agent_did: &str,
    content: &str,
) -> Result<(String, String, String)> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRequest(
                        filter: {{ {} }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        request_id
                        session_id
                        behavior_id
                        content
                    }}
                }}"#,
                gents::session::public_request_filter(&format!(
                    r#"agent_did: {{ _eq: "{}" }}, content: {{ _eq: "{}" }}"#,
                    escape_graphql_string(agent_did),
                    escape_graphql_string(content),
                )),
            ),
        )
        .await
        {
            Ok(response) => {
                if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
                    return Ok((
                        row.get("request_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        row.get("session_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        row.get("behavior_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ));
                }
            }
            Err(error) => {
                if Instant::now() >= deadline {
                    bail!("runtime AgentRequest query failed: {error:#}");
                }
            }
        }
        if Instant::now() >= deadline {
            let client_requests = client_node
                .execute("{ AgentRequest { request_id agent_did content requester_did } }")
                .await;
            let runtime_all = graphql_query(
                graphql,
                "{ AgentRequest { request_id agent_did content requester_did } }",
            )
            .await
            .unwrap_or_else(|error| serde_json::json!({ "error": error.to_string() }));
            bail!(
                "runtime never received the enrolled client's AgentRequest; \
                 runtime_all={runtime_all}; client_requests={:?}",
                client_requests.data
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

/// Poll the canonical request-output owner until the runtime request reaches a
/// terminal lifecycle with visible selected content. Terminal selection lives
/// on `AgentRequest.terminal_output`; the exact physical request row is decoded
/// through the canonical `AgentRequestRow` owner and its presentation is
/// reconstructed from the selected canonical header and its segments — never
/// from inline message content or a response document.
async fn wait_for_complete_agent_response(
    graphql: &str,
    request_id: &str,
    session: &str,
    timeout: Duration,
) -> Result<RuntimeTerminalReply> {
    let deadline = Instant::now() + timeout;
    let mut empty_terminal_since = None::<Instant>;
    loop {
        let response = graphql_query(
            graphql,
            &format!(
                r#"{{
                    AgentRequest(
                        filter: {{
                            request_id: {{ _eq: "{}" }},
                            session_id: {{ _eq: "{}" }}
                        }},
                        order: {{ created_at: DESC }},
                        limit: 1
                    ) {{
                        _docID agent_did requester_did session_id request_id
                        lifecycle_state failure_reason terminal_output
                    }}
                }}"#,
                escape_graphql_string(request_id),
                escape_graphql_string(session),
            ),
        )
        .await?;
        if let Ok(row) = first_graphql_row(&response, "AgentRequest") {
            let request: gents_protocol::row::AgentRequestRow = serde_json::from_value(row.clone())
                .context("decoding canonical AgentRequest row")?;
            // Requester/session lineage must survive the runtime round trip
            // before the terminal output is observed for this physical row.
            anyhow::ensure!(
                request.session_id.as_deref() == Some(session)
                    && request
                        .agent_did
                        .as_deref()
                        .is_some_and(|did| !did.is_empty())
                    && request
                        .requester_did
                        .as_deref()
                        .is_some_and(|did| !did.is_empty()),
                "canonical AgentRequest row lost requester/session lineage for {request_id}: {row}"
            );
            if request
                .lifecycle_state
                .is_some_and(|state| state.as_str() == "failed")
            {
                bail!(
                    "live inference failed for {request_id}: failure_reason={:?}; row={row}",
                    request.failure_reason
                );
            }
            if request.is_terminal() {
                let output = gents::session::observe_request_output(
                    &gents::ConfigAccess::Graphql(graphql.to_owned()),
                    &request,
                )
                .await?;
                let visible = match &output {
                    gents::session::CanonicalRequestOutput::TerminalMessage {
                        header,
                        presentation,
                        ..
                    } => {
                        anyhow::ensure!(
                            header.request_doc_id.as_deref() == request.doc_id.as_deref()
                                && header.session_id == session,
                            "canonical terminal header does not match the physical request for {request_id}: header={header:?}; row={row}"
                        );
                        Some(presentation.body_markdown.clone())
                    }
                    gents::session::CanonicalRequestOutput::TerminalNoMessage => bail!(
                        "live inference completed without a canonical terminal message for {request_id}: {output:?}"
                    ),
                    gents::session::CanonicalRequestOutput::Denied
                    | gents::session::CanonicalRequestOutput::Invalid
                    | gents::session::CanonicalRequestOutput::Conflicted
                    | gents::session::CanonicalRequestOutput::Retracted => bail!(
                        "canonical terminal output is not presentable for {request_id}: {output:?}"
                    ),
                    _ => None,
                };
                if let Some(visible) = visible.filter(|text| !text.trim().is_empty()) {
                    let Some(TerminalOutput::Message { message_doc_id }) =
                        request.terminal_output.clone()
                    else {
                        bail!("terminal message output lost its header identity for {request_id}: {row}");
                    };
                    return Ok(RuntimeTerminalReply {
                        header_doc_id: message_doc_id,
                        text: visible,
                    });
                }
                if empty_terminal_since
                    .get_or_insert_with(Instant::now)
                    .elapsed()
                    >= Duration::from_secs(15)
                {
                    bail!(
                        "live inference produced no visible canonical terminal content for {request_id} within 15s of the first terminal observation: {row}; output={output:?}"
                    );
                }
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for canonical terminal output with visible text for {request_id}; last={response}"
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

/// The runtime-selected terminal header (`AgentRequest.terminal_output`) and
/// its presented text.
#[derive(Debug, Clone)]
struct RuntimeTerminalReply {
    header_doc_id: String,
    text: String,
}

/// The presented text of the exact terminal header the runtime selected, once
/// the client holds the completed request selecting that header and the header
/// reconstructs Ready. No other message of the request (the user prompt, an
/// earlier assistant turn) can stand in for it.
fn client_selected_reply(
    requests: &[AgentRequestRow],
    messages: &[TranscriptMessageRow],
    segments: &[OutputSegmentRow],
    request_id: &str,
    header_doc_id: &str,
) -> Option<String> {
    let request = requests.iter().find(|row| {
        row.request_id == request_id
            && row.lifecycle_state == Some(RequestLifecycleState::Completed)
            && matches!(
                &row.terminal_output,
                Some(TerminalOutput::Message { message_doc_id }) if message_doc_id == header_doc_id
            )
    })?;
    let header = messages.iter().find(|row| {
        row.doc_id == header_doc_id
            && row.message.role == MessageRole::Assistant
            && row.message.request_doc_id.is_some()
            && row.message.request_doc_id == request.doc_id
    })?;
    match project_canonical_message(header, segments, &[], &[]) {
        CanonicalMessageProjection::Ready(message) => {
            Some(gents_protocol::transcript::present_message(&message).body_markdown)
        }
        _ => None,
    }
}

async fn wait_for_client_complete_response(
    core: &ClientCore,
    session_id: &str,
    agent_did: &str,
    request_id: &str,
    runtime_reply: &RuntimeTerminalReply,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        // Match desktop_session_snapshot: transcript rows deliberately do not
        // live in the global observer. Read the app's bounded, scoped page.
        core.ensure_session_hydration_started(session_id, agent_did)
            .await?;
        core.refresh_local_request(agent_did, request_id).await?;
        let page = gents_desktop_core::client::load_session_transcript_page(
            core.node(),
            session_id,
            Some(agent_did),
            Some(core.principal().did()),
            None,
            Some(40),
        )
        .await?;
        let snapshot = core.store().snapshot();
        let client_reply = client_selected_reply(
            &snapshot.requests,
            &page.store.transcript_messages,
            &page.store.output_segments,
            request_id,
            &runtime_reply.header_doc_id,
        );
        if client_reply.as_deref() == Some(runtime_reply.text.as_str()) {
            return Ok(());
        }
        if snapshot.requests.iter().any(|row| {
            row.request_id == request_id
                && row
                    .lifecycle_state
                    .is_some_and(|state| state.as_str() == "failed")
        }) {
            bail!(
                "client received a failed request for {request_id}: {:?}",
                snapshot.requests
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "client never reconstructed the runtime-selected terminal header for {request_id} ({runtime_reply:?}); client_reply={client_reply:?}; requests={:?}; transcript_messages={:?}",
                snapshot.requests,
                snapshot.transcript_messages
            );
        }
        sleep(Duration::from_millis(250)).await;
    }
}

async fn wait_for_chat_ready_enrollment(
    core: &ClientCore,
    agent_did: &str,
) -> Result<gents_desktop_core::client::PeerRecord> {
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut updates = core.sync_state_updates();
    loop {
        let now = Utc::now();
        if let Some(record) = core
            .peer_records()
            .await
            .into_iter()
            .find(|record| record.agent_did == agent_did && record.is_chat_ready_at(now))
        {
            return Ok(record);
        }
        if Instant::now() >= deadline {
            let records = core.peer_records().await;
            bail!("timed out waiting for enrolled chat-ready route; peers={records:?}");
        }
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            updates.changed(),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => bail!("client sync-state channel closed"),
            Err(_) => {
                let records = core.peer_records().await;
                bail!("timed out waiting for enrolled chat-ready route; peers={records:?}");
            }
        }
    }
}

async fn wait_for_client_behavior_readiness(core: &ClientCore, agent_did: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut updates = core.store_change_updates();
    loop {
        let snapshot = core.store().snapshot();
        let has_behavior = snapshot
            .behaviors
            .iter()
            .any(|row| row.agent_did == agent_did);
        let has_readiness = snapshot
            .behavior_readiness
            .iter()
            .any(|row| row.agent_did == agent_did);
        if has_behavior && has_readiness {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for gossiped AgentBehavior/AgentBehaviorReadiness; behaviors={} readiness={}",
                snapshot.behaviors.len(),
                snapshot.behavior_readiness.len()
            );
        }
        match tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            updates.changed(),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(_)) => bail!("app projection channel closed"),
            Err(_) => {
                bail!(
                    "timed out waiting for gossiped AgentBehavior/AgentBehaviorReadiness; behaviors={} readiness={}",
                    snapshot.behaviors.len(),
                    snapshot.behavior_readiness.len()
                );
            }
        }
    }
}

async fn query_collection_dids(
    node: &gents::defra_node::EmbeddedNode,
    collection: &str,
) -> Result<Vec<String>> {
    let response = node
        .execute(&format!("{{ {collection} {{ agent_did }} }}"))
        .await;
    if response.has_errors() {
        bail!("query {collection} failed: {:?}", response.errors);
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get(collection))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| {
            row.get("agent_did")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect())
}

/// Negative control for `client_selected_reply`: with the reply text also
/// present in the user prompt and an earlier assistant turn of the same
/// request, only the runtime-selected header of the completed request counts.
#[test]
fn client_reply_requires_the_runtime_selected_terminal_header() {
    use gents_protocol::output::{
        MessageBlock, MessagePublication, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
        PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose,
        StreamDeclaration, StreamPayload, TranscriptMessage,
    };
    const REQUEST_DOC: &str = "bae-request";
    const TEXT: &str = "hello";
    let segment = |doc_id: &str, key: &str| OutputSegmentRow {
        doc_id: doc_id.into(),
        segment: OutputSegment {
            agent_did: "did:key:agent".into(),
            requester_did: None,
            session_id: "session".into(),
            request_doc_id: REQUEST_DOC.into(),
            source: OutputSource::Authored { key: key.into() },
            writer: OutputWriter::RequestExecution {
                execution_generation: "generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: TEXT.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::Text,
                }),
            }],
            payload: TEXT.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![TEXT.len() as u64],
            }),
            created_at: "2026-09-25T00:00:00Z".into(),
        },
    };
    let header =
        |doc_id: &str, close_doc_id: &str, sequence: u32, role: MessageRole| TranscriptMessageRow {
            doc_id: doc_id.into(),
            message: TranscriptMessage {
                message_key: format!("session:{sequence}"),
                session_id: "session".into(),
                agent_did: "did:key:agent".into(),
                requester_did: None,
                request_doc_id: Some(REQUEST_DOC.into()),
                publication: MessagePublication::RequestExecution {
                    execution_generation: "generation".into(),
                },
                outcome: OutputOutcome::Complete,
                sequence,
                role,
                native_id: None,
                blocks: vec![MessageBlock::Text {
                    text: PresentedPayload {
                        output: PayloadRef {
                            close_doc_id: close_doc_id.into(),
                            stream: 0,
                        },
                        presentation: PayloadPresentation::Full,
                    },
                }],
                created_at: "2026-09-25T00:00:00Z".into(),
            },
        };
    let request = |state: RequestLifecycleState, terminal: Option<&str>| AgentRequestRow {
        doc_id: Some(REQUEST_DOC.into()),
        request_id: "request".into(),
        lifecycle_state: Some(state),
        terminal_output: terminal.map(|message_doc_id| TerminalOutput::Message {
            message_doc_id: message_doc_id.into(),
        }),
        ..Default::default()
    };
    let prompt = header("bae-prompt", "bae-prompt-close", 1, MessageRole::User);
    let earlier = header(
        "bae-earlier",
        "bae-earlier-close",
        2,
        MessageRole::Assistant,
    );
    let selected = header(
        "bae-selected",
        "bae-selected-close",
        3,
        MessageRole::Assistant,
    );
    let segments = vec![
        segment("bae-prompt-close", "prompt"),
        segment("bae-earlier-close", "earlier"),
        segment("bae-selected-close", "selected"),
    ];
    let completed = [request(
        RequestLifecycleState::Completed,
        Some("bae-selected"),
    )];
    let replicated_prefix = [prompt.clone(), earlier.clone()];
    for (label, requests, messages) in [
        (
            "prompt and earlier turn only",
            &completed[..],
            &replicated_prefix[..],
        ),
        (
            "request not yet completed",
            &[request(RequestLifecycleState::Processing, None)][..],
            &[prompt.clone(), earlier.clone(), selected.clone()][..],
        ),
    ] {
        assert_eq!(
            client_selected_reply(requests, messages, &segments, "request", "bae-selected"),
            None,
            "{label} must not satisfy the client wait"
        );
    }
    let all = [prompt, earlier, selected];
    assert_eq!(
        client_selected_reply(&completed, &all, &segments[..2], "request", "bae-selected"),
        None,
        "the selected header must reconstruct Ready, not just exist"
    );
    assert_eq!(
        client_selected_reply(&completed, &all, &segments, "request", "bae-selected").as_deref(),
        Some(TEXT)
    );
}
