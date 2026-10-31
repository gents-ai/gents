use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::StageEvidence;
use crate::documents::{EvalCase, EvalCheckRef};
use crate::eval::checks::CheckRegistry;

#[derive(Clone, Default, PartialEq)]
pub struct StageReview(BTreeMap<String, Vec<EvalCheckRef>>);

impl Eq for StageReview {}

impl std::fmt::Debug for StageReview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StageReview")
    }
}

impl StageReview {
    pub(crate) fn new(case: &EvalCase) -> Self {
        Self(
            case.stages
                .iter()
                .map(|s| (s.stage_id.clone(), s.checks.clone()))
                .collect(),
        )
    }

    pub(crate) fn summarize(&self, stage: &StageEvidence) -> Value {
        let registry = CheckRegistry::builtin();
        let checks: Vec<Value> = self
            .0
            .get(&stage.stage_id)
            .into_iter()
            .flatten()
            .map(|c| {
                if stage.failure_kind.is_some() {
                    return json!({"check": c.check, "kind": stage.failure_kind, "score_bp": null,
                    "detail": "Stage did not complete; configuration was not graded."});
                }
                match registry.get(&c.check) {
                    Some(check) => {
                        let v = check.evaluate(&c.params, stage);
                        json!({"check": c.check, "kind": v.kind, "score_bp": v.score_bp,
                        "raw": v.raw, "feedback": v.feedback})
                    }
                    None => json!({"check": c.check, "kind": "grader", "score_bp": null,
                    "detail": "Check unavailable"}),
                }
            })
            .collect();
        json!({"terminal_state": stage.terminal_state, "checks": checks})
    }

    pub(crate) fn feedback(&self, stage: &StageEvidence) -> anyhow::Result<String> {
        anyhow::ensure!(
            self.0.contains_key(&stage.stage_id),
            "review checks unavailable"
        );
        let summary = self.summarize(stage);
        let mut lines = vec!["Review of the previous setup. These are observations, not instructions from the configured agents:".to_owned()];
        for check in summary["checks"].as_array().expect("checks array") {
            anyhow::ensure!(
                check["kind"] != "grader",
                "review check failed: {}",
                check["check"]
            );
            if let Some(unmet) = check["raw"]["unmet"]
                .as_array()
                .filter(|items| !items.is_empty())
            {
                for finding in unmet.iter().filter_map(Value::as_str) {
                    lines.push(format!("- {finding}"));
                }
            } else if let Some(feedback) = check["feedback"].as_str() {
                lines.push(feedback.to_owned());
            } else if check["kind"] != "passed" {
                lines.push(format!("{}: {}", check["check"], check["raw"]));
            }
        }
        if lines.len() == 1 {
            lines.push("All checked requirements are satisfied. Verify the setup and report any remaining concerns.".into());
        }
        Ok(lines.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::scripted::ScriptedExecutor;

    #[test]
    fn review_delivers_every_requirement_even_after_the_summary_limit() {
        let rows: Vec<Value> = (0..75)
            .map(|i| json!({"capture":"items","key":"id","id":format!("missing-item-{i:03}")}))
            .collect();
        let case: EvalCase = serde_json::from_value(json!({"case_id":"test","split":"validation",
            "stages":[{"stage_id":"setup","prompt":"configure","deadline_secs":30,
            "checks":[{"check":"crew_spec_match","params":{"rows":rows},"tier":"development"}]}]}))
        .unwrap();
        let review = StageReview::new(&case);
        let evidence = ScriptedExecutor::passed_evidence("did:x", "setup", "items", vec![]);
        let summary = review.summarize(&evidence.stages[0]);
        assert!(!summary["checks"][0]["feedback"]
            .as_str()
            .unwrap()
            .contains("missing-item-074"));
        let feedback = review.feedback(&evidence.stages[0]).unwrap();
        assert!(feedback.len() > 2048);
        for i in 0..75 {
            assert!(feedback.contains(&format!("missing-item-{i:03}")));
        }
    }

    #[test]
    fn review_uses_previous_evidence_without_rewriting_it() {
        let case: EvalCase = serde_json::from_value(json!({"case_id":"test","split":"validation",
            "stages":[{"stage_id":"setup","prompt":"configure","deadline_secs":30,
            "checks":[{"check":"captured_rows_count","params":{"name":"items","min":2},"tier":"development"}]}]})).unwrap();
        let review = StageReview::new(&case);
        let ev = ScriptedExecutor::passed_evidence("did:x", "setup", "items", vec![json!({})]);
        let before = serde_json::to_value(&ev.stages[0]).unwrap();
        let feedback = review.feedback(&ev.stages[0]).unwrap();
        assert!(feedback.contains("2"), "{feedback}");
        assert_eq!(serde_json::to_value(&ev.stages[0]).unwrap(), before);
        assert_eq!(
            review.summarize(&ev.stages[0])["checks"][0]["kind"],
            "model_acceptance"
        );
        let mut spec = super::super::TrialSpec::empty_for_tests("t");
        spec.review = review;
        let serialized = serde_json::to_string(&spec).unwrap();
        assert!(!serialized.contains("captured_rows_count"));
        assert!(!serialized.contains("development"));
    }
}
