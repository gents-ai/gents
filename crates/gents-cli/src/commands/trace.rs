use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

use anyhow::{Context, Result};
use gents::adapter_projection::{
    adapter_projection_eval_jsonl_record_schema, adapter_projection_eval_jsonl_records,
    adapter_projection_json_schema, adapter_projection_jsonl_record_schema,
    adapter_projection_jsonl_records, adapter_projection_native_json,
    adapter_projection_native_json_schema, build_adapter_projection,
    validate_adapter_projection_contract, AdapterProjectionKind, ProjectionContext,
    ProjectionRedactionMode,
};
use gents::graphql::escape_graphql_string;
use gents::run_timeline::build_run_timeline;
use gents::run_timeline_fetch::{load_run_timeline, load_run_timeline_rows};
use gents::tool_call_lifecycle::ToolCallState;
use gents::trace_export::{
    analyze_request_failure, analyze_tool_call_with_persisted_outcome, extract_raw_tool_call_json,
    latency_ms, raw_message_json, AmyToolCallTraceRecord,
};
use gents_protocol::row::AgentRequestRow;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::cli::args::{
    TraceCaptureArgs, TraceCommand, TraceExportArgs, TraceProjectArgs, TraceProjectSchemaArgs,
    TraceProjectionArg, TraceProjectionFormatArg, TraceProjectionRedactionArg, TraceTimelineArgs,
};
use crate::config_writes::ConfigAccess;
use crate::{
    graphql_rows_or_empty_if_collection_missing, graphql_string_list_literal, print_json,
    write_json_output_file,
};

mod projection_acp;

pub(crate) async fn dispatch(command: TraceCommand) -> Result<()> {
    match command {
        TraceCommand::Export(args) => trace_export(args).await,
        TraceCommand::Timeline(args) => trace_timeline(args).await,
        TraceCommand::Project(args) => trace_project(args).await,
        TraceCommand::ProjectSchema(args) => trace_project_schema(args),
        TraceCommand::Capture(args) => trace_capture(args).await,
    }
}

/// Fetch rendered-request capture metadata — and, for exactly one match, its
/// `request_json` field-commit CID. This is the one deliberate body read in
/// the system: `--include-body` selects `request_json` and the raw provenance
/// manifest; without it neither is even queried, and the default output is the
/// same metadata surface the timeline exposes.
async fn trace_capture(args: TraceCaptureArgs) -> Result<()> {
    let (access, _home_dir) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;

    let mut clauses = Vec::new();
    if let Some(capture_key) = args.capture_key.as_deref() {
        clauses.push(format!(
            r#"capture_key: {{ _eq: "{}" }}"#,
            escape_graphql_string(capture_key)
        ));
    }
    if let Some(request_id) = args.request_id.as_deref() {
        clauses.push(format!(
            r#"request_id: {{ _eq: "{}" }}"#,
            escape_graphql_string(request_id)
        ));
    }
    if clauses.is_empty() {
        anyhow::bail!("pass --capture-key or --request-id");
    }
    if let Some(scope) = args.scope.as_deref() {
        clauses.push(format!(
            r#"capture_scope: {{ _eq: "{}" }}"#,
            escape_graphql_string(scope)
        ));
    }
    if let Some(turn) = args.turn {
        clauses.push(format!("turn_index: {{ _eq: {turn} }}"));
    }
    if let Some(attempt) = args.attempt {
        clauses.push(format!("attempt: {{ _eq: {attempt} }}"));
    }

    let body_fields = if args.include_body {
        "\n                request_json"
    } else {
        ""
    };
    let query = format!(
        r#"{{
            RenderedRequest(
                filter: {{ {filter} }},
                order: {{ created_at: ASC }}
            ) {{
                _docID
                capture_key
                request_doc_id
                request_id
                session_id
                capture_scope
                turn_index
                attempt
                capture_version
                model_name
                source
                provenance_json
                created_at{body_fields}
            }}
        }}"#,
        filter = clauses.join(", "),
    );
    let raw_rows =
        graphql_rows_or_empty_if_collection_missing(&access, "RenderedRequest", &query).await?;

    let mut entries = raw_rows
        .into_iter()
        .map(|raw| {
            let row: gents::run_timeline::TimelineRenderedRequestRow =
                serde_json::from_value(raw.clone()).context("decoding RenderedRequest row")?;
            Ok((row, raw))
        })
        .collect::<Result<Vec<_>>>()?;
    if entries.is_empty() {
        anyhow::bail!("no capture rows matched");
    }
    // Identity order: parsed numeric order key first, unparseable rows last by
    // capture key — deterministic either way, never a lexical seq sort.
    entries.sort_by(|(left, _), (right, _)| {
        let left_key = capture_order_padded(left);
        let right_key = capture_order_padded(right);
        left_key
            .cmp(&right_key)
            .then_with(|| left.capture_key.cmp(&right.capture_key))
    });

    if args.list {
        let captures = entries
            .iter()
            .map(|(row, raw)| capture_metadata_value(row, raw, args.include_body))
            .collect::<Vec<_>>();
        let value = json!({ "captures": captures });
        return write_or_print(args.output_file.as_deref(), &value);
    }

    if entries.len() > 1 {
        let keys = entries
            .iter()
            .map(|(row, _)| {
                format!(
                    "  {} ({} turn {} attempt {})",
                    row.capture_key,
                    row.capture_scope.as_deref().unwrap_or("?"),
                    row.turn_index.map_or("?".to_string(), |t| t.to_string()),
                    row.attempt.map_or("?".to_string(), |a| a.to_string()),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!(
            "{count} capture rows matched; narrow with --scope/--turn/--attempt or pass --list:\n{keys}",
            count = entries.len(),
        );
    }

    let (row, raw) = &entries[0];
    let mut value = capture_metadata_value(row, raw, args.include_body);
    let commit = match row.doc_id.as_deref() {
        Some(doc_id) => gents::rendered_request::commits::request_json_commit(&access, doc_id)
            .await?
            .map(|commit| json!({ "cid": commit.cid, "height": commit.height })),
        None => None,
    };
    value["request_json_commit"] = commit.unwrap_or_else(|| json!("unavailable"));
    write_or_print(args.output_file.as_deref(), &value)
}

/// The metadata object for one capture row: the timeline's event derivation
/// plus the document id, with the body fields attached only on request.
fn capture_metadata_value(
    row: &gents::run_timeline::TimelineRenderedRequestRow,
    raw: &Value,
    include_body: bool,
) -> Value {
    let event = gents::run_timeline::rendered_request_event(row);
    let mut value = serde_json::to_value(&event).unwrap_or_else(|_| json!({}));
    value["doc_id"] = json!(row.doc_id);
    if include_body {
        value["request_json"] = raw.get("request_json").cloned().unwrap_or(Value::Null);
        value["provenance_json"] = json!(row.provenance_json);
    }
    value
}

fn capture_order_padded(row: &gents::run_timeline::TimelineRenderedRequestRow) -> String {
    use gents_protocol::rendered_request::{CaptureOrderKey, CaptureScope};

    let scope = row
        .capture_scope
        .as_deref()
        .and_then(|scope| scope.parse::<CaptureScope>().ok());
    match (scope, row.turn_index, row.attempt) {
        (Some(scope), Some(turn_index), Some(attempt)) => CaptureOrderKey {
            scope,
            turn_index,
            attempt,
        }
        .padded(),
        // '~' sorts after every padded key's alphabet, pushing unparseable
        // rows to the end.
        _ => format!("~{}", row.capture_key),
    }
}

fn write_or_print(output_file: Option<&std::path::Path>, value: &Value) -> Result<()> {
    if let Some(path) = output_file {
        write_json_output_file(path, value)?;
    } else {
        print_json(value)?;
    }
    Ok(())
}

async fn trace_timeline(args: TraceTimelineArgs) -> Result<()> {
    let (access, _home_dir) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let timeline = load_run_timeline(&access, &args.request_id).await?;
    let value = serde_json::to_value(&timeline)?;
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &value)?;
    } else {
        print_json(&value)?;
    }
    Ok(())
}

async fn trace_project(args: TraceProjectArgs) -> Result<()> {
    let (access, _home_dir) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let actor_did = args.actor_did;
    let projection_kind = adapter_projection_kind(args.projection);
    let scope = projection_acp::ProjectionDocumentScope {
        agent_did: optional_scope_arg("scope-agent-did", args.scope_agent_did)?,
        behavior_id: optional_scope_arg("scope-behavior-id", args.scope_behavior_id)?,
        session_id: optional_scope_arg("scope-session-id", args.scope_session_id)?,
    };
    let rows = load_run_timeline_rows(&access, &args.request_id).await?;
    let acp_scope = projection_acp::projection_acp_read_scope(
        &access,
        args.acp_policy_id.as_deref(),
        actor_did.as_deref(),
        projection_kind,
        &rows.request,
    )
    .await?;
    let rows = match acp_scope.as_ref() {
        Some(acp_scope) => {
            projection_acp::apply_projection_acp_read_filter(rows, acp_scope).await?
        }
        None => rows,
    };
    let timeline =
        projection_acp::apply_projection_document_scope(build_run_timeline(rows), &scope)?;
    let context = ProjectionContext {
        actor_did,
        redaction_mode: projection_redaction_mode(args.redaction),
    };
    let projection = build_adapter_projection(projection_kind, &timeline, &context);
    validate_adapter_projection_contract(&projection)?;
    match args.format {
        TraceProjectionFormatArg::Json => {
            let value = serde_json::to_value(&projection)?;
            if let Some(path) = args.output_file.as_deref() {
                write_json_output_file(path, &value)?;
            } else {
                print_json(&value)?;
            }
        }
        TraceProjectionFormatArg::NativeJson => {
            let value = adapter_projection_native_json(&projection);
            if let Some(path) = args.output_file.as_deref() {
                write_json_output_file(path, &value)?;
            } else {
                print_json(&value)?;
            }
        }
        TraceProjectionFormatArg::Jsonl => {
            let records = adapter_projection_jsonl_records(&projection);
            write_jsonl(args.output_file.as_deref(), &records)?;
        }
        TraceProjectionFormatArg::EvalJsonl => {
            let records = adapter_projection_eval_jsonl_records(&projection);
            write_jsonl(args.output_file.as_deref(), &records)?;
        }
    }
    Ok(())
}

fn trace_project_schema(args: TraceProjectSchemaArgs) -> Result<()> {
    let kind = adapter_projection_kind(args.projection);
    let schema = match args.format {
        TraceProjectionFormatArg::Json => adapter_projection_json_schema(kind),
        TraceProjectionFormatArg::NativeJson => adapter_projection_native_json_schema(kind),
        TraceProjectionFormatArg::Jsonl => adapter_projection_jsonl_record_schema(kind),
        TraceProjectionFormatArg::EvalJsonl => adapter_projection_eval_jsonl_record_schema(kind),
    };
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &schema)?;
    } else {
        print_json(&schema)?;
    }
    Ok(())
}

fn optional_scope_arg(field: &str, value: Option<String>) -> Result<Option<String>> {
    value
        .map(|value| crate::require_non_empty(field, &value).map(ToOwned::to_owned))
        .transpose()
}

fn adapter_projection_kind(arg: TraceProjectionArg) -> AdapterProjectionKind {
    match arg {
        TraceProjectionArg::Atif => AdapterProjectionKind::AtifTrajectory,
        TraceProjectionArg::OpenaiCodex => AdapterProjectionKind::OpenAiCodexRunTrace,
        TraceProjectionArg::Langgraph => AdapterProjectionKind::LangGraphStateHistory,
        TraceProjectionArg::MultiAgent => AdapterProjectionKind::MultiAgentTask,
    }
}

fn projection_redaction_mode(arg: TraceProjectionRedactionArg) -> ProjectionRedactionMode {
    match arg {
        TraceProjectionRedactionArg::Full => ProjectionRedactionMode::Full,
        TraceProjectionRedactionArg::TrainingSafe => ProjectionRedactionMode::TrainingSafe,
        TraceProjectionRedactionArg::Public => ProjectionRedactionMode::Public,
    }
}

async fn trace_export(args: TraceExportArgs) -> Result<()> {
    let (access, _home_dir) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let requested_request = match args.request_id.as_deref() {
        Some(request_id) => Some(load_request_by_id(&access, request_id).await?),
        None => None,
    };
    if let (Some(session_id), Some(request)) = (args.session_id.as_deref(), &requested_request) {
        if request.session_id.as_deref() != Some(session_id) {
            anyhow::bail!(
                "--session-id {session_id} does not match request {} session_id={}",
                request.request_id,
                request.session_id.as_deref().unwrap_or("")
            );
        }
    }

    let session_filter = args.session_id.as_deref().or_else(|| {
        requested_request
            .as_ref()
            .and_then(|request| request.session_id.as_deref())
    });
    let tool_calls = load_tool_calls(&access, args.limit, session_filter).await?;
    if tool_calls.is_empty() {
        write_jsonl::<AmyToolCallTraceRecord>(args.output_file.as_deref(), &[])?;
        return Ok(());
    }

    let session_ids = unique_tool_call_session_ids(&tool_calls);
    let messages = load_messages_for_tool_calls(&access, &tool_calls).await?;
    let mut requests = load_requests_for_sessions(&access, &session_ids).await?;
    if let Some(request) = requested_request {
        if !requests
            .iter()
            .any(|row| row.request_id == request.request_id)
        {
            requests.push(request);
        }
    }
    let responses = load_responses_for_sessions(&access, &session_ids).await?;
    let sessions = load_sessions(&access, &session_ids).await?;
    let conversations = load_conversations(&access, &session_ids).await?;
    let behaviors = load_behaviors(&access, &requests, &sessions, &conversations).await?;

    let records = build_records(
        &tool_calls,
        &messages,
        &requests,
        &responses,
        &sessions,
        &conversations,
        &behaviors,
        &args,
    )?;
    let records = match args.request_id.as_deref() {
        Some(request_id) => records
            .into_iter()
            .filter(|record| record.request_id.as_deref() == Some(request_id))
            .collect::<Vec<_>>(),
        None => records,
    };

    write_jsonl(args.output_file.as_deref(), &records)
}

fn build_records(
    tool_calls: &[ToolCallRow],
    messages: &HashMap<(String, i64), MessageRow>,
    requests: &[AgentRequestRow],
    responses: &[ResponseRow],
    sessions: &HashMap<String, SessionRow>,
    conversations: &HashMap<String, ConversationRow>,
    behaviors: &HashMap<String, BehaviorRow>,
    args: &TraceExportArgs,
) -> Result<Vec<AmyToolCallTraceRecord>> {
    let requests_by_session = rows_by_session(requests);
    let responses_by_session = rows_by_session(responses);
    let responses_by_request = responses
        .iter()
        .map(|row| (row.request_id.clone(), row))
        .collect::<HashMap<_, _>>();

    tool_calls
        .iter()
        .map(|tool_call| -> Result<AmyToolCallTraceRecord> {
            let session_requests = requests_by_session
                .get(tool_call.session_id.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let session_responses = responses_by_session
                .get(tool_call.session_id.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let request =
                infer_request_for_tool_call(tool_call, session_requests, session_responses);
            let response = request.and_then(|request| {
                responses_by_request
                    .get(request.request_id.as_str())
                    .copied()
            });
            let request_failure = combined_request_failure_text(request, response);
            let request_failure_class = analyze_request_failure(request_failure.as_deref());
            let lifecycle_state_text = tool_call
                .lifecycle_state
                .as_deref()
                .filter(|state| !state.trim().is_empty())
                .with_context(|| {
                    format!(
                        "tool call {} is missing lifecycle_state",
                        tool_call.tool_call_id
                    )
                })?;
            let lifecycle_state = ToolCallState::from_persisted(lifecycle_state_text)
                .with_context(|| {
                    format!(
                        "tool call {} has invalid lifecycle_state {lifecycle_state_text:?}",
                        tool_call.tool_call_id
                    )
                })?;
            let analysis = analyze_tool_call_with_persisted_outcome(
                &tool_call.tool_name,
                &tool_call.args,
                &tool_call.result,
                lifecycle_state,
                tool_call.tool_failure_class.as_deref(),
            );
            let message = tool_call
                .message_sequence
                .and_then(|sequence| messages.get(&(tool_call.session_id.clone(), sequence)));
            let raw_assistant_message = message.map(|message| raw_message_json(&message.content));
            let raw_tool_call_json = message.and_then(|message| {
                extract_raw_tool_call_json(
                    &message.role,
                    &message.content,
                    &tool_call.tool_call_id,
                    &tool_call.tool_name,
                )
            });
            let session = sessions.get(tool_call.session_id.as_str());
            let conversation = conversations.get(tool_call.session_id.as_str());
            let behavior_id = first_nonempty([
                request.and_then(|request| request.behavior_id.as_deref()),
                conversation.and_then(|conversation| conversation.behavior_id.as_deref()),
                session.and_then(|session| session.behavior_id.as_deref()),
            ]);
            let behavior = behavior_id.and_then(|behavior_id| behaviors.get(behavior_id));
            let metadata = request.and_then(parse_request_metadata);
            let run_id = args
                .run_id
                .clone()
                .or_else(|| metadata_string(metadata.as_ref(), "run_id"))
                .or_else(|| metadata_string(metadata.as_ref(), "runId"));
            let case_id = args
                .case_id
                .clone()
                .or_else(|| metadata_string(metadata.as_ref(), "case_id"))
                .or_else(|| metadata_string(metadata.as_ref(), "caseId"));
            let backend_id = first_nonempty([
                request.and_then(|request| request.backend_id.as_deref()),
                behavior.and_then(|behavior| behavior.backend_id.as_deref()),
            ]);
            let agent_did = first_nonempty([
                request.and_then(|request| request.agent_did.as_deref()),
                conversation.and_then(|conversation| conversation.agent_did.as_deref()),
                behavior.and_then(|behavior| behavior.agent_did.as_deref()),
            ]);

            Ok(AmyToolCallTraceRecord {
                run_id,
                case_id,
                prompt: request.and_then(|request| request.content.clone()),
                agent_did: agent_did.map(ToOwned::to_owned),
                behavior_id: behavior_id.map(ToOwned::to_owned),
                session_id: tool_call.session_id.clone(),
                request_id: request.map(|request| request.request_id.clone()),
                request_status: request
                    .and_then(|request| request.lifecycle_state)
                    .map(|state| state.as_str().to_string()),
                request_failure_reason: request.and_then(|request| request.failure_reason.clone()),
                response_status: response.and_then(|response| response.status.clone()),
                response_error_message: response
                    .and_then(|response| response.error_message.clone()),
                request_failure_class,
                backend_id: backend_id.map(ToOwned::to_owned),
                model_name: behavior.and_then(|behavior| behavior.model_name.clone()),
                inference_profile_id: behavior
                    .and_then(|behavior| behavior.inference_profile_id.clone()),
                raw_assistant_message,
                raw_tool_call_json,
                tool_call_id: tool_call.tool_call_id.clone(),
                native_or_meta_tool: tool_call.tool_name.clone(),
                selected_service_id: analysis.selected_service_id,
                selected_tool_name: analysis.selected_tool_name,
                raw_arguments: tool_call.args.clone(),
                argument_parse_result: analysis.argument_parse_result,
                schema_validation_result: analysis.schema_validation_result,
                validation_errors: analysis.validation_errors,
                repair_attempt: None,
                final_arguments_sent: analysis.final_arguments_sent,
                tool_result: tool_call.result.clone(),
                native_tool_output: analysis.native_tool_output,
                tool_result_ok: analysis.tool_result_ok,
                tool_call_completed: lifecycle_state == ToolCallState::Completed,
                tool_status: lifecycle_state_text.to_string(),
                task_outcome: None,
                tool_failure_class: analysis.tool_failure_class,
                tool_error: analysis.tool_error,
                failure_class: analysis.tool_failure_class,
                started_at: tool_call.started_at.clone(),
                completed_at: tool_call.completed_at.clone(),
                latency_ms: latency_ms(
                    tool_call.started_at.as_deref(),
                    tool_call.completed_at.as_deref(),
                ),
                retry_count: request.and_then(|request| request.retry_count),
            })
        })
        .collect()
}

async fn load_tool_calls(
    access: &ConfigAccess,
    limit: usize,
    session_id: Option<&str>,
) -> Result<Vec<ToolCallRow>> {
    let filter = session_id
        .map(|session_id| {
            format!(
                r#"filter: {{ session_id: {{ _eq: "{}" }} }}, "#,
                escape_graphql_string(session_id)
            )
        })
        .unwrap_or_default();
    let query = format!(
        r#"{{
            AgentToolCall(
                {filter}
                order: {{ started_at: DESC }},
                limit: {limit}
            ) {{
                session_id
                request_id
                message_sequence
                tool_name
                tool_call_id
                args
                result
                lifecycle_state
                tool_failure_class
                started_at
                completed_at
            }}
        }}"#
    );
    load_rows(access, "AgentToolCall", &query).await
}

async fn load_request_by_id(access: &ConfigAccess, request_id: &str) -> Result<AgentRequestRow> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: DESC }},
                limit: 2
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    let mut rows = load_rows::<AgentRequestRow>(access, "AgentRequest", &query).await?;
    match rows.len() {
        0 => Err(anyhow::anyhow!("request {request_id} not found")),
        1 => Ok(rows.remove(0)),
        count => anyhow::bail!(
            "request_id {request_id} is ambiguous across {count} AgentRequest documents"
        ),
    }
}

async fn load_requests_for_sessions(
    access: &ConfigAccess,
    session_ids: &[String],
) -> Result<Vec<AgentRequestRow>> {
    if session_ids.is_empty() {
        return Ok(Vec::new());
    }
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _in: {} }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
            }}
        }}"#,
        graphql_string_list_literal(session_ids)
    );
    load_rows(access, "AgentRequest", &query).await
}

async fn load_responses_for_sessions(
    access: &ConfigAccess,
    session_ids: &[String],
) -> Result<Vec<ResponseRow>> {
    if session_ids.is_empty() {
        return Ok(Vec::new());
    }
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ session_id: {{ _in: {} }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                session_id
                status
                error_message
                materialized_message_sequence
            }}
        }}"#,
        graphql_string_list_literal(session_ids)
    );
    load_rows(access, "AgentResponse", &query).await
}

async fn load_sessions(
    access: &ConfigAccess,
    session_ids: &[String],
) -> Result<HashMap<String, SessionRow>> {
    if session_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _in: {} }} }}
            ) {{
                session_id
                behavior_id
            }}
        }}"#,
        graphql_string_list_literal(session_ids)
    );
    Ok(load_rows::<SessionRow>(access, "AgentSession", &query)
        .await?
        .into_iter()
        .map(|row| (row.session_id.clone(), row))
        .collect())
}

async fn load_conversations(
    access: &ConfigAccess,
    session_ids: &[String],
) -> Result<HashMap<String, ConversationRow>> {
    if session_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _in: {} }} }}
            ) {{
                session_id
                agent_did
                behavior_id
            }}
        }}"#,
        graphql_string_list_literal(session_ids)
    );
    Ok(
        load_rows::<ConversationRow>(access, "AgentConversation", &query)
            .await?
            .into_iter()
            .map(|row| (row.session_id.clone(), row))
            .collect(),
    )
}

async fn load_messages_for_tool_calls(
    access: &ConfigAccess,
    tool_calls: &[ToolCallRow],
) -> Result<HashMap<(String, i64), MessageRow>> {
    let mut sequences_by_session: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    for tool_call in tool_calls {
        if let Some(sequence) = tool_call.message_sequence {
            sequences_by_session
                .entry(tool_call.session_id.clone())
                .or_default()
                .insert(sequence);
        }
    }

    let mut out = HashMap::new();
    for (session_id, sequences) in sequences_by_session {
        let sequence_values = sequences.into_iter().collect::<Vec<_>>();
        let query = format!(
            r#"{{
                AgentMessage(
                    filter: {{
                        _and: [
                            {{ session_id: {{ _eq: "{}" }} }},
                            {{ sequence: {{ _in: {} }} }}
                        ]
                    }}
                ) {{
                    session_id
                    sequence
                    role
                    content
                }}
            }}"#,
            escape_graphql_string(&session_id),
            graphql_int_list_literal(&sequence_values)
        );
        for row in load_rows::<MessageRow>(access, "AgentMessage", &query).await? {
            out.insert((row.session_id.clone(), row.sequence), row);
        }
    }
    Ok(out)
}

async fn load_behaviors(
    access: &ConfigAccess,
    requests: &[AgentRequestRow],
    sessions: &HashMap<String, SessionRow>,
    conversations: &HashMap<String, ConversationRow>,
) -> Result<HashMap<String, BehaviorRow>> {
    let mut behavior_ids = BTreeSet::new();
    for request in requests {
        if let Some(behavior_id) = request
            .behavior_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            behavior_ids.insert(behavior_id.to_string());
        }
    }
    for session in sessions.values() {
        if let Some(behavior_id) = session
            .behavior_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            behavior_ids.insert(behavior_id.to_string());
        }
    }
    for conversation in conversations.values() {
        if let Some(behavior_id) = conversation
            .behavior_id
            .as_deref()
            .filter(|value| !value.is_empty())
        {
            behavior_ids.insert(behavior_id.to_string());
        }
    }
    if behavior_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let behavior_ids = behavior_ids.into_iter().collect::<Vec<_>>();
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _in: {} }} }}
            ) {{
                behavior_id
                agent_did
                backend_id
                model_name
                inference_profile_id
            }}
        }}"#,
        graphql_string_list_literal(&behavior_ids)
    );
    Ok(load_rows::<BehaviorRow>(access, "AgentBehavior", &query)
        .await?
        .into_iter()
        .map(|row| (row.behavior_id.clone(), row))
        .collect())
}

async fn load_rows<T>(access: &ConfigAccess, collection: &str, query: &str) -> Result<Vec<T>>
where
    T: DeserializeOwned,
{
    graphql_rows_or_empty_if_collection_missing(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

fn infer_request_for_tool_call<'a>(
    tool_call: &ToolCallRow,
    requests: &[&'a AgentRequestRow],
    responses: &[&ResponseRow],
) -> Option<&'a AgentRequestRow> {
    if let Some(request_id) = tool_call
        .request_id
        .as_deref()
        .filter(|request_id| !request_id.is_empty())
    {
        // AgentToolCall.request_id is the durable provenance edge. If it names
        // a request that was not loaded, do not silently reassign the call by
        // timestamp or message position.
        return requests
            .iter()
            .copied()
            .find(|request| request.request_id == request_id);
    }

    if let Some(sequence) = tool_call.message_sequence {
        if let Some(response) = responses
            .iter()
            .filter_map(|response| {
                let materialized = response.materialized_message_sequence?;
                (materialized >= sequence).then_some((*response, materialized))
            })
            .min_by_key(|(_, materialized)| *materialized)
            .map(|(response, _)| response)
        {
            if let Some(request) = requests
                .iter()
                .copied()
                .find(|request| request.request_id == response.request_id)
            {
                return Some(request);
            }
        }
    }

    if let Some(started_at) = tool_call
        .started_at
        .as_deref()
        .and_then(parse_rfc3339_millis)
    {
        if let Some(request) = requests
            .iter()
            .copied()
            .filter_map(|request| {
                let created_at = request
                    .created_at
                    .as_deref()
                    .and_then(parse_rfc3339_millis)?;
                (created_at <= started_at).then_some((request, created_at))
            })
            .max_by_key(|(_, created_at)| *created_at)
            .map(|(request, _)| request)
        {
            return Some(request);
        }
    }

    if requests.len() == 1 {
        return requests.first().copied();
    }

    None
}

fn rows_by_session<T: HasSessionId>(rows: &[T]) -> HashMap<&str, Vec<&T>> {
    let mut out: HashMap<&str, Vec<&T>> = HashMap::new();
    for row in rows {
        if let Some(session_id) = row.session_id() {
            out.entry(session_id).or_default().push(row);
        }
    }
    out
}

fn combined_request_failure_text(
    request: Option<&AgentRequestRow>,
    response: Option<&ResponseRow>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(request) = request {
        push_nonempty(&mut parts, request.lifecycle_state.map(|s| s.as_str()));
        push_nonempty(&mut parts, request.failure_reason.as_deref());
    }
    if let Some(response) = response {
        push_nonempty(&mut parts, response.status.as_deref());
        push_nonempty(&mut parts, response.error_message.as_deref());
    }

    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn parse_request_metadata(request: &AgentRequestRow) -> Option<Value> {
    let metadata = request.metadata.as_deref()?.trim();
    if metadata.is_empty() {
        return None;
    }
    serde_json::from_str(metadata).ok()
}

fn metadata_string(metadata: Option<&Value>, key: &str) -> Option<String> {
    metadata?
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn first_nonempty<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Option<&'a str> {
    values
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
}

fn push_nonempty(parts: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        parts.push(value.to_string());
    }
}

fn parse_rfc3339_millis(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn unique_tool_call_session_ids(tool_calls: &[ToolCallRow]) -> Vec<String> {
    tool_calls
        .iter()
        .filter_map(|row| {
            let session_id = row.session_id.trim();
            (!session_id.is_empty()).then_some(session_id.to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn graphql_int_list_literal(values: &[i64]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn write_jsonl<T: Serialize>(path: Option<&std::path::Path>, records: &[T]) -> Result<()> {
    let mut output = String::new();
    for record in records {
        output.push_str(&serde_json::to_string(record)?);
        output.push('\n');
    }

    if let Some(path) = path {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating output directory {}", parent.display()))?;
        }
        fs::write(path, output).with_context(|| format!("writing JSONL {}", path.display()))?;
    } else {
        print!("{output}");
    }
    Ok(())
}

trait HasSessionId {
    fn session_id(&self) -> Option<&str>;
}

#[derive(Debug, Clone, Deserialize)]
struct ToolCallRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    message_sequence: Option<i64>,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_call_id: String,
    #[serde(default)]
    args: String,
    #[serde(default)]
    result: String,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    tool_failure_class: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

impl HasSessionId for AgentRequestRow {
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    error_message: Option<String>,
    #[serde(default)]
    materialized_message_sequence: Option<i64>,
}

impl HasSessionId for ResponseRow {
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

#[derive(Debug, Clone, Deserialize)]
struct MessageRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    sequence: i64,
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    behavior_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConversationRow {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    inference_profile_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_metadata_hydrates_run_and_case_ids() {
        let request = AgentRequestRow {
            request_id: "req-1".to_string(),
            metadata: Some(r#"{"run_id":"run-1","case_id":"case-1"}"#.to_string()),
            ..empty_request()
        };
        let metadata = parse_request_metadata(&request);

        assert_eq!(
            metadata_string(metadata.as_ref(), "run_id").as_deref(),
            Some("run-1")
        );
        assert_eq!(
            metadata_string(metadata.as_ref(), "case_id").as_deref(),
            Some("case-1")
        );
    }

    #[test]
    fn infers_request_by_materialized_message_sequence() {
        let requests = vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                session_id: Some("session-1".to_string()),
                ..empty_request()
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                session_id: Some("session-1".to_string()),
                ..empty_request()
            },
        ];
        let request_refs = requests.iter().collect::<Vec<_>>();
        let responses = [
            ResponseRow {
                request_id: "req-1".to_string(),
                session_id: Some("session-1".to_string()),
                materialized_message_sequence: Some(4),
                ..empty_response()
            },
            ResponseRow {
                request_id: "req-2".to_string(),
                session_id: Some("session-1".to_string()),
                materialized_message_sequence: Some(8),
                ..empty_response()
            },
        ];
        let response_refs = responses.iter().collect::<Vec<_>>();
        let tool_call = ToolCallRow {
            session_id: "session-1".to_string(),
            message_sequence: Some(3),
            ..empty_tool_call()
        };

        let request = infer_request_for_tool_call(&tool_call, &request_refs, &response_refs)
            .expect("request");

        assert_eq!(request.request_id, "req-1");
    }

    fn empty_tool_call() -> ToolCallRow {
        ToolCallRow {
            session_id: String::new(),
            request_id: None,
            message_sequence: None,
            tool_name: String::new(),
            tool_call_id: String::new(),
            args: String::new(),
            result: String::new(),
            lifecycle_state: None,
            tool_failure_class: None,
            started_at: None,
            completed_at: None,
        }
    }

    fn empty_request() -> AgentRequestRow {
        serde_json::from_value(json!({ "request_id": "" }))
            .expect("canonical AgentRequest test row")
    }

    fn empty_response() -> ResponseRow {
        ResponseRow {
            request_id: String::new(),
            session_id: None,
            status: None,
            error_message: None,
            materialized_message_sequence: None,
        }
    }
}
