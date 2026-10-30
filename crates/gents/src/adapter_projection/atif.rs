use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use gents_protocol::transcript::{normalize_markdown_text, present_message, PresentedMessageRole};

use crate::run_timeline::{
    RunTimeline, RunTimelineEvent, TimelineMessageEvent, TimelineToolCallEvent,
};

use super::redaction::{redact_json_value, redact_option, redact_str};
use super::ProjectionContext;

pub const ATIF_SCHEMA_VERSION: &str = "ATIF-v1.7";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifTrajectory {
    pub schema_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    pub agent: AtifAgent,
    pub steps: Vec<AtifStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<AtifFinalMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifAgent {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AtifStepSource {
    System,
    User,
    Agent,
}

impl AtifStepSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifStep {
    pub step_id: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: AtifStepSource,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<AtifToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<AtifObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    pub arguments: BTreeMap<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifObservation {
    pub results: Vec<AtifObservationResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifObservationResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AtifFinalMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cached_tokens: Option<u64>,
    pub total_steps: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<BTreeMap<String, Value>>,
}

pub(super) fn build_atif_trajectory(
    timeline: &RunTimeline,
    context: &ProjectionContext,
) -> AtifTrajectory {
    let mut steps = Vec::new();
    let mut consumed_tool_call_ids = BTreeSet::new();
    let mut emitted_request_input = false;

    if let Some(content) = timeline
        .request
        .content
        .as_deref()
        .filter(|content| !content.trim().is_empty())
    {
        steps.push(AtifStep {
            step_id: 0,
            timestamp: valid_timestamp(timeline.request.created_at.as_deref()),
            source: AtifStepSource::User,
            message: redact_str(content, context),
            reasoning_content: None,
            tool_calls: None,
            observation: None,
            llm_call_count: None,
            extra: optional_extra([
                ("request_id", string_value(&timeline.request_id)),
                (
                    "session_id",
                    optional_string_value(timeline.session_id.as_deref()),
                ),
                (
                    "lifecycle_state",
                    optional_string_value(timeline.request.lifecycle_state.map(|s| s.as_str())),
                ),
            ]),
        });
        emitted_request_input = true;
    }

    for event in &timeline.events {
        match event {
            RunTimelineEvent::Message(message) if root_message(timeline, message) => {
                let Some((source, content, reasoning)) = projected_message(message) else {
                    continue;
                };
                if emitted_request_input
                    && source == AtifStepSource::User
                    && timeline
                        .request
                        .content
                        .as_deref()
                        .is_some_and(|request_content| {
                            normalize_markdown_text(request_content) == content
                        })
                {
                    emitted_request_input = false;
                    continue;
                }

                let paired_tools = if source == AtifStepSource::Agent {
                    paired_tools_for_message(timeline, message)
                } else {
                    Vec::new()
                };
                consumed_tool_call_ids.extend(
                    paired_tools
                        .iter()
                        .map(|tool| tool.tool_call_id.as_str().to_string()),
                );
                steps.push(step_for_message(
                    message,
                    source,
                    &content,
                    reasoning.as_deref(),
                    paired_tools,
                    context,
                ));
            }
            RunTimelineEvent::ToolCall(tool)
                if root_tool(timeline, tool)
                    && !consumed_tool_call_ids.contains(&tool.tool_call_id)
                    && !tool_has_paired_message(timeline, tool) =>
            {
                consumed_tool_call_ids.insert(tool.tool_call_id.clone());
                steps.push(step_for_unpaired_tool(tool, context));
            }
            RunTimelineEvent::Request(_)
            | RunTimelineEvent::RenderedRequest(_)
            | RunTimelineEvent::InferenceCall(_)
            | RunTimelineEvent::Compaction(_)
            | RunTimelineEvent::ProviderContextReduction(_)
            | RunTimelineEvent::Message(_)
            | RunTimelineEvent::ToolCall(_)
            | RunTimelineEvent::GoalTransition(_) => {}
        }
    }

    if steps.is_empty() {
        steps.push(AtifStep {
            step_id: 1,
            timestamp: valid_timestamp(timeline.request.created_at.as_deref()),
            source: AtifStepSource::User,
            message: String::new(),
            reasoning_content: None,
            tool_calls: None,
            observation: None,
            llm_call_count: None,
            extra: optional_extra([("request_id", string_value(&timeline.request_id))]),
        });
    }
    for (index, step) in steps.iter_mut().enumerate() {
        step.step_id = index + 1;
    }

    let root_provider_calls = timeline
        .inference_calls
        .iter()
        .filter(|call| root_inference_call(timeline, call))
        .collect::<Vec<_>>();
    let inference_call_count = root_provider_calls
        .iter()
        .filter(|call| call.call_kind == "inference")
        .count();
    let tool_call_count = timeline
        .events
        .iter()
        .filter(
            |event| matches!(event, RunTimelineEvent::ToolCall(tool) if root_tool(timeline, tool)),
        )
        .count();
    let compaction_count = timeline
        .events
        .iter()
        .filter(|event| {
            matches!(event, RunTimelineEvent::Compaction(compaction) if compaction.request_id == timeline.request_id)
                || matches!(event, RunTimelineEvent::ProviderContextReduction(reduction) if reduction.request_id == timeline.request_id)
        })
        .count();
    let (total_prompt_tokens, total_completion_tokens, total_cached_tokens) =
        trajectory_usage_totals(timeline);
    let aggregate_token_budget_limit = timeline
        .request
        .max_total_tokens
        .and_then(|limit| u64::try_from(limit).ok())
        .filter(|limit| *limit > 0);
    // Match live rehydrate: budgeted-but-not-yet-charged is used=0, not "missing".
    let aggregate_token_budget_used = aggregate_token_budget_limit.and_then(|_| {
        crate::provider_usage::sum_charged_from_persisted_parts(
            root_provider_calls
                .iter()
                .map(|call| (call.prompt_tokens, call.completion_tokens)),
        )
        .ok()
    });
    let aggregate_token_budget_remaining = aggregate_token_budget_limit
        .zip(aggregate_token_budget_used)
        .map(|(limit, used)| crate::provider_usage::remaining_budget(limit, used));

    AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.to_string(),
        session_id: timeline
            .session_id
            .clone()
            .or_else(|| Some(timeline.request_id.clone())),
        trajectory_id: Some(timeline.request_id.clone()),
        agent: AtifAgent {
            name: timeline.agent_id.clone().unwrap_or_else(|| "gents".to_string()),
            version: env!("CARGO_PKG_VERSION").to_string(),
            model_name: root_observed_model(timeline),
            extra: optional_extra([
                ("runtime", string_value("gents")),
                ("node_did", optional_string_value(timeline.node_did.as_deref())),
                (
                    "agent_id",
                    optional_string_value(timeline.agent_id.as_deref()),
                ),
                (
                    "backend_id",
                    optional_string_value(timeline.request.backend_id.as_deref()),
                ),
            ]),
        },
        final_metrics: Some(AtifFinalMetrics {
            total_prompt_tokens,
            total_completion_tokens,
            total_cached_tokens,
            total_steps: steps.len(),
            extra: optional_extra([
                ("request_id", string_value(&timeline.request_id)),
                (
                    "lifecycle_state",
                    optional_string_value(timeline.request.lifecycle_state.map(|s| s.as_str())),
                ),
                ("inference_call_count", Some(json!(inference_call_count))),
                ("tool_call_count", Some(json!(tool_call_count))),
                ("compaction_count", Some(json!(compaction_count))),
                ("child_request_ids", Some(json!(timeline.child_request_ids))),
                // Durable request-wide budget observation (same charge rule as
                // loop_stream rehydrate / InferenceCall persist).
                (
                    "aggregate_token_budget_limit",
                    aggregate_token_budget_limit.map(|limit| json!(limit)),
                ),
                (
                    "aggregate_token_budget_used",
                    aggregate_token_budget_used.map(|used| json!(used)),
                ),
                (
                    "aggregate_token_budget_remaining",
                    aggregate_token_budget_remaining.map(|remaining| json!(remaining)),
                ),
            ]),
        }),
        steps,
        notes: Some(
            "Projected from Gents' persisted run timeline; token metrics and request-wide budget used/remaining come from durable InferenceCall rows under the same charge rule as live enforcement."
                .to_string(),
        ),
        extra: optional_extra([
            ("source_projection_id", string_value("run_timeline")),
            ("source_request_id", string_value(&timeline.request_id)),
            // Capture metadata only (keys, hashes, ordering, admission join).
            // ATIF `extra` bypasses redaction, which is safe here precisely
            // because bodies never enter the timeline this is derived from.
            (
                "rendered_captures",
                super::rendered_captures_json(timeline),
            ),
        ]),
    }
}

pub(super) fn validate_atif_trajectory(violations: &mut Vec<String>, trajectory: &AtifTrajectory) {
    if trajectory.schema_version != ATIF_SCHEMA_VERSION {
        violations.push(format!(
            "output.projection.schema_version expected {ATIF_SCHEMA_VERSION:?}, got {:?}",
            trajectory.schema_version
        ));
    }
    require_nonempty(
        violations,
        "output.projection.agent.name",
        &trajectory.agent.name,
    );
    require_nonempty(
        violations,
        "output.projection.agent.version",
        &trajectory.agent.version,
    );
    if trajectory.steps.is_empty() {
        violations.push("output.projection.steps must not be empty".to_string());
    }
    for (index, step) in trajectory.steps.iter().enumerate() {
        let expected_step_id = index + 1;
        if step.step_id != expected_step_id {
            violations.push(format!(
                "output.projection.steps[{index}].step_id expected {expected_step_id}, got {}",
                step.step_id
            ));
        }
        if let Some(timestamp) = step.timestamp.as_deref() {
            if chrono::DateTime::parse_from_rfc3339(timestamp).is_err() {
                violations.push(format!(
                    "output.projection.steps[{index}].timestamp must be ISO 8601"
                ));
            }
        }
        let tool_call_ids = step
            .tool_calls
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|tool| tool.tool_call_id.as_str())
            .collect::<BTreeSet<_>>();
        for (tool_index, tool) in step
            .tool_calls
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            require_nonempty(
                violations,
                &format!("output.projection.steps[{index}].tool_calls[{tool_index}].tool_call_id"),
                &tool.tool_call_id,
            );
            require_nonempty(
                violations,
                &format!("output.projection.steps[{index}].tool_calls[{tool_index}].function_name"),
                &tool.function_name,
            );
        }
        if let Some(observation) = step.observation.as_ref() {
            for (result_index, result) in observation.results.iter().enumerate() {
                if let Some(source_call_id) = result.source_call_id.as_deref() {
                    if !tool_call_ids.contains(source_call_id) {
                        violations.push(format!(
                            "output.projection.steps[{index}].observation.results[{result_index}].source_call_id {:?} does not reference a tool call in the same step",
                            source_call_id
                        ));
                    }
                }
            }
        }
    }
}

pub(super) fn atif_projection_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["schema_version", "agent", "steps"],
        "properties": {
            "schema_version": { "const": ATIF_SCHEMA_VERSION },
            "session_id": optional_string_schema(),
            "trajectory_id": optional_string_schema(),
            "agent": { "$ref": "#/$defs/agent" },
            "steps": {
                "type": "array",
                "minItems": 1,
                "items": { "$ref": "#/$defs/step" }
            },
            "notes": optional_string_schema(),
            "final_metrics": {
                "anyOf": [
                    { "$ref": "#/$defs/final_metrics" },
                    { "type": "null" }
                ]
            },
            "extra": optional_object_schema()
        },
        "$defs": {
            "agent": {
                "type": "object",
                "additionalProperties": false,
                "required": ["name", "version"],
                "properties": {
                    "name": { "type": "string" },
                    "version": { "type": "string" },
                    "model_name": optional_string_schema(),
                    "extra": optional_object_schema()
                }
            },
            "step": {
                "type": "object",
                "additionalProperties": false,
                "required": ["step_id", "source", "message"],
                "properties": {
                    "step_id": { "type": "integer", "minimum": 1 },
                    "timestamp": optional_string_schema(),
                    "source": { "enum": ["system", "user", "agent"] },
                    "message": { "type": "string" },
                    "reasoning_content": optional_string_schema(),
                    "tool_calls": {
                        "anyOf": [
                            {
                                "type": "array",
                                "items": { "$ref": "#/$defs/tool_call" }
                            },
                            { "type": "null" }
                        ]
                    },
                    "observation": {
                        "anyOf": [
                            { "$ref": "#/$defs/observation" },
                            { "type": "null" }
                        ]
                    },
                    "llm_call_count": {
                        "anyOf": [
                            { "type": "integer", "minimum": 0 },
                            { "type": "null" }
                        ]
                    },
                    "extra": optional_object_schema()
                }
            },
            "tool_call": {
                "type": "object",
                "additionalProperties": false,
                "required": ["tool_call_id", "function_name", "arguments"],
                "properties": {
                    "tool_call_id": { "type": "string" },
                    "function_name": { "type": "string" },
                    "arguments": { "type": "object" },
                    "extra": optional_object_schema()
                }
            },
            "observation": {
                "type": "object",
                "additionalProperties": false,
                "required": ["results"],
                "properties": {
                    "results": {
                        "type": "array",
                        "items": { "$ref": "#/$defs/observation_result" }
                    }
                }
            },
            "observation_result": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "source_call_id": optional_string_schema(),
                    "content": optional_string_schema(),
                    "extra": optional_object_schema()
                }
            },
            "final_metrics": {
                "type": "object",
                "additionalProperties": false,
                "required": ["total_steps"],
                "properties": {
                    "total_prompt_tokens": optional_nonnegative_integer_schema(),
                    "total_completion_tokens": optional_nonnegative_integer_schema(),
                    "total_cached_tokens": optional_nonnegative_integer_schema(),
                    "total_steps": { "type": "integer", "minimum": 0 },
                    "extra": optional_object_schema()
                }
            }
        }
    })
}

fn step_for_message(
    message: &TimelineMessageEvent,
    source: AtifStepSource,
    content: &str,
    reasoning: Option<&str>,
    paired_tools: Vec<&TimelineToolCallEvent>,
    context: &ProjectionContext,
) -> AtifStep {
    let (tool_calls, observation) = tool_payload(paired_tools, context);
    AtifStep {
        step_id: 0,
        timestamp: valid_timestamp(Some(&message.header.created_at)),
        source,
        message: redact_str(content, context),
        reasoning_content: redact_option(reasoning, context),
        tool_calls,
        observation,
        llm_call_count: None,
        extra: optional_extra([
            (
                "request_id",
                optional_string_value(message.request_id.as_deref()),
            ),
            ("session_id", string_value(&message.session_id)),
            ("message_sequence", Some(json!(message.sequence))),
            ("gents_role", string_value(message_role_label(message))),
        ]),
    }
}

fn step_for_unpaired_tool(tool: &TimelineToolCallEvent, context: &ProjectionContext) -> AtifStep {
    let (tool_calls, observation) = tool_payload(vec![tool], context);
    AtifStep {
        step_id: 0,
        timestamp: valid_timestamp(tool.started_at.as_deref()),
        source: AtifStepSource::Agent,
        message: String::new(),
        reasoning_content: None,
        tool_calls,
        observation,
        llm_call_count: None,
        extra: optional_extra([
            (
                "request_id",
                optional_string_value(tool.request_id.as_deref()),
            ),
            ("session_id", string_value(&tool.session_id)),
            (
                "message_sequence",
                tool.message_sequence.map(|sequence| json!(sequence)),
            ),
            ("synthetic_tool_step", Some(Value::Bool(true))),
        ]),
    }
}

fn tool_payload(
    tools: Vec<&TimelineToolCallEvent>,
    context: &ProjectionContext,
) -> (Option<Vec<AtifToolCall>>, Option<AtifObservation>) {
    if tools.is_empty() {
        return (None, None);
    }
    let tool_calls = tools
        .iter()
        .map(|tool| AtifToolCall {
            tool_call_id: tool.tool_call_id.clone(),
            function_name: tool.tool_name.clone(),
            arguments: atif_arguments(&tool.args, context),
            extra: optional_extra([
                ("status", string_value(&tool.status)),
                (
                    "lifecycle_state",
                    optional_string_value(tool.lifecycle_state.as_deref()),
                ),
                ("started_at", timestamp_value(tool.started_at.as_deref())),
                (
                    "completed_at",
                    timestamp_value(tool.completed_at.as_deref()),
                ),
                (
                    "selected_service_id",
                    optional_string_value(tool.selected_service_id.as_deref()),
                ),
                (
                    "selected_tool_name",
                    optional_string_value(tool.selected_tool_name.as_deref()),
                ),
                (
                    "child_request_id",
                    optional_string_value(tool.child_request_id.as_deref()),
                ),
            ]),
        })
        .collect::<Vec<_>>();
    let results = tools
        .iter()
        .map(|tool| AtifObservationResult {
            source_call_id: Some(tool.tool_call_id.clone()),
            content: tool
                .result
                .as_deref()
                .map(|result| redact_str(result, context)),
            extra: optional_extra([
                ("status", string_value(&tool.status)),
                (
                    "tool_failure_class",
                    optional_string_value(tool.tool_failure_class.as_deref()),
                ),
                (
                    "denial_reason",
                    redact_option(tool.denial_reason.as_deref(), context).map(Value::String),
                ),
                ("latency_ms", tool.latency_ms.map(|latency| json!(latency))),
                (
                    "await_mode",
                    optional_string_value(tool.await_mode.as_deref()),
                ),
                (
                    "cancel_cause",
                    redact_option(tool.cancel_cause.as_deref(), context).map(Value::String),
                ),
                (
                    "child_request_id",
                    optional_string_value(tool.child_request_id.as_deref()),
                ),
            ]),
        })
        .collect::<Vec<_>>();
    (Some(tool_calls), Some(AtifObservation { results }))
}

fn paired_tools_for_message<'a>(
    timeline: &'a RunTimeline,
    message: &TimelineMessageEvent,
) -> Vec<&'a TimelineToolCallEvent> {
    timeline
        .events
        .iter()
        .filter_map(|event| match event {
            RunTimelineEvent::ToolCall(tool)
                if root_tool(timeline, tool)
                    && tool.session_id == message.session_id
                    && tool.message_sequence == Some(message.sequence) =>
            {
                Some(tool)
            }
            _ => None,
        })
        .collect()
}

fn tool_has_paired_message(timeline: &RunTimeline, tool: &TimelineToolCallEvent) -> bool {
    timeline.events.iter().any(|event| match event {
        RunTimelineEvent::Message(message)
            if root_message(timeline, message)
                && message.session_id == tool.session_id
                && Some(message.sequence) == tool.message_sequence =>
        {
            projected_message(message).is_some_and(|(source, _, _)| source == AtifStepSource::Agent)
        }
        _ => false,
    })
}

fn projected_message(
    message: &TimelineMessageEvent,
) -> Option<(AtifStepSource, String, Option<String>)> {
    let presented = present_message(&message.message);
    let source = match presented.role {
        PresentedMessageRole::Tool => return None,
        _ if matches!(
            message.message,
            gents_protocol::message::Message::System { .. }
        ) =>
        {
            AtifStepSource::System
        }
        PresentedMessageRole::User => AtifStepSource::User,
        PresentedMessageRole::Assistant => AtifStepSource::Agent,
    };
    Some((
        source,
        presented.body_markdown,
        presented.reasoning_markdown,
    ))
}

fn message_role_label(message: &TimelineMessageEvent) -> &'static str {
    match present_message(&message.message).role {
        PresentedMessageRole::User => "user",
        PresentedMessageRole::Assistant => "assistant",
        PresentedMessageRole::Tool => "tool",
    }
}

fn root_message(timeline: &RunTimeline, message: &TimelineMessageEvent) -> bool {
    event_belongs_to_root(
        timeline,
        message.request_id.as_deref(),
        Some(&message.session_id),
    )
}

fn root_tool(timeline: &RunTimeline, tool: &TimelineToolCallEvent) -> bool {
    event_belongs_to_root(timeline, tool.request_id.as_deref(), Some(&tool.session_id))
}

fn event_belongs_to_root(
    timeline: &RunTimeline,
    request_id: Option<&str>,
    session_id: Option<&str>,
) -> bool {
    request_id == Some(timeline.request_id.as_str())
        || (request_id.is_none() && session_id == timeline.session_id.as_deref())
}

fn atif_arguments(raw: &str, context: &ProjectionContext) -> BTreeMap<String, Value> {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(arguments)) => redact_json_value(Value::Object(arguments), context)
            .as_object()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect(),
        Ok(value) => BTreeMap::from([("_value".to_string(), redact_json_value(value, context))]),
        Err(_) => BTreeMap::from([("_raw".to_string(), Value::String(redact_str(raw, context)))]),
    }
}

fn valid_timestamp(timestamp: Option<&str>) -> Option<String> {
    timestamp
        .filter(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).is_ok())
        .map(ToOwned::to_owned)
}

fn timestamp_value(timestamp: Option<&str>) -> Option<Value> {
    valid_timestamp(timestamp).map(Value::String)
}

fn string_value(value: &str) -> Option<Value> {
    Some(Value::String(value.to_string()))
}

fn optional_string_value(value: Option<&str>) -> Option<Value> {
    value.map(|value| Value::String(value.to_string()))
}

fn optional_extra<const N: usize>(
    entries: [(&str, Option<Value>); N],
) -> Option<BTreeMap<String, Value>> {
    let extra = entries
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_string(), value)))
        .collect::<BTreeMap<_, _>>();
    (!extra.is_empty()).then_some(extra)
}

fn optional_string_schema() -> Value {
    json!({
        "anyOf": [
            { "type": "string" },
            { "type": "null" }
        ]
    })
}

fn optional_object_schema() -> Value {
    json!({
        "anyOf": [
            { "type": "object" },
            { "type": "null" }
        ]
    })
}

fn optional_nonnegative_integer_schema() -> Value {
    json!({
        "anyOf": [
            { "type": "integer", "minimum": 0 },
            { "type": "null" }
        ]
    })
}

pub fn root_observed_model(timeline: &RunTimeline) -> Option<String> {
    let root_doc_id = timeline.request_doc_id.as_deref()?;
    let mut observed = timeline.events.iter().filter_map(|event| match event {
        RunTimelineEvent::RenderedRequest(capture)
            if capture.request_doc_id.as_deref() == Some(root_doc_id)
                && capture.request_id.as_deref() == Some(timeline.request_id.as_str()) =>
        {
            Some(capture)
        }
        _ => None,
    });
    let first = observed
        .next()?
        .model_name
        .as_deref()
        .filter(|name| !name.is_empty())?;
    observed
        .all(|capture| capture.model_name.as_deref() == Some(first))
        .then(|| first.to_string())
}

fn root_inference_call(
    timeline: &RunTimeline,
    call: &crate::run_timeline::TimelineInferenceCallRow,
) -> bool {
    match timeline.request_doc_id.as_deref() {
        Some(request_doc_id) => call.request_doc_id.as_deref() == Some(request_doc_id),
        None => call.request_id == timeline.request_id,
    }
}

fn trajectory_usage_totals(timeline: &RunTimeline) -> (Option<u64>, Option<u64>, Option<u64>) {
    let root_calls = timeline
        .inference_calls
        .iter()
        .filter(|call| root_inference_call(timeline, call))
        .filter(|call| matches!(call.call_kind.as_str(), "inference" | "compaction"));
    crate::provider_usage::sum_persisted_usage_columns(root_calls.map(|call| {
        (
            call.prompt_tokens,
            call.completion_tokens,
            call.cached_input_tokens,
        )
    }))
}

fn require_nonempty(violations: &mut Vec<String>, path: &str, value: &str) {
    if value.trim().is_empty() {
        violations.push(format!("{path} is required"));
    }
}
