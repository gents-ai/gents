use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use gents::config_client::ConfigAccess;
use gents::defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::CancelBackgroundToolCallOutcome;
use gents::UpdateSubscriptionSource;
use gents_codex_protocol as codex;
use serde_json::Value;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::agent_projection::agent_tool_item;
use super::command_projection::{
    codex_command_status, command_execution_item, command_output_payload, tool_projection_status,
    ToolProjectionStatus,
};
use super::progress::{
    decode_gents_tool_call_progress, gents_tool_progress_query, observed_tool_status,
    tool_completed_at_ms, GentsToolCallProgress,
};
use super::projection_state::ProjectionStatus;
use super::protocol::{now_millis, send_notification};
use super::store::query_node_json;
use super::{ConnectionState, ShimState};

#[expect(
    clippy::too_many_arguments,
    reason = "the watcher owns each turn coordinate it reports against"
)]
pub(super) fn spawn_background_tool_watcher(
    connection: ConnectionState,
    state: ShimState,
    request_doc_id: String,
    session_id: String,
    thread_id: String,
    turn_id: String,
    cwd: PathBuf,
    running: BTreeSet<String>,
) {
    let _ = spawn_background_tool_watcher_handle(
        connection,
        state,
        request_doc_id,
        session_id,
        thread_id,
        turn_id,
        cwd,
        running,
        None,
    );
}

#[expect(
    clippy::too_many_arguments,
    reason = "the watcher owns each turn coordinate it reports against"
)]
fn spawn_background_tool_watcher_handle(
    connection: ConnectionState,
    state: ShimState,
    request_doc_id: String,
    session_id: String,
    thread_id: String,
    turn_id: String,
    cwd: PathBuf,
    mut running: BTreeSet<String>,
    mut first_query_observed: Option<oneshot::Sender<()>>,
) -> Option<JoinHandle<()>> {
    if running.is_empty() {
        return None;
    }

    Some(tokio::spawn(async move {
        let mut updates = state.node.subscribe_updates();
        while !running.is_empty() {
            if connection.outbound.is_closed() {
                tracing::debug!("Codex shim background tool watcher stopped after outbound closed");
                break;
            }

            let response = match query_node_json(
                state.node.as_ref(),
                &gents_tool_progress_query(&request_doc_id, &session_id),
            )
            .await
            {
                Ok(response) => Some(response),
                Err(error) => {
                    tracing::warn!(%error, "Codex shim background tool watcher query failed");
                    None
                }
            };
            if let Some(observed) = first_query_observed.take() {
                let _ = observed.send(());
            }

            if let Some(response) = response {
                let tool_rows = response
                    .pointer("/data/AgentToolCall")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let current_tools = tool_rows
                    .iter()
                    .filter_map(decode_gents_tool_call_progress)
                    .map(|tool| (tool.tool_call_key.clone(), tool))
                    .collect::<BTreeMap<_, _>>();

                let tracked = running.iter().cloned().collect::<Vec<_>>();
                for tool_key in tracked {
                    let Some(tool) = current_tools.get(&tool_key) else {
                        continue;
                    };
                    if tool.request_doc_id.as_deref() != Some(request_doc_id.as_str()) {
                        tracing::warn!(
                            tool_key,
                            "Codex background tool crossed physical request scope"
                        );
                        continue;
                    }
                    match observed_tool_status(tool) {
                        ProjectionStatus::InProgress => {}
                        status => {
                            let (Some(tool_doc_id), Some(node_did)) =
                                (tool.doc_id.as_deref(), tool.node_did.as_deref())
                            else {
                                tracing::warn!(
                                    tool_key,
                                    "Codex background tool omitted exact canonical identity"
                                );
                                continue;
                            };
                            let access = ConfigAccess::local(state.node.clone());
                            let presentation =
                                match gents::tool_call_lifecycle::load_tool_call_presentation(
                                    &access,
                                    tool_doc_id,
                                    node_did,
                                    &session_id,
                                    tool.requester_did.as_deref(),
                                )
                                .await
                                {
                                    Ok(presentation) => presentation,
                                    Err(error) => {
                                        tracing::warn!(%error, tool_key, "Codex background canonical tool payload is not ready");
                                        continue;
                                    }
                                };
                            let Some(result) = presentation.result else {
                                continue;
                            };
                            let mut tool = tool.clone();
                            tool.args = presentation.arguments;
                            tool.result = result;
                            if let Err(error) = send_background_tool_completion(
                                &connection.outbound,
                                &state,
                                &thread_id,
                                &turn_id,
                                &tool,
                                codex_command_status(status),
                                &cwd,
                            )
                            .await
                            {
                                tracing::warn!(%error, "Codex shim background tool completion send failed");
                                return;
                            }
                            running.remove(&tool_key);
                        }
                    }
                }
            }

            if running.is_empty() {
                break;
            }

            tokio::select! {
                _ = connection.outbound.closed() => {
                    tracing::debug!("Codex shim background tool watcher stopped after outbound closed");
                }
                _ = tokio::time::sleep(state.poll_interval) => {}
                msg = updates.recv() => {
                    if msg.is_none() {
                        tracing::warn!("Codex shim background update subscription closed");
                    }
                    let dropped = updates.check_and_reset_dropped();
                    if dropped > 0 {
                        tracing::warn!(dropped, "Codex shim background update subscription dropped messages");
                    }
                }
            }
        }
    }))
}

async fn send_background_tool_completion(
    outbound: &super::Outbound,
    state: &ShimState,
    thread_id: &str,
    turn_id: &str,
    tool: &GentsToolCallProgress,
    status: codex::CommandExecutionStatus,
    cwd: &Path,
) -> Result<()> {
    if let ToolProjectionStatus::Agent(projection) = tool_projection_status(tool) {
        return send_notification(
            outbound,
            state,
            codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
                item: agent_tool_item(thread_id, tool, &projection),
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
                completed_at_ms: tool_completed_at_ms(tool).unwrap_or_else(now_millis),
            }),
        )
        .await;
    }
    if let Some(delta) = command_output_payload(tool) {
        send_notification(
            outbound,
            state,
            codex::ServerNotification::CommandExecutionOutputDelta(
                codex::CommandExecutionOutputDeltaNotification {
                    thread_id: thread_id.to_string(),
                    turn_id: turn_id.to_string(),
                    item_id: tool.tool_call_key.clone(),
                    delta,
                },
            ),
        )
        .await?;
    }

    send_notification(
        outbound,
        state,
        codex::ServerNotification::ItemCompleted(codex::ItemCompletedNotification {
            item: command_execution_item(cwd, tool, status),
            thread_id: thread_id.to_string(),
            turn_id: turn_id.to_string(),
            completed_at_ms: tool_completed_at_ms(tool).unwrap_or_else(now_millis),
        }),
    )
    .await
}

#[derive(Debug, Clone)]
struct BackgroundTerminalRow {
    tool_call_key: String,
}

pub(super) async fn clean_background_terminals(state: &ShimState, thread_id: &str) -> Result<()> {
    let rows = load_running_background_terminal_rows(state.node.as_ref(), thread_id).await?;
    for row in rows {
        cancel_projected_background_tool_key(state, &row.tool_call_key).await?;
    }
    Ok(())
}

pub(super) async fn cancel_projected_background_tool_key(
    state: &ShimState,
    tool_call_key: &str,
) -> Result<CancelBackgroundToolCallOutcome> {
    let (session_id, tool_call_id) = background_tool_address(state, tool_call_key).await?;
    let outcome = gents::cancel_background_tool_call(
        state.node.clone(),
        &state.background_execution_registry,
        &session_id,
        &tool_call_id,
    )
    .await?;
    match &outcome {
        CancelBackgroundToolCallOutcome::Cancelled { .. }
        | CancelBackgroundToolCallOutcome::AlreadyTerminal { .. } => Ok(outcome),
        CancelBackgroundToolCallOutcome::Lost => anyhow::bail!(
            "GENTS background tool `{tool_call_key}` is not owned by this runtime; it was settled as lost without verifying termination"
        ),
        CancelBackgroundToolCallOutcome::Unverified => anyhow::bail!(
            "GENTS background tool `{tool_call_key}` is still running after termination was requested"
        ),
        CancelBackgroundToolCallOutcome::NotFound => {
            anyhow::bail!("unknown GENTS background tool `{tool_call_key}`")
        }
        CancelBackgroundToolCallOutcome::NotBackground => {
            anyhow::bail!("GENTS tool `{tool_call_key}` is not a background tool")
        }
    }
}

/// Codex holds a row's stored `tool_call_key` as its process id, and that key
/// is `{request_doc_id}:...`, not `session_id:tool_call_id`. A key that names
/// no stored row is read as the `session_id:tool_call_id` form `clean` builds.
async fn background_tool_address(
    state: &ShimState,
    tool_call_key: &str,
) -> Result<(String, String)> {
    let escaped_key = escape_graphql_string(tool_call_key);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ tool_call_key: {{ _eq: "{escaped_key}" }} }}, limit: 1) {{ session_id tool_call_id }} }}"#
    );
    let response = query_node_json(state.node.as_ref(), &query).await?;
    if let Some(row) = response.pointer("/data/AgentToolCall/0") {
        if let (Some(session_id), Some(tool_call_id)) = (
            row.get("session_id").and_then(Value::as_str),
            row.get("tool_call_id").and_then(Value::as_str),
        ) {
            return Ok((session_id.to_owned(), tool_call_id.to_owned()));
        }
    }
    let Some((session_id, tool_call_id)) = tool_call_key.split_once(':') else {
        anyhow::bail!("Codex process id `{tool_call_key}` is not a GENTS background tool key");
    };
    Ok((session_id.to_owned(), tool_call_id.to_owned()))
}

async fn load_running_background_terminal_rows(
    node: &EmbeddedNode,
    thread_id: &str,
) -> Result<Vec<BackgroundTerminalRow>> {
    let escaped_thread_id = escape_graphql_string(thread_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    session_id: {{ _eq: "{escaped_thread_id}" }},
                    await_mode: {{ _eq: "background" }},
                    lifecycle_state: {{ _eq: "running" }}
                }},
                order: {{ started_at: ASC }}
            ) {{
                session_id
                tool_call_id
            }}
        }}"#
    );
    let response = query_node_json(node, &query).await?;
    let rows = response
        .pointer("/data/AgentToolCall")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(rows
        .iter()
        .filter_map(decode_background_terminal_row)
        .collect())
}

fn decode_background_terminal_row(row: &Value) -> Option<BackgroundTerminalRow> {
    Some(BackgroundTerminalRow {
        tool_call_key: format!(
            "{}:{}",
            row.get("session_id")?.as_str()?,
            row.get("tool_call_id")?.as_str()?
        ),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::time::Duration;

    use gents::session::canonical_rows::{
        output_segment_create_variables, transcript_message_create_variables,
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_protocol::job::JobState;
    use gents_protocol::output::{OutputSegment, TranscriptMessage};
    use tokio::sync::{mpsc, oneshot, Mutex};

    use super::super::{CodexSidecar, ConnectionState, ShimState};
    use super::*;
    use crate::commands::shim_test_fixtures::seed_job_row;

    async fn insert_segment(node: &EmbeddedNode, segment: &OutputSegment) -> String {
        let response = node
            .execute_request(
                gents::defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                    .with_variables(output_segment_create_variables(segment).unwrap()),
            )
            .await;
        assert!(
            !response.has_errors(),
            "segment seed: {:?}",
            response.errors
        );
        gents::graphql::single_mutation_document(&response, "create_AgentOutputSegment")
            .unwrap()
            .unwrap()["_docID"]
            .as_str()
            .unwrap()
            .to_string()
    }

    async fn insert_header(node: &EmbeddedNode, header: &TranscriptMessage) {
        let response = node
            .execute_request(
                gents::defra_node::QueryRequest::new(CREATE_AGENT_MESSAGE_MUTATION)
                    .with_variables(transcript_message_create_variables(header).unwrap()),
            )
            .await;
        assert!(
            !response.has_errors(),
            "message seed: {:?}",
            response.errors
        );
    }

    fn test_connection() -> (ConnectionState, mpsc::UnboundedReceiver<String>) {
        let (outbound, outbound_rx) = mpsc::unbounded_channel::<String>();
        (
            ConnectionState {
                outbound,
                turn_streams: Arc::new(Mutex::new(BTreeMap::new())),
                fuzzy_file_search_sessions: Arc::new(Mutex::new(BTreeMap::new())),
                pending_steering_inputs: Arc::new(Mutex::new(BTreeMap::new())),
                child_thread_streams: Arc::new(Mutex::new(BTreeMap::new())),
                root_continuation_streams: Arc::new(Mutex::new(BTreeMap::new())),
            },
            outbound_rx,
        )
    }

    fn test_state(
        tempdir: &tempfile::TempDir,
        node: Arc<EmbeddedNode>,
        poll_interval: Duration,
    ) -> ShimState {
        ShimState {
            usage_cache: Arc::default(),
            codex_home: tempdir.path().join("codex-home"),
            trace_path: tempdir
                .path()
                .join("codex-home/log/codex-shim-events.jsonl"),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            fs_root: None,
            node,
            background_execution_registry: gents::BackgroundExecutionRegistry::default(),
            graphql: gents::config_client::GraphqlEndpoint::anonymous("http://127.0.0.1/graphql"),
            node_did: Arc::from("did:test:background-watcher"),
            agent_id: Arc::from("did:test:background-watcher:default"),
            id_counter: Arc::new(AtomicU64::new(1)),
            timeout: Duration::from_secs(5),
            poll_interval,
            sidecar: Arc::new(Mutex::new(CodexSidecar::default())),
            auth_token: None,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "test seed helper takes the seeded row's fields"
    )]
    async fn seed_background_tool(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        tool_call_id: &str,
        tool_name: &str,
        await_mode: &str,
        spawned_by_tool_call_doc_id: Option<&str>,
        lifecycle_state: &str,
        status: &str,
    ) -> String {
        let tool_call_key = format!("{session_id}:{tool_call_id}");
        let spawned_by = spawned_by_tool_call_doc_id
            .map(|doc_id| {
                format!(
                    "spawned_by_tool_call_doc_id: \"{}\",",
                    escape_graphql_string(doc_id)
                )
            })
            .unwrap_or_default();
        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    request_id: "{request_id}",
                    request_doc_id: "{request_id}",
                    session_id: "{session_id}",
                    node_did: "did:test:background-watcher",
                    message_sequence: 1,
                    tool_name: "{tool_name}",
                    tool_call_id: "{tool_call_id}",
                    {spawned_by}
                    status: "{status}",
                    lifecycle_state: "{lifecycle_state}",
                    started_at: "2026-07-07T12:00:00Z",
                    completed_at: "2026-07-07T12:00:01Z",
                    await_mode: "{await_mode}"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "seed AgentToolCall failed: {:?}",
            response.errors
        );
        gents::graphql::single_mutation_document(&response, "create_AgentToolCall")
            .expect("tool mutation envelope")
            .expect("created tool row")["_docID"]
            .as_str()
            .expect("physical tool ID")
            .to_string()
    }

    async fn seed_canonical_tool_delivery(
        node: &EmbeddedNode,
        request_id: &str,
        session_id: &str,
        tool_call_id: &str,
        tool_doc_id: &str,
        spawned_parent: Option<(&str, &str)>,
        result: &str,
    ) {
        use gents_protocol::output::{
            MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment,
            OutputSource, OutputWriter, PayloadPresentation, PayloadRef, PresentedPayload,
            SegmentRun, SourceClose, StreamDeclaration, StreamPayload, ToolResultPart,
            TranscriptMessage,
        };

        let node_did = "did:test:background-watcher";
        let created_at = "2026-07-07T12:00:00Z";
        let args = if spawned_parent.is_some() {
            r#"{"tool_name":"bash","args":{"command":"true"}}"#
        } else {
            r#"{"command":"true"}"#
        };
        let (accepted_tool_doc_id, accepted_call_id, accepted_tool_name) = spawned_parent
            .map(|(doc_id, call_id)| (doc_id, call_id, "spawn_process"))
            .unwrap_or((tool_doc_id, tool_call_id, "bash"));
        let accepted_segment = OutputSegment {
            node_did: node_did.into(),
            requester_did: None,
            session_id: session_id.into(),
            request_doc_id: request_id.into(),
            source: OutputSource::ProviderTurn {
                scope: "inference.1".parse().unwrap(),
                turn_index: 0,
                attempt: 0,
            },
            writer: OutputWriter::RequestExecution {
                execution_generation: "watcher-generation".into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: args.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolArguments {
                        id: accepted_call_id.into(),
                        call_id: None,
                        name: accepted_tool_name.into(),
                    },
                }),
            }],
            payload: args.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![args.len() as u64],
            }),
            created_at: created_at.into(),
        };
        let accepted_close_doc_id = insert_segment(node, &accepted_segment).await;
        insert_header(
            node,
            &TranscriptMessage {
                message_key: format!("accepted:{request_id}:{tool_call_id}"),
                session_id: session_id.into(),
                node_did: node_did.into(),
                requester_did: None,
                request_doc_id: Some(request_id.into()),
                publication: MessagePublication::RequestExecution {
                    execution_generation: "watcher-generation".into(),
                },
                outcome: OutputOutcome::Complete,
                sequence: 1,
                role: MessageRole::Assistant,
                native_id: None,
                blocks: vec![MessageBlock::ToolCall {
                    tool_call_doc_id: accepted_tool_doc_id.into(),
                    id: accepted_call_id.into(),
                    call_id: None,
                    name: accepted_tool_name.into(),
                    arguments: PayloadRef {
                        close_doc_id: accepted_close_doc_id,
                        stream: 0,
                    },
                    signature: None,
                    additional_params: None,
                }],
                created_at: created_at.into(),
            },
        )
        .await;

        let result_segment = OutputSegment {
            node_did: node_did.into(),
            requester_did: None,
            session_id: session_id.into(),
            request_doc_id: request_id.into(),
            source: OutputSource::ToolCall {
                tool_call_doc_id: tool_doc_id.into(),
            },
            writer: OutputWriter::ToolExecution {
                tool_call_doc_id: tool_doc_id.into(),
            },
            ordinal: Some(0),
            runs: vec![SegmentRun {
                stream: 0,
                bytes: result.len() as u32,
                declaration: Some(StreamDeclaration {
                    block_index: 0,
                    part_index: 0,
                    payload: StreamPayload::ToolOutput,
                }),
            }],
            payload: result.into(),
            close: Some(SourceClose::Closed {
                outcome: OutputOutcome::Complete,
                segments: 1,
                stream_bytes: vec![result.len() as u64],
            }),
            created_at: created_at.into(),
        };
        let result_close_doc_id = insert_segment(node, &result_segment).await;
        if spawned_parent.is_some() {
            return;
        }
        insert_header(
            node,
            &TranscriptMessage {
                message_key: format!("delivery:{request_id}:{tool_call_id}"),
                session_id: session_id.into(),
                node_did: node_did.into(),
                requester_did: None,
                request_doc_id: Some(request_id.into()),
                publication: MessagePublication::ToolDelivery {
                    tool_call_doc_id: tool_doc_id.into(),
                },
                outcome: OutputOutcome::Complete,
                sequence: 2,
                role: MessageRole::User,
                native_id: None,
                blocks: vec![MessageBlock::ToolResult {
                    tool_call_doc_id: tool_doc_id.into(),
                    id: tool_call_id.into(),
                    call_id: None,
                    parts: vec![ToolResultPart::Text {
                        text: PresentedPayload {
                            output: PayloadRef {
                                close_doc_id: result_close_doc_id,
                                stream: 0,
                            },
                            presentation: PayloadPresentation::Full,
                        },
                    }],
                }],
                created_at: created_at.into(),
            },
        )
        .await;
    }

    async fn receive_item_completed(outbound_rx: &mut mpsc::UnboundedReceiver<String>) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let message = outbound_rx.recv().await.expect("watcher outbound message");
                let value: Value = serde_json::from_str(&message).expect("valid JSON notification");
                if value.get("method").and_then(Value::as_str) == Some("item/completed") {
                    return value;
                }
            }
        })
        .await
        .expect("watcher should emit item/completed")
    }

    async fn assert_watcher_recovers_after_initial_observation(
        test_name: &str,
        schemas_before_start: bool,
        spawned: bool,
    ) {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(tempdir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        if schemas_before_start {
            gents::schema::ensure_runtime_schemas(&node)
                .await
                .expect("runtime schemas");
        }

        let request_id = format!("req-{test_name}");
        let session_id = format!("session-{test_name}");
        let tool_call_id = format!("call-{test_name}");
        let tool_call_key = format!("{session_id}:{tool_call_id}");
        let (connection, mut outbound_rx) = test_connection();
        let state = test_state(&tempdir, node.clone(), Duration::from_millis(10));
        let running = BTreeSet::from([tool_call_key.clone()]);
        let (observed_tx, observed_rx) = oneshot::channel();
        let handle = spawn_background_tool_watcher_handle(
            connection,
            state,
            request_id.clone(),
            session_id.clone(),
            session_id.clone(),
            request_id.clone(),
            tempdir.path().to_path_buf(),
            running,
            Some(observed_tx),
        )
        .expect("watcher should spawn for tracked tool");

        tokio::time::timeout(Duration::from_secs(5), observed_rx)
            .await
            .expect("watcher should make its initial observation")
            .expect("watcher should signal first query attempt");
        if !schemas_before_start {
            gents::schema::ensure_runtime_schemas(&node)
                .await
                .expect("runtime schemas");
        }
        let parent_call_id = format!("spawn-{test_name}");
        let parent_doc_id = if spawned {
            Some(
                seed_background_tool(
                    &node,
                    &request_id,
                    &session_id,
                    &parent_call_id,
                    "spawn_process",
                    "foreground",
                    None,
                    "completed",
                    "completed",
                )
                .await,
            )
        } else {
            None
        };
        let tool_doc_id = seed_background_tool(
            &node,
            &request_id,
            &session_id,
            &tool_call_id,
            "bash",
            "background",
            parent_doc_id.as_deref(),
            "completed",
            "completed",
        )
        .await;
        seed_canonical_tool_delivery(
            &node,
            &request_id,
            &session_id,
            &tool_call_id,
            &tool_doc_id,
            parent_doc_id
                .as_deref()
                .map(|doc_id| (doc_id, parent_call_id.as_str())),
            "done",
        )
        .await;

        let completed = receive_item_completed(&mut outbound_rx).await;
        assert_eq!(completed["params"]["item"]["id"], tool_call_key);
        assert_eq!(completed["params"]["item"]["status"], "completed");
        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("watcher should exit after durable terminal observation")
            .expect("watcher task should not panic");
    }

    #[tokio::test]
    async fn background_tool_watcher_exits_when_outbound_closes_while_tool_is_running() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let node = Arc::new(
            crate::shared::test_support::node_builder()
                .data_path(tempdir.path().join("node"))
                .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
                .build()
                .await
                .expect("embedded node"),
        );
        gents::schema::ensure_runtime_schemas(&node)
            .await
            .expect("runtime schemas");

        let request_id = "req-background-disconnect";
        let session_id = "session-background-disconnect";
        let tool_call_id = "call-background-disconnect";
        let tool_call_key = format!("{session_id}:{tool_call_id}");
        let mutation = format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    request_id: "{request_id}",
                    session_id: "{session_id}",
                    node_did: "did:test:background-watcher",
                    message_sequence: 1,
                    tool_name: "bash",
                    tool_call_id: "{tool_call_id}",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "2026-07-07T12:00:00Z",
                    await_mode: "background"
                }}) {{ _docID }}
            }}"#
        );
        let response = node.execute(&mutation).await;
        assert!(
            !response.has_errors(),
            "seed AgentToolCall failed: {:?}",
            response.errors
        );

        let (connection, outbound_rx) = test_connection();
        let state = test_state(&tempdir, node, Duration::from_secs(60));

        let mut running = BTreeSet::new();
        running.insert(tool_call_key);
        let (observed_tx, observed_rx) = oneshot::channel();
        let handle = spawn_background_tool_watcher_handle(
            connection,
            state,
            request_id.to_string(),
            session_id.to_string(),
            session_id.to_string(),
            request_id.to_string(),
            tempdir.path().to_path_buf(),
            running,
            Some(observed_tx),
        )
        .expect("watcher should spawn for running tool");

        tokio::time::timeout(Duration::from_secs(5), observed_rx)
            .await
            .expect("watcher should query running background tool before disconnect")
            .expect("watcher should signal first query");
        drop(outbound_rx);

        tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("watcher should exit promptly when outbound closes")
            .expect("watcher task should not panic");
    }

    #[tokio::test]
    async fn background_tool_watcher_retains_key_while_row_is_temporarily_missing() {
        assert_watcher_recovers_after_initial_observation("background-missing", true, false).await;
    }

    #[tokio::test]
    async fn background_tool_watcher_retries_after_query_error() {
        assert_watcher_recovers_after_initial_observation("background-query-retry", false, false)
            .await;
    }

    #[tokio::test]
    async fn background_tool_watcher_completes_spawned_process_without_child_result_header() {
        assert_watcher_recovers_after_initial_observation("spawned-process", true, true).await;
    }

    /// Job control authors as the node, so the node DID must be a registered
    /// signing identity.
    async fn job_test_state(dir: &std::path::Path) -> ShimState {
        let mut state = super::super::turn_projection::tests::notification_test_state(dir).await;
        let identity = gents::KeyIdentity::load_or_create(dir.join("actor.key"), None).unwrap();
        state.node_did = Arc::from(gents::NodeIdentity::did(&identity));
        state
    }

    async fn seed_running_job(state: &ShimState, session: &str, call: &str) {
        let (node, did) = (&state.node, &state.node_did);
        seed_job_row(node, did, session, call, call, "running", JobState::Running).await;
    }

    async fn job_state(state: &ShimState, session: &str, call: &str) -> JobState {
        gents::plugin_job::view::load_job_view(
            &ConfigAccess::local(state.node.clone()),
            None,
            gents::plugin_job::view::JobKey::Address {
                session_id: session.into(),
                tool_call_id: call.into(),
            },
        )
        .await
        .unwrap()
        .state
    }

    /// Codex receives the stored `tool_call_key` as the row's `process_id`
    /// (`command_execution_item`).
    async fn stored_tool_call_key(state: &ShimState, session: &str, call: &str) -> String {
        let query = format!(
            r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }}, tool_call_id: {{ _eq: "{call}" }} }}) {{ tool_call_key }} }}"#
        );
        let response = query_node_json(state.node.as_ref(), &query).await.unwrap();
        response["data"]["AgentToolCall"][0]["tool_call_key"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn job_row_is_tracked_as_background_row() {
        let dir = tempfile::tempdir().unwrap();
        let state = job_test_state(dir.path()).await;
        seed_running_job(&state, "codex-job", "call-a").await;

        let rows = load_running_background_terminal_rows(state.node.as_ref(), "codex-job")
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tool_call_key, "codex-job:call-a");
        state.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_row_stops_through_its_process_key() {
        let dir = tempfile::tempdir().unwrap();
        let state = job_test_state(dir.path()).await;
        seed_running_job(&state, "codex-job", "call-a").await;
        let process_id = stored_tool_call_key(&state, "codex-job", "call-a").await;

        let outcome = cancel_projected_background_tool_key(&state, &process_id).await;

        assert!(
            matches!(
                outcome,
                Ok(CancelBackgroundToolCallOutcome::Cancelled {
                    live_execution_cancelled: false
                })
            ),
            "{outcome:?}"
        );
        let stopped = job_state(&state, "codex-job", "call-a").await;
        assert_eq!(stopped, JobState::Cancelled);
        state.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_rows_stop_on_clean() {
        let dir = tempfile::tempdir().unwrap();
        let state = job_test_state(dir.path()).await;
        seed_running_job(&state, "codex-clean", "call-a").await;
        seed_running_job(&state, "codex-clean", "call-b").await;
        seed_running_job(&state, "codex-other", "call-c").await;

        clean_background_terminals(&state, "codex-clean")
            .await
            .unwrap();

        for call in ["call-a", "call-b"] {
            let stopped = job_state(&state, "codex-clean", call).await;
            assert_eq!(stopped, JobState::Cancelled, "{call}");
        }
        let other = job_state(&state, "codex-other", "call-c").await;
        assert_eq!(other, JobState::Running);
        state.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_row_already_ended_is_ok() {
        let dir = tempfile::tempdir().unwrap();
        let state = job_test_state(dir.path()).await;
        let (node, did) = (&state.node, &state.node_did);
        seed_job_row(
            node,
            did,
            "codex-job",
            "call-a",
            "call-a",
            "completed",
            JobState::Completed,
        )
        .await;

        let outcome = cancel_projected_background_tool_key(&state, "codex-job:call-a").await;

        let Ok(CancelBackgroundToolCallOutcome::AlreadyTerminal { state: ended }) = outcome else {
            panic!("expected AlreadyTerminal, got {outcome:?}");
        };
        assert_eq!(ended, "completed");
        let still = job_state(&state, "codex-job", "call-a").await;
        assert_eq!(still, JobState::Completed);
        state.node.shutdown().await;
    }

    #[tokio::test]
    async fn job_unknown_key_reports_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let state = super::super::turn_projection::tests::notification_test_state(dir.path()).await;

        let err = cancel_projected_background_tool_key(&state, "codex-none:call-none")
            .await
            .unwrap_err();

        assert_eq!(
            err.to_string(),
            "unknown GENTS background tool `codex-none:call-none`"
        );
        state.node.shutdown().await;
    }
}
