use std::collections::HashMap;
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
use gents::run_timeline::{build_run_timeline, RunTimelineRows, TimelineToolCallRow};
use gents::run_timeline_fetch::{load_run_timeline, load_run_timeline_rows};
use gents::tool_call_lifecycle::ToolCallState;
use gents::trace_export::{
    analyze_request_failure, analyze_tool_call_with_persisted_outcome, extract_raw_tool_call_json,
    latency_ms, raw_message_json, AmyToolCallTraceRecord,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::args::{
    TraceCaptureArgs, TraceCommand, TraceExportArgs, TraceProjectArgs, TraceProjectSchemaArgs,
    TraceProjectionArg, TraceProjectionFormatArg, TraceProjectionRedactionArg, TraceTimelineArgs,
};
use crate::config_writes::ConfigAccess;
use crate::{graphql_rows_or_empty_if_collection_missing, print_json, write_json_output_file};

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
    let mut timelines = HashMap::new();
    let requested = if let Some(request_id) = args.request_id.as_deref() {
        let rows = load_run_timeline_rows(&access, request_id).await?;
        if let Some(session_id) = args.session_id.as_deref() {
            anyhow::ensure!(
                rows.request.session_id.as_deref() == Some(session_id),
                "--session-id does not match the selected request"
            );
        }
        let physical = rows
            .request
            .doc_id
            .clone()
            .context("selected request has no physical document ID")?;
        timelines.insert(request_id.to_owned(), rows);
        Some(physical)
    } else {
        None
    };
    let filter = if let Some(doc_id) = requested.as_deref() {
        format!(
            r#"filter: {{request_doc_id: {{_eq: "{}"}}}},"#,
            escape_graphql_string(doc_id)
        )
    } else if let Some(session_id) = args.session_id.as_deref() {
        format!(
            r#"filter: {{session_id: {{_eq: "{}"}}}},"#,
            escape_graphql_string(session_id)
        )
    } else {
        String::new()
    };
    let query = format!(
        r#"{{AgentToolCall({filter} order:{{started_at:DESC}}, limit:{}) {{_docID request_doc_id request_id session_id message_sequence tool_name tool_call_id args result lifecycle_state tool_failure_class started_at completed_at}}}}"#,
        args.limit
    );
    let calls: Vec<TimelineToolCallRow> = load_rows(&access, "AgentToolCall", &query).await?;
    let mut records = Vec::with_capacity(calls.len());
    for call in calls {
        let rows = match (call.request_id.as_deref(), call.request_doc_id.as_deref()) {
            (Some(id), Some(_)) => {
                if !timelines.contains_key(id) {
                    timelines.insert(id.to_owned(), load_run_timeline_rows(&access, id).await?);
                }
                timelines.get(id)
            }
            (None, None) => None,
            _ => anyhow::bail!(
                "tool call {} has an incomplete request identity",
                call.tool_call_id
            ),
        };
        records.push(build_record(&call, rows, &args)?);
    }
    write_jsonl(args.output_file.as_deref(), &records)
}

fn build_record(
    tool_call: &TimelineToolCallRow,
    rows: Option<&RunTimelineRows>,
    args: &TraceExportArgs,
) -> Result<AmyToolCallTraceRecord> {
    let request = rows.map(|rows| &rows.request);
    if let Some(request) = request {
        anyhow::ensure!(
            tool_call.request_id.as_deref() == Some(request.request_id.as_str())
                && tool_call.request_doc_id.is_some()
                && tool_call.request_doc_id == request.doc_id
                && request.session_id.as_deref() == Some(tool_call.session_id.as_str()),
            "tool call {} does not belong to the selected physical request",
            tool_call.tool_call_id
        );
    }
    let response = rows.and_then(|rows| {
        rows.responses.iter().find(|response| {
            response.request_doc_id == rows.request.doc_id
                && response.request_id == rows.request.request_id
        })
    });
    let mut failure_parts = Vec::new();
    if let Some(request) = request {
        push_nonempty(
            &mut failure_parts,
            request.lifecycle_state.map(|state| state.as_str()),
        );
        push_nonempty(&mut failure_parts, request.failure_reason.as_deref());
    }
    if let Some(response) = response {
        push_nonempty(&mut failure_parts, response.status.as_deref());
        push_nonempty(&mut failure_parts, response.error_message.as_deref());
    }
    let request_failure = (!failure_parts.is_empty()).then(|| failure_parts.join("\n"));
    let request_failure_class = analyze_request_failure(request_failure.as_deref());
    let lifecycle_state_text = tool_call
        .lifecycle_state
        .as_deref()
        .context("tool call is missing lifecycle_state")?;
    let lifecycle_state = ToolCallState::from_persisted(lifecycle_state_text)
        .with_context(|| format!("invalid tool call lifecycle_state {lifecycle_state_text:?}"))?;
    let analysis = analyze_tool_call_with_persisted_outcome(
        &tool_call.tool_name,
        &tool_call.args,
        &tool_call.result,
        lifecycle_state,
        tool_call.tool_failure_class.as_deref(),
    );
    let message = rows.and_then(|rows| {
        tool_call.message_sequence.and_then(|sequence| {
            rows.messages.iter().find(|message| {
                message.sequence == sequence
                    && message.session_id == tool_call.session_id
                    && message.request_doc_id == rows.request.doc_id
                    && message.request_id.as_deref() == Some(rows.request.request_id.as_str())
            })
        })
    });
    let raw_assistant_message = message.map(|message| raw_message_json(&message.content));
    let raw_tool_call_json = message.and_then(|message| {
        extract_raw_tool_call_json(
            &message.role,
            &message.content,
            &tool_call.tool_call_id,
            &tool_call.tool_name,
        )
    });
    let model_name = rows
        .map(|rows| build_run_timeline(rows.clone()))
        .as_ref()
        .and_then(gents::adapter_projection::root_observed_model);
    let backend_id = rows.and_then(|rows| {
        let mut observed = rows.inference_calls.iter().filter(|call| {
            call.request_doc_id == rows.request.doc_id && call.request_id == rows.request.request_id
        });
        let first = observed.next()?.backend_id.as_ref()?;
        observed
            .all(|call| call.backend_id.as_ref() == Some(first))
            .then(|| first.clone())
    });
    Ok(AmyToolCallTraceRecord {
        run_id: args.run_id.clone(),
        case_id: args.case_id.clone(),
        prompt: request.and_then(|request| request.content.clone()),
        agent_did: request.and_then(|request| request.agent_did.clone()),
        behavior_id: request.and_then(|request| request.behavior_id.clone()),
        session_id: tool_call.session_id.clone(),
        request_id: request.map(|request| request.request_id.clone()),
        request_status: request
            .and_then(|request| request.lifecycle_state)
            .map(|state| state.as_str().to_string()),
        request_failure_reason: request.and_then(|request| request.failure_reason.clone()),
        response_status: response.and_then(|response| response.status.clone()),
        response_error_message: response.and_then(|response| response.error_message.clone()),
        request_failure_class,
        backend_id,
        model_name,
        inference_profile_id: None,
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
}

async fn load_rows<T: DeserializeOwned>(
    access: &ConfigAccess,
    collection: &str,
    query: &str,
) -> Result<Vec<T>> {
    graphql_rows_or_empty_if_collection_missing(access, collection, query)
        .await?
        .into_iter()
        .map(serde_json::from_value)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("decoding {collection} rows"))
}

fn push_nonempty(parts: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        parts.push(value.to_owned());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gents::run_timeline::{
        TimelineInferenceCallRow, TimelineMessageRow, TimelineRenderedRequestRow,
        TimelineRequestRow,
    };

    fn args() -> TraceExportArgs {
        TraceExportArgs {
            home: None,
            graphql: None,
            session_id: None,
            request_id: None,
            run_id: Some("run".into()),
            case_id: Some("case".into()),
            limit: 10,
            output_file: None,
        }
    }
    fn call() -> TimelineToolCallRow {
        TimelineToolCallRow {
            request_id: Some("request".into()),
            request_doc_id: Some("physical".into()),
            session_id: "session".into(),
            tool_call_id: "call".into(),
            tool_name: "bash".into(),
            args: "{}".into(),
            result: "done".into(),
            lifecycle_state: Some("completed".into()),
            message_sequence: Some(3),
            ..Default::default()
        }
    }
    fn rows() -> RunTimelineRows {
        RunTimelineRows {
            request: TimelineRequestRow {
                request_id: "request".into(),
                doc_id: Some("physical".into()),
                session_id: Some("session".into()),
                agent_did: Some("owner".into()),
                behavior_id: Some("behavior".into()),
                content: Some("prompt".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }
    #[test]
    fn export_requires_physical_request_identity_and_never_infers_orphans() {
        let mut wrong = rows();
        wrong.request.doc_id = Some("other".into());
        assert!(build_record(&call(), Some(&wrong), &args()).is_err());
        let mut orphan = call();
        orphan.request_id = None;
        orphan.request_doc_id = None;
        let record = build_record(&orphan, None, &args()).unwrap();
        assert!(record.request_id.is_none());
        assert!(record.agent_did.is_none());
        assert!(record.raw_assistant_message.is_none());
    }
    #[test]
    fn export_uses_only_matching_historical_observations_and_explicit_labels() {
        let mut rows = rows();
        rows.inference_calls = vec![
            TimelineInferenceCallRow {
                request_id: "request".into(),
                request_doc_id: Some("physical".into()),
                backend_id: Some("observed-backend".into()),
                ..Default::default()
            },
            TimelineInferenceCallRow {
                request_id: "request".into(),
                request_doc_id: Some("foreign".into()),
                backend_id: Some("foreign-backend".into()),
                ..Default::default()
            },
        ];
        rows.rendered_requests = vec![TimelineRenderedRequestRow {
            request_id: Some("request".into()),
            request_doc_id: Some("physical".into()),
            model_name: Some("observed-model".into()),
            ..Default::default()
        }];
        rows.messages.push(TimelineMessageRow {
            request_id: Some("request".into()),
            request_doc_id: Some("foreign".into()),
            session_id: "session".into(),
            sequence: 3,
            role: "assistant".into(),
            content: "foreign content".into(),
            ..Default::default()
        });
        let record = build_record(&call(), Some(&rows), &args()).unwrap();
        assert_eq!(record.run_id.as_deref(), Some("run"));
        assert_eq!(record.case_id.as_deref(), Some("case"));
        assert_eq!(record.backend_id.as_deref(), Some("observed-backend"));
        assert_eq!(record.model_name.as_deref(), Some("observed-model"));
        assert_eq!(record.prompt.as_deref(), Some("prompt"));
        assert!(record.inference_profile_id.is_none());
        assert!(record.raw_assistant_message.is_none());
        rows.inference_calls.push(TimelineInferenceCallRow {
            request_id: "request".into(),
            request_doc_id: Some("physical".into()),
            backend_id: Some("another-backend".into()),
            ..Default::default()
        });
        assert!(build_record(&call(), Some(&rows), &args())
            .unwrap()
            .backend_id
            .is_none());
    }
}
