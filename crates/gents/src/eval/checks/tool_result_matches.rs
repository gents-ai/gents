use super::{
    captured_fields_match::CapturedFieldsMatch, grader, Check, CheckDescription, CheckVerdict,
};
use crate::eval::runner::{CaptureResult, StageEvidence};
use serde::Deserialize;
use serde_json::{json, Value};

pub struct ToolResultMatches;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    expect: Vec<Value>,
}

impl Check for ToolResultMatches {
    fn name(&self) -> &'static str {
        "tool_result_matches"
    }
    fn version(&self) -> &'static str {
        "1"
    }
    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let params: Params = match serde_json::from_value(params.clone()) {
            Ok(params) => params,
            Err(e) => return grader("bad_params", e.to_string()),
        };
        let params = json!({"name":"tool_result","expect":params.expect});
        let mut evidence = stage.clone();
        evidence.captures.clear();
        evidence.captures.insert(
            "tool_result".into(),
            CaptureResult::Documents { rows: vec![] },
        );
        let mut best = CapturedFieldsMatch.evaluate(&params, &evidence);
        if best.score_bp.is_none() {
            return best;
        }
        for call in &stage.tool_calls {
            if call.status.as_deref().or(call.lifecycle_state.as_deref()) != Some("completed")
                || call.tool_failure_class.is_some()
            {
                continue;
            }
            let result = match &call.result {
                Value::String(text) => serde_json::from_str(text).unwrap_or(Value::Null),
                value => value.clone(),
            };
            evidence.captures.insert(
                "tool_result".into(),
                CaptureResult::Documents { rows: vec![result] },
            );
            let mut verdict = CapturedFieldsMatch.evaluate(&params, &evidence);
            if verdict.score_bp > best.score_bp {
                verdict.raw["tool_name"] = json!(call.tool_name);
                best = verdict;
            }
        }
        best
    }
    fn describe(&self) -> CheckDescription {
        let mut description = CapturedFieldsMatch.describe();
        let expect = description.params_schema["properties"]["expect"].clone();
        description.name = self.name().into();
        description.version = self.version().into();
        description.summary="Requires one successful tool result to satisfy all field expectations, regardless of the tool's name. Expectations cannot be assembled across different calls; failed calls and final-answer prose are not evidence.".into();
        description.params_schema = json!({"type":"object","required":["expect"],"additionalProperties":false,"properties":{"expect":expect}});
        description.reads = vec!["stage:tool_calls".into()];
        description
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::{embedded::observe::ToolCallEvidence, ScriptedExecutor};
    #[test]
    fn stock_records_accepts_requested_projection_without_requiring_filter_key_echo() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/configurator_evals/schema_experience/cases/stock_records.json"
        )))
        .unwrap();
        let use_stage = fixture["stages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stage| stage["stage_id"] == "use")
            .unwrap();
        let params = &use_stage["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|check| check["check"] == "tool_result_matches")
            .unwrap()["params"];
        let mut stage = ScriptedExecutor::passed_evidence("did:test", "use", "unused", vec![])
            .stages
            .remove(0);
        stage.tool_calls = vec![ToolCallEvidence {
            tool_name: "find_part_by_sku".into(),
            status: Some("completed".into()),
            lifecycle_state: Some("completed".into()),
            tool_failure_class: None,
            started_at: None,
            completed_at: None,
            args: json!({"fields":["label","quantity"],"sku":"SEAL-2"}),
            result: json!({"collection":"Part","count":1,"results":[{"label":"pump seals","quantity":3}]}),
        }];
        assert_eq!(
            ToolResultMatches.evaluate(params, &stage).score_bp,
            Some(10000)
        );
        for incomplete in [
            json!({"collection":"Part","count":1,"results":[{"sku":"SEAL-2"}]}),
            json!({"collection":"Part","count":0,"results":[]}),
            json!({"collection":"Other","count":1,"results":[{"label":"pump seals","quantity":3}]}),
        ] {
            stage.tool_calls[0].result = incomplete;
            assert_ne!(
                ToolResultMatches.evaluate(params, &stage).score_bp,
                Some(10000)
            );
        }
        stage.tool_calls.clear();
        assert_eq!(ToolResultMatches.evaluate(params, &stage).score_bp, Some(0));
    }

    #[test]
    fn success_uses_one_real_tool_result_and_rejects_errors_or_cross_call_matches() {
        let mut stage = ScriptedExecutor::passed_evidence("did:test", "read", "unused", vec![])
            .stages
            .remove(0);
        let params = json!({"expect":[{"field":"collection","equals":"Part"},{"field":"results","contains":"SEAL-2"}]});
        let call = |name: &str, result: Value| ToolCallEvidence {
            tool_name: name.into(),
            status: Some("completed".into()),
            lifecycle_state: None,
            tool_failure_class: None,
            started_at: None,
            completed_at: None,
            args: json!({}),
            result,
        };
        for name in ["query", "an_arbitrary_declared_tool"] {
            stage.tool_calls = vec![call(
                name,
                json!({"collection":"Part","results":[{"sku":"SEAL-2"}]}),
            )];
            assert_eq!(
                ToolResultMatches.evaluate(&params, &stage).score_bp,
                Some(10000)
            );
            stage.tool_calls[0].result = Value::String(stage.tool_calls[0].result.to_string());
            assert_eq!(
                ToolResultMatches.evaluate(&params, &stage).score_bp,
                Some(10000)
            );
            stage.tool_calls[0].tool_failure_class = Some("executionFailed".into());
            assert_eq!(
                ToolResultMatches.evaluate(&params, &stage).score_bp,
                Some(0)
            );
        }
        stage.tool_calls = vec![
            call("first", json!({"collection":"Part"})),
            call(
                "second",
                json!({"collection":"Wrong","results":[{"sku":"SEAL-2"}]}),
            ),
        ];
        assert_eq!(
            ToolResultMatches.evaluate(&params, &stage).score_bp,
            Some(5000)
        );
        assert!(ToolResultMatches
            .evaluate(&json!({"expect":[{"field":"x","matches":"["}]}), &stage)
            .score_bp
            .is_none());
    }
}
