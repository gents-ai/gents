//! Lens v2→v3: adds subagent extensions to AgentToolCall, AgentRequest,
//! and ToolSelection. Forward transform populates new fields with their
//! defaults; inverse transform drops them for P2P backward-compat.
//!
//! Operates over the same JSON-document iterator API as the v1→v2 lens.

use std::collections::HashMap;
use std::error::Error;

use lens_sdk::StreamOption;
use serde_json::Value;

#[cfg(target_arch = "wasm32")]
lens_sdk::define!(try_transform, try_inverse);

fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    for item in iter {
        let mut doc = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };

        // Detect collection by shape-unique fields. Transforms are commutative
        // (or_insert never overwrites existing values), so a doc matching
        // multiple heuristics is handled safely.

        // AgentToolCall: uniquely identified by tool_call_key.
        if doc.contains_key("tool_call_key") {
            doc.entry("await_mode".to_string())
                .or_insert(Value::String("foreground".to_string()));
            doc.entry("cancel_policy".to_string())
                .or_insert(Value::String("cascade".to_string()));
            doc.entry("child_request_id".to_string())
                .or_insert(Value::Null);
            doc.entry("request_id".to_string()).or_insert(Value::Null);
        }

        // AgentRequest: has request_id AND agent_did (distinguishes from
        // AgentToolCall which also gains request_id in v3).
        if doc.contains_key("request_id") && doc.contains_key("agent_did") {
            doc.entry("subagent_depth".to_string())
                .or_insert(Value::Number(0.into()));
            doc.entry("caused_by_parent_request_id".to_string())
                .or_insert(Value::Null);
            doc.entry("caused_by_parent_tool_call_id".to_string())
                .or_insert(Value::Null);
        }

        // ToolSelection: uniquely identified by selection_id.
        if doc.contains_key("selection_id") {
            doc.entry("subagent_targets".to_string())
                .or_insert(Value::Array(Vec::new()));
            doc.entry("subagent_spawn_enabled".to_string())
                .or_insert(Value::Bool(false));
            doc.entry("subagent_steering_enabled".to_string())
                .or_insert(Value::Bool(false));
            doc.entry("subagent_background_enabled".to_string())
                .or_insert(Value::Bool(false));
        }

        return Ok(StreamOption::Some(doc));
    }
    Ok(StreamOption::EndOfStream)
}

fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    for item in iter {
        let mut doc = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };

        // Drop all v3-only fields for P2P backward-compat with v2 nodes.
        for field in &[
            // AgentToolCall
            "await_mode",
            "cancel_policy",
            "child_request_id",
            "request_id",
            // AgentRequest
            "subagent_depth",
            "caused_by_parent_request_id",
            "caused_by_parent_tool_call_id",
            // ToolSelection
            "subagent_targets",
            "subagent_spawn_enabled",
            "subagent_steering_enabled",
            "subagent_background_enabled",
        ] {
            doc.remove(*field);
        }

        return Ok(StreamOption::Some(doc));
    }
    Ok(StreamOption::EndOfStream)
}
