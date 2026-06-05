use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;

use anyhow::{Context, Result};
use defra_agent::adapter_projection::{
    adapter_projection_json_schema, adapter_projection_jsonl_record_schema,
    adapter_projection_jsonl_records, adapter_projection_training_jsonl_record_schema,
    adapter_projection_training_jsonl_records, build_adapter_projection,
    validate_adapter_projection_contract, AdapterProjectionKind, ProjectionContext,
    ProjectionRedactionMode,
};
use defra_agent::graphql::escape_graphql_string;
use defra_agent::run_timeline::{
    build_run_timeline, RunTimeline, RunTimelineEvent, RunTimelineRows, TimelineConversationRow,
    TimelineMessageRow, TimelineRequestEvent, TimelineRequestRow, TimelineResponseRow,
    TimelineSessionRow, TimelineToolCallEvent, TimelineToolCallRow,
};
use defra_agent::trace_export::{
    analyze_request_failure, analyze_tool_call, extract_raw_tool_call_json, latency_ms,
    raw_message_json, AmyToolCallTraceRecord,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cli::args::{
    TraceCommand, TraceExportArgs, TraceProjectArgs, TraceProjectSchemaArgs, TraceProjectionArg,
    TraceProjectionFormatArg, TraceProjectionRedactionArg, TraceTimelineArgs,
};
use crate::config_writes::ConfigAccess;
use crate::{
    graphql_rows_or_empty_if_collection_missing, graphql_string_list_literal, print_json,
    write_json_output_file,
};

pub(crate) async fn dispatch(command: TraceCommand) -> Result<()> {
    match command {
        TraceCommand::Export(args) => trace_export(args).await,
        TraceCommand::Timeline(args) => trace_timeline(args).await,
        TraceCommand::Project(args) => trace_project(args).await,
        TraceCommand::ProjectSchema(args) => trace_project_schema(args),
    }
}

async fn trace_timeline(args: TraceTimelineArgs) -> Result<()> {
    let (access, _home_dir) =
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
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
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
    let scope = ProjectionDocumentScope {
        agent_did: optional_scope_arg("scope-agent-did", args.scope_agent_did)?,
        behavior_id: optional_scope_arg("scope-behavior-id", args.scope_behavior_id)?,
        session_id: optional_scope_arg("scope-session-id", args.scope_session_id)?,
    };
    let timeline = apply_projection_document_scope(
        load_run_timeline(&access, &args.request_id).await?,
        &scope,
    )?;
    let context = ProjectionContext {
        actor_did: args.actor_did,
        redaction_mode: projection_redaction_mode(args.redaction),
    };
    let projection = build_adapter_projection(
        adapter_projection_kind(args.projection),
        &timeline,
        &context,
    );
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
        TraceProjectionFormatArg::Jsonl => {
            let records = adapter_projection_jsonl_records(&projection);
            write_jsonl(args.output_file.as_deref(), &records)?;
        }
        TraceProjectionFormatArg::TrainingJsonl => {
            let records = adapter_projection_training_jsonl_records(&projection);
            write_jsonl(args.output_file.as_deref(), &records)?;
        }
    }
    Ok(())
}

fn trace_project_schema(args: TraceProjectSchemaArgs) -> Result<()> {
    let kind = adapter_projection_kind(args.projection);
    let schema = match args.format {
        TraceProjectionFormatArg::Json => adapter_projection_json_schema(kind),
        TraceProjectionFormatArg::Jsonl => adapter_projection_jsonl_record_schema(kind),
        TraceProjectionFormatArg::TrainingJsonl => {
            adapter_projection_training_jsonl_record_schema(kind)
        }
    };
    if let Some(path) = args.output_file.as_deref() {
        write_json_output_file(path, &schema)?;
    } else {
        print_json(&schema)?;
    }
    Ok(())
}

async fn load_run_timeline(access: &ConfigAccess, request_id: &str) -> Result<RunTimeline> {
    let request = load_timeline_request_by_id(access, request_id).await?;
    let session_id = request.session_id.as_deref();

    let mut requests = match session_id {
        Some(session_id) => load_timeline_requests_for_session(access, session_id).await?,
        None => Vec::new(),
    };
    merge_timeline_request(&mut requests, request.clone());
    for child in load_timeline_child_requests(access, &request.request_id).await? {
        merge_timeline_request(&mut requests, child);
    }

    let messages = match session_id {
        Some(session_id) => load_timeline_messages_for_session(access, session_id).await?,
        None => Vec::new(),
    };
    let tool_calls = match session_id {
        Some(session_id) => load_timeline_tool_calls_for_session(access, session_id).await?,
        None => Vec::new(),
    };
    let responses = match session_id {
        Some(session_id) => load_timeline_responses_for_session(access, session_id).await?,
        None => load_timeline_responses_for_request(access, &request.request_id).await?,
    };
    let session = match session_id {
        Some(session_id) => load_timeline_session(access, session_id).await?,
        None => None,
    };
    let conversation = match session_id {
        Some(session_id) => load_timeline_conversation(access, session_id).await?,
        None => None,
    };

    Ok(build_run_timeline(RunTimelineRows {
        request,
        session,
        conversation,
        requests,
        messages,
        tool_calls,
        responses,
    }))
}

#[derive(Debug, Default)]
struct ProjectionDocumentScope {
    agent_did: Option<String>,
    behavior_id: Option<String>,
    session_id: Option<String>,
}

impl ProjectionDocumentScope {
    fn has_filters(&self) -> bool {
        self.agent_did.is_some() || self.behavior_id.is_some() || self.session_id.is_some()
    }

    fn description(&self) -> String {
        let mut parts = Vec::new();
        if let Some(agent_did) = self.agent_did.as_deref() {
            parts.push(format!("agent_did={agent_did}"));
        }
        if let Some(behavior_id) = self.behavior_id.as_deref() {
            parts.push(format!("behavior_id={behavior_id}"));
        }
        if let Some(session_id) = self.session_id.as_deref() {
            parts.push(format!("session_id={session_id}"));
        }
        parts.join(", ")
    }
}

fn apply_projection_document_scope(
    mut timeline: RunTimeline,
    scope: &ProjectionDocumentScope,
) -> Result<RunTimeline> {
    if !scope.has_filters() {
        return Ok(timeline);
    }

    if !timeline_root_matches_scope(&timeline, scope) {
        anyhow::bail!(
            "projection scope denied request {} for {}",
            timeline.request_id,
            scope.description()
        );
    }

    let allowed_request_ids = scoped_request_ids(&timeline, scope);
    timeline.events.retain(|event| {
        should_keep_scoped_timeline_event(event, &timeline.request_id, &allowed_request_ids, scope)
    });
    Ok(timeline)
}

fn timeline_root_matches_scope(timeline: &RunTimeline, scope: &ProjectionDocumentScope) -> bool {
    scope_value_matches(
        scope.agent_did.as_deref(),
        [
            timeline.request.agent_did.as_deref(),
            timeline.agent_did.as_deref(),
        ],
    ) && scope_value_matches(
        scope.behavior_id.as_deref(),
        [
            timeline.request.behavior_id.as_deref(),
            timeline.behavior_id.as_deref(),
            timeline
                .session
                .as_ref()
                .and_then(|session| session.behavior_id.as_deref()),
        ],
    ) && scope_value_matches(
        scope.session_id.as_deref(),
        [
            timeline.request.session_id.as_deref(),
            timeline.session_id.as_deref(),
        ],
    )
}

fn scoped_request_ids(timeline: &RunTimeline, scope: &ProjectionDocumentScope) -> BTreeSet<String> {
    let mut allowed = BTreeSet::from([timeline.request_id.clone()]);
    for event in &timeline.events {
        if let RunTimelineEvent::Request(request) = event {
            if request_event_matches_scope(request, scope) {
                allowed.insert(request.request_id.clone());
            }
        }
    }
    allowed
}

fn request_event_matches_scope(
    request: &TimelineRequestEvent,
    scope: &ProjectionDocumentScope,
) -> bool {
    scope_value_matches(scope.agent_did.as_deref(), [request.agent_did.as_deref()])
        && scope_value_matches(
            scope.behavior_id.as_deref(),
            [request.behavior_id.as_deref()],
        )
        && scope_value_matches(scope.session_id.as_deref(), [request.session_id.as_deref()])
}

fn should_keep_scoped_timeline_event(
    event: &RunTimelineEvent,
    root_request_id: &str,
    allowed_request_ids: &BTreeSet<String>,
    scope: &ProjectionDocumentScope,
) -> bool {
    match event {
        RunTimelineEvent::Request(request) => {
            request.request_id == root_request_id
                || allowed_request_ids.contains(&request.request_id)
                || request
                    .parent_request_id
                    .as_deref()
                    .is_some_and(|parent_request_id| {
                        allowed_request_ids.contains(parent_request_id)
                    })
        }
        RunTimelineEvent::Message(message) => scoped_request_id_allowed(
            message.request_id.as_deref(),
            Some(message.session_id.as_str()),
            allowed_request_ids,
            scope,
        ),
        RunTimelineEvent::ToolCall(tool_call) => {
            scoped_tool_call_allowed(tool_call, allowed_request_ids, scope)
        }
        RunTimelineEvent::Response(response) => allowed_request_ids.contains(&response.request_id),
    }
}

fn scoped_tool_call_allowed(
    tool_call: &TimelineToolCallEvent,
    allowed_request_ids: &BTreeSet<String>,
    scope: &ProjectionDocumentScope,
) -> bool {
    scoped_request_id_allowed(
        tool_call.request_id.as_deref(),
        Some(tool_call.session_id.as_str()),
        allowed_request_ids,
        scope,
    )
}

fn scoped_request_id_allowed(
    request_id: Option<&str>,
    session_id: Option<&str>,
    allowed_request_ids: &BTreeSet<String>,
    scope: &ProjectionDocumentScope,
) -> bool {
    request_id
        .map(|request_id| allowed_request_ids.contains(request_id))
        .unwrap_or_else(|| {
            scope.agent_did.is_none()
                && scope.behavior_id.is_none()
                && scope_value_matches(scope.session_id.as_deref(), [session_id])
        })
}

fn scope_value_matches<'a>(
    expected: Option<&str>,
    actual_values: impl IntoIterator<Item = Option<&'a str>>,
) -> bool {
    let Some(expected) = expected.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    actual_values
        .into_iter()
        .flatten()
        .any(|actual| actual.trim() == expected)
}

fn optional_scope_arg(field: &str, value: Option<String>) -> Result<Option<String>> {
    value
        .map(|value| crate::require_non_empty(field, &value).map(ToOwned::to_owned))
        .transpose()
}

fn adapter_projection_kind(arg: TraceProjectionArg) -> AdapterProjectionKind {
    match arg {
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
        crate::resolve_config_access(args.home.as_deref(), args.graphql.as_deref(), false).await?;
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
    );
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
    requests: &[RequestRow],
    responses: &[ResponseRow],
    sessions: &HashMap<String, SessionRow>,
    conversations: &HashMap<String, ConversationRow>,
    behaviors: &HashMap<String, BehaviorRow>,
    args: &TraceExportArgs,
) -> Vec<AmyToolCallTraceRecord> {
    let requests_by_session = rows_by_session(requests);
    let responses_by_session = rows_by_session(responses);
    let responses_by_request = responses
        .iter()
        .map(|row| (row.request_id.clone(), row))
        .collect::<HashMap<_, _>>();

    tool_calls
        .iter()
        .map(|tool_call| {
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
            let analysis = analyze_tool_call(
                &tool_call.tool_name,
                &tool_call.args,
                &tool_call.result,
                &tool_call.status,
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

            AmyToolCallTraceRecord {
                run_id,
                case_id,
                prompt: request.and_then(|request| request.content.clone()),
                agent_did: agent_did.map(ToOwned::to_owned),
                behavior_id: behavior_id.map(ToOwned::to_owned),
                session_id: tool_call.session_id.clone(),
                request_id: request.map(|request| request.request_id.clone()),
                request_status: request.and_then(|request| request.status.clone()),
                request_lifecycle_state: request
                    .and_then(|request| request.lifecycle_state.clone()),
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
                tool_call_completed: tool_call.status.eq_ignore_ascii_case("completed"),
                tool_status: tool_call.status.clone(),
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
            }
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
                message_sequence
                tool_name
                tool_call_id
                args
                result
                status
                started_at
                completed_at
            }}
        }}"#
    );
    load_rows(access, "AgentToolCall", &query).await
}

async fn load_request_by_id(access: &ConfigAccess, request_id: &str) -> Result<RequestRow> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows::<RequestRow>(access, "AgentRequest", &query)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))
}

async fn load_timeline_request_by_id(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<TimelineRequestRow> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows::<TimelineRequestRow>(access, "AgentRequest", &query)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("request {request_id} not found"))
}

async fn load_timeline_requests_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineRequestRow>> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentRequest", &query).await
}

async fn load_timeline_child_requests(
    access: &ConfigAccess,
    parent_request_id: &str,
) -> Result<Vec<TimelineRequestRow>> {
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ caused_by_parent_request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                metadata
                status
                lifecycle_state
                backend_id
                failure_reason
                created_at
                retry_count
                interrupt_requested_at
                caused_by_parent_request_id
                caused_by_parent_tool_call_id
            }}
        }}"#,
        escape_graphql_string(parent_request_id)
    );
    load_rows(access, "AgentRequest", &query).await
}

async fn load_timeline_messages_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineMessageRow>> {
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                session_id
                sequence
                role
                content
                timestamp
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentMessage", &query).await
}

async fn load_timeline_tool_calls_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineToolCallRow>> {
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                request_id
                session_id
                message_sequence
                tool_name
                tool_call_id
                args
                result
                status
                lifecycle_state
                started_at
                deadline_at
                completed_at
                selected_service_id
                selected_tool_name
                tool_failure_class
                denial_reason
                denied_argv
                denied_command
                denied_argument
                denied_subcommand
                denied_prefix
                policy_mode
                policy_network
                latency_ms
                await_mode
                cancel_policy
                cancel_cause
                child_request_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentToolCall", &query).await
}

async fn load_timeline_responses_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineResponseRow>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                reasoning
                status
                error_message
                token_count
                progress_seq
                materialized_message_sequence
                materialized_at
                created_at
                completed_at
                interrupted_at
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    load_rows(access, "AgentResponse", &query).await
}

async fn load_timeline_responses_for_request(
    access: &ConfigAccess,
    request_id: &str,
) -> Result<Vec<TimelineResponseRow>> {
    let query = format!(
        r#"{{
            AgentResponse(
                filter: {{ request_id: {{ _eq: "{}" }} }},
                order: {{ created_at: ASC }}
            ) {{
                request_id
                agent_did
                behavior_id
                session_id
                content
                reasoning
                status
                error_message
                token_count
                progress_seq
                materialized_message_sequence
                materialized_at
                created_at
                completed_at
                interrupted_at
            }}
        }}"#,
        escape_graphql_string(request_id)
    );
    load_rows(access, "AgentResponse", &query).await
}

async fn load_timeline_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Option<TimelineSessionRow>> {
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                limit: 1
            ) {{
                session_id
                agent_name
                behavior_id
                started
                ended
                status
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    Ok(
        load_rows::<TimelineSessionRow>(access, "AgentSession", &query)
            .await?
            .into_iter()
            .next(),
    )
}

async fn load_timeline_conversation(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Option<TimelineConversationRow>> {
    let query = format!(
        r#"{{
            AgentConversation(
                filter: {{ session_id: {{ _eq: "{}" }} }},
                limit: 1
            ) {{
                session_id
                agent_name
                agent_did
                behavior_id
                title
                title_source
                preview_text
                status
                created_at
                updated_at
                latest_request_id
                forked_from_session_id
            }}
        }}"#,
        escape_graphql_string(session_id)
    );
    Ok(
        load_rows::<TimelineConversationRow>(access, "AgentConversation", &query)
            .await?
            .into_iter()
            .next(),
    )
}

fn merge_timeline_request(rows: &mut Vec<TimelineRequestRow>, request: TimelineRequestRow) {
    if !rows.iter().any(|row| row.request_id == request.request_id) {
        rows.push(request);
    }
}

async fn load_requests_for_sessions(
    access: &ConfigAccess,
    session_ids: &[String],
) -> Result<Vec<RequestRow>> {
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
                status
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
    requests: &[RequestRow],
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
    requests: &[&'a RequestRow],
    responses: &[&ResponseRow],
) -> Option<&'a RequestRow> {
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
    request: Option<&RequestRow>,
    response: Option<&ResponseRow>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(request) = request {
        push_nonempty(&mut parts, request.status.as_deref());
        push_nonempty(&mut parts, request.lifecycle_state.as_deref());
        push_nonempty(&mut parts, request.failure_reason.as_deref());
    }
    if let Some(response) = response {
        push_nonempty(&mut parts, response.status.as_deref());
        push_nonempty(&mut parts, response.error_message.as_deref());
    }

    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn parse_request_metadata(request: &RequestRow) -> Option<Value> {
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
    status: String,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RequestRow {
    #[serde(default)]
    request_id: String,
    #[serde(default)]
    agent_did: Option<String>,
    #[serde(default)]
    behavior_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    metadata: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<String>,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    failure_reason: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    retry_count: Option<i64>,
}

impl HasSessionId for RequestRow {
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
        let request = RequestRow {
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
            RequestRow {
                request_id: "req-1".to_string(),
                session_id: Some("session-1".to_string()),
                ..empty_request()
            },
            RequestRow {
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
            message_sequence: None,
            tool_name: String::new(),
            tool_call_id: String::new(),
            args: String::new(),
            result: String::new(),
            status: String::new(),
            started_at: None,
            completed_at: None,
        }
    }

    fn empty_request() -> RequestRow {
        RequestRow {
            request_id: String::new(),
            agent_did: None,
            behavior_id: None,
            session_id: None,
            content: None,
            metadata: None,
            status: None,
            lifecycle_state: None,
            backend_id: None,
            failure_reason: None,
            created_at: None,
            retry_count: None,
        }
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
