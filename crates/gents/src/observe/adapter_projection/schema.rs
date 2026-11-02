use serde_json::{json, Value};

use super::{
    atif, AdapterProjectionKind, ADAPTER_PROJECTION_VERSION,
    EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID, RUN_TIMELINE_PROJECTION_ID,
};

mod native;

use native::{
    langgraph_projection_schema, multi_agent_projection_schema, openai_codex_projection_schema,
};

pub fn adapter_projection_native_json_schema(kind: AdapterProjectionKind) -> Value {
    let mut schema = projection_json_schema(kind);
    if let Some(object) = schema.as_object_mut() {
        object.insert(
            "$schema".to_string(),
            Value::String("https://json-schema.org/draft/2020-12/schema".to_string()),
        );
        object.insert(
            "$id".to_string(),
            Value::String(format!(
                "https://schemas.defra.ai/gents/adapter-projection/{}/{}-native.schema.json",
                kind.id(),
                ADAPTER_PROJECTION_VERSION
            )),
        );
        object.insert(
            "title".to_string(),
            Value::String(format!("{} Native Projection", kind.title())),
        );
    }
    schema
}

pub fn adapter_projection_json_schema(kind: AdapterProjectionKind) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://schemas.defra.ai/gents/adapter-projection/{}/{}.schema.json", kind.id(), ADAPTER_PROJECTION_VERSION),
        "title": kind.title(),
        "type": "object",
        "additionalProperties": false,
        "required": [
            "projection_id",
            "projection_version",
            "source_request_id",
            "redaction_mode",
            "provenance",
            "output"
        ],
        "properties": {
            "projection_id": { "const": kind.id() },
            "projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "source_request_id": string_schema(),
            "source_request_doc_id": optional_string_schema(),
            "source_session_id": optional_string_schema(),
            "source_node_did": optional_string_schema(),
            "source_agent_id": optional_string_schema(),
            "redaction_mode": redaction_mode_schema(),
            "provenance": provenance_schema(),
            "rendered_captures": rendered_captures_schema(),
            "output": {
                "type": "object",
                "additionalProperties": false,
                "required": ["adapter", "projection"],
                "properties": {
                    "adapter": { "const": kind.id() },
                    "projection": envelope_projection_json_schema(kind)
                }
            }
        }
    })
}

/// Capture metadata only: the shape has no field a captured body could travel in.
fn rendered_captures_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["capture_key", "provenance_status"],
            "properties": {
                "capture_key": string_schema(),
                "request_doc_id": optional_string_schema(),
                "request_id": optional_string_schema(),
                "session_id": optional_string_schema(),
                "capture_scope": optional_string_schema(),
                "scope_kind": optional_string_schema(),
                "scope_seq": { "type": "integer" },
                "turn_index": { "type": "integer" },
                "attempt": { "type": "integer" },
                "capture_version": { "type": "integer" },
                "model_name": optional_string_schema(),
                "source": optional_string_schema(),
                "prompt_hash": optional_string_schema(),
                "tools_hash": optional_string_schema(),
                "provenance_status": string_schema(),
                "manifest_version": { "type": "integer" },
                "call_id": optional_string_schema(),
                "call_seq": { "type": "integer" },
                "created_at": optional_string_schema()
            }
        }
    })
}

pub fn adapter_projection_jsonl_record_schema(kind: AdapterProjectionKind) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://schemas.defra.ai/gents/adapter-projection/{}/{}-jsonl-record.schema.json", kind.id(), ADAPTER_PROJECTION_VERSION),
        "title": format!("{} JSONL Record", kind.title()),
        "type": "object",
        "additionalProperties": false,
        "required": [
            "projection_id",
            "projection_version",
            "source_request_id",
            "redaction_mode",
            "record_kind",
            "record_index",
            "record_id",
            "value"
        ],
        "properties": {
            "projection_id": { "const": kind.id() },
            "projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "source_request_id": string_schema(),
            "source_session_id": optional_string_schema(),
            "redaction_mode": redaction_mode_schema(),
            "record_kind": jsonl_record_kind_schema(kind),
            "record_index": { "type": "integer", "minimum": 0 },
            "record_id": string_schema(),
            "value": { "type": "object" }
        }
    })
}

pub fn adapter_projection_eval_jsonl_record_schema(kind: AdapterProjectionKind) -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": format!("https://schemas.defra.ai/gents/adapter-projection/{}/{}-eval-jsonl-record.schema.json", kind.id(), ADAPTER_PROJECTION_VERSION),
        "title": format!("{} Eval JSONL Record", kind.title()),
        "type": "object",
        "additionalProperties": false,
        "required": [
            "projection_id",
            "projection_version",
            "source_request_id",
            "redaction_mode",
            "record_index",
            "record_id",
            "sample_kind",
            "adapter_record_kind",
            "adapter_record_id"
        ],
        "properties": {
            "projection_id": { "const": kind.id() },
            "projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "source_request_id": string_schema(),
            "source_session_id": optional_string_schema(),
            "redaction_mode": redaction_mode_schema(),
            "record_index": { "type": "integer", "minimum": 0 },
            "record_id": string_schema(),
            "sample_kind": training_sample_kind_schema(kind),
            "adapter_record_kind": jsonl_record_kind_schema(kind),
            "adapter_record_id": string_schema(),
            "role": optional_string_schema(),
            "input": optional_string_schema(),
            "output": optional_string_schema(),
            "tool_name": optional_string_schema(),
            "status": optional_string_schema(),
            "parent_request_id": optional_string_schema(),
            "child_request_id": optional_string_schema(),
            "metadata": { "type": "object" }
        }
    })
}

pub fn adapter_projection_schema_index() -> Value {
    let projections = [
        AdapterProjectionKind::AtifTrajectory,
        AdapterProjectionKind::OpenAiCodexRunTrace,
        AdapterProjectionKind::LangGraphStateHistory,
        AdapterProjectionKind::MultiAgentTask,
    ];
    json!({
        "projection_version": ADAPTER_PROJECTION_VERSION,
        "source_projection_id": RUN_TIMELINE_PROJECTION_ID,
        "schemas": projections
            .iter()
            .map(|kind| {
                json!({
                    "projection_id": kind.id(),
                    "title": kind.title(),
                    "native_json_schema_id": adapter_projection_native_json_schema(*kind).get("$id").cloned().unwrap_or(Value::Null),
                    "json_schema_id": adapter_projection_json_schema(*kind).get("$id").cloned().unwrap_or(Value::Null),
                    "jsonl_record_schema_id": adapter_projection_jsonl_record_schema(*kind).get("$id").cloned().unwrap_or(Value::Null),
                    "eval_jsonl_record_schema_id": adapter_projection_eval_jsonl_record_schema(*kind).get("$id").cloned().unwrap_or(Value::Null)
                })
            })
            .collect::<Vec<_>>()
    })
}

fn projection_json_schema(kind: AdapterProjectionKind) -> Value {
    match kind {
        AdapterProjectionKind::AtifTrajectory => atif::atif_projection_schema(),
        AdapterProjectionKind::OpenAiCodexRunTrace => openai_codex_projection_schema(),
        AdapterProjectionKind::LangGraphStateHistory => langgraph_projection_schema(),
        AdapterProjectionKind::MultiAgentTask => multi_agent_projection_schema(),
    }
}

fn envelope_projection_json_schema(kind: AdapterProjectionKind) -> Value {
    let mut schema = projection_json_schema(kind);
    if kind == AdapterProjectionKind::AtifTrajectory {
        rewrite_local_definition_refs(
            &mut schema,
            "#/properties/output/properties/projection/$defs/",
        );
    }
    schema
}

fn rewrite_local_definition_refs(value: &mut Value, prefix: &str) {
    match value {
        Value::Object(object) => {
            let definition = object
                .get("$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/$defs/"))
                .map(ToOwned::to_owned);
            if let Some(definition) = definition {
                object.insert(
                    "$ref".to_string(),
                    Value::String(format!("{prefix}{definition}")),
                );
            }
            for child in object.values_mut() {
                rewrite_local_definition_refs(child, prefix);
            }
        }
        Value::Array(values) => {
            for child in values {
                rewrite_local_definition_refs(child, prefix);
            }
        }
        _ => {}
    }
}

fn provenance_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["runtime", "source_projection_id", "source_projection_version", "source_version_status"],
        "properties": {
            "runtime": { "const": "gents" },
            "source_projection_id": {
                "enum": [RUN_TIMELINE_PROJECTION_ID, EXTERNAL_ADAPTER_CAPTURE_PROJECTION_ID]
            },
            "source_projection_version": { "const": ADAPTER_PROJECTION_VERSION },
            "source_version_status": {
                "enum": ["current_state_captured_only", "external_adapter_capture"]
            },
            "actor_did": optional_string_schema(),
            "rendered_request_refs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["capture_doc_id", "request_doc_id", "request_commit_cid"],
                    "properties": {
                        "capture_doc_id": string_schema(),
                        "request_doc_id": string_schema(),
                        "request_commit_cid": string_schema()
                    }
                }
            }
        }
    })
}

fn jsonl_record_kind_schema(kind: AdapterProjectionKind) -> Value {
    match kind {
        AdapterProjectionKind::AtifTrajectory => {
            json!({ "enum": ["atif_agent", "atif_step", "atif_final_metrics"] })
        }
        AdapterProjectionKind::OpenAiCodexRunTrace => {
            json!({ "enum": ["openai_codex_trace_item"] })
        }
        AdapterProjectionKind::LangGraphStateHistory => {
            json!({ "enum": ["langgraph_values", "langgraph_node", "langgraph_edge", "langgraph_task"] })
        }
        AdapterProjectionKind::MultiAgentTask => {
            json!({ "enum": ["multi_agent_participant", "multi_agent_message", "multi_agent_delegation", "multi_agent_tool_event"] })
        }
    }
}

fn training_sample_kind_schema(kind: AdapterProjectionKind) -> Value {
    match kind {
        AdapterProjectionKind::AtifTrajectory => {
            json!({ "enum": ["prompt", "message", "tool_call"] })
        }
        AdapterProjectionKind::OpenAiCodexRunTrace => {
            json!({ "enum": ["prompt", "message", "tool_call"] })
        }
        AdapterProjectionKind::LangGraphStateHistory => {
            json!({ "enum": ["state_snapshot", "state_node", "state_transition", "task"] })
        }
        AdapterProjectionKind::MultiAgentTask => {
            json!({ "enum": ["participant", "message", "delegation", "tool_call"] })
        }
    }
}

fn redaction_mode_schema() -> Value {
    json!({ "enum": ["full", "training_safe", "public"] })
}

fn string_schema() -> Value {
    json!({ "type": "string" })
}

fn optional_string_schema() -> Value {
    json!({
        "anyOf": [
            { "type": "string" },
            { "type": "null" }
        ]
    })
}
