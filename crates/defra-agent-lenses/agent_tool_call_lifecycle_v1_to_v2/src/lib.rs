//! WASM Lens migration: AgentToolCall v1 -> v2.

use std::collections::HashMap;
use std::error::Error;

use lens_sdk::StreamOption;
use serde_json::Value;

lens_sdk::define!(try_transform, try_inverse);

/// Compute the v2 (lifecycle_state, tool_failure_class) pair from the v1
/// (status, tool_failure_class) pair. Public for unit tests.
pub fn compute_v2_fields(
    status: Option<&str>,
    legacy_failure_class: Option<&str>,
) -> (String, Option<String>) {
    match (status, legacy_failure_class) {
        // In-flight calls become Running. Failure class preserved if non-null
        // (will be rebucketed by the time it reaches a terminal state).
        (Some("called"), legacy) => ("running".to_string(), legacy.map(rebucket_failure_class)),
        // Successful completion: no failure class.
        (Some("completed"), None) => ("completed".to_string(), None),
        // Timeout completion: state becomes timedOut, failure class cleared.
        (Some("completed"), Some("tool_timeout")) => ("timedOut".to_string(), None),
        // Other completion-with-failure: state becomes failed, failure class
        // rebucketed to the Lean 5-variant vocabulary.
        (Some("completed"), Some(legacy)) => ("failed".to_string(), Some(rebucket_failure_class(legacy))),
        // Unrecognized status: preserve, do not migrate.
        (Some(s), legacy) => (s.to_string(), legacy.map(rebucket_failure_class)),
        (None, _) => ("running".to_string(), None),
    }
}

/// Map a legacy 12-variant ToolFailureClass string to the Lean 5-variant
/// FailureClass string. Per R1 spec section "ToolFailureClass collapse".
pub fn rebucket_failure_class(legacy: &str) -> String {
    match legacy {
        // Identity.
        "service_unavailable" => "serviceUnavailable".to_string(),
        // Service-side discovery failures collapse to ServiceUnavailable.
        "tool_not_found" | "resource_not_found" | "service_schema_drift" => {
            "serviceUnavailable".to_string()
        }
        // Argument validation failures collapse to ArgumentInvalid.
        "invalid_tool_arguments" | "invalid_json_arguments" | "arguments_not_object" => {
            "argumentInvalid".to_string()
        }
        // Tool execution errors collapse to ToolReturnedError.
        "tool_runtime_error" | "nonzero_command_exit" | "unclassified" => {
            "toolReturnedError".to_string()
        }
        // Already-Lean-vocabulary values pass through (defensive: lens runs
        // idempotently on partially-migrated data).
        "argumentInvalid" | "serviceUnavailable" | "transport" | "toolReturnedError"
        | "external" => legacy.to_string(),
        // Unknown: classify as External (non-tool-layer concern).
        _ => "external".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn called_becomes_running() {
        let (state, fc) = compute_v2_fields(Some("called"), None);
        assert_eq!(state, "running");
        assert_eq!(fc, None);
    }

    #[test]
    fn completed_no_failure_class_stays_completed() {
        let (state, fc) = compute_v2_fields(Some("completed"), None);
        assert_eq!(state, "completed");
        assert_eq!(fc, None);
    }

    #[test]
    fn completed_with_tool_timeout_becomes_timedOut() {
        let (state, fc) = compute_v2_fields(Some("completed"), Some("tool_timeout"));
        assert_eq!(state, "timedOut");
        assert_eq!(fc, None);
    }

    #[test]
    fn completed_with_invalid_arguments_becomes_failed_argumentInvalid() {
        let (state, fc) = compute_v2_fields(Some("completed"), Some("invalid_tool_arguments"));
        assert_eq!(state, "failed");
        assert_eq!(fc, Some("argumentInvalid".to_string()));
    }

    #[test]
    fn completed_with_nonzero_exit_becomes_failed_toolReturnedError() {
        let (state, fc) = compute_v2_fields(Some("completed"), Some("nonzero_command_exit"));
        assert_eq!(state, "failed");
        assert_eq!(fc, Some("toolReturnedError".to_string()));
    }

    #[test]
    fn unknown_failure_class_becomes_external() {
        assert_eq!(rebucket_failure_class("some_future_variant"), "external");
    }

    #[test]
    fn already_migrated_failure_class_passes_through() {
        assert_eq!(rebucket_failure_class("argumentInvalid"), "argumentInvalid");
    }
}

fn try_transform(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    for item in iter {
        let mut input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };

        let status = input.get("status").and_then(|v| v.as_str()).map(str::to_string);
        let legacy_fc = input
            .get("tool_failure_class")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        let (lifecycle_state, new_fc) =
            compute_v2_fields(status.as_deref(), legacy_fc.as_deref());

        input.insert(
            "lifecycle_state".to_string(),
            Value::String(lifecycle_state),
        );
        input.insert(
            "tool_failure_class".to_string(),
            new_fc.map(Value::String).unwrap_or(Value::Null),
        );

        return Ok(StreamOption::Some(input));
    }
    Ok(StreamOption::EndOfStream)
}

fn try_inverse(
    iter: &mut dyn Iterator<Item = lens_sdk::Result<Option<HashMap<String, Value>>>>,
) -> Result<StreamOption<HashMap<String, Value>>, Box<dyn Error>> {
    for item in iter {
        let mut input = match item? {
            Some(v) => v,
            None => return Ok(StreamOption::None),
        };
        // v2->v1 inverse: drop the lifecycle_state field. tool_failure_class
        // stays in v1 vocabulary form because we cannot losslessly recover the
        // 12-variant legacy vocabulary from the 5-variant Lean vocabulary; the
        // inverse intentionally leaves the rebucketed value in place.
        input.remove("lifecycle_state");
        return Ok(StreamOption::Some(input));
    }
    Ok(StreamOption::EndOfStream)
}
