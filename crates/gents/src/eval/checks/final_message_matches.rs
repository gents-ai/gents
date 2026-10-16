use regex::Regex;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::eval::checks::{
    excerpt, graded, graded_reason_codes, grader, Check, CheckDescription, CheckVerdict,
    EXCERPT_CHARS,
};
use crate::eval::runner::executor::StageEvidence;

/// Feedback names the missing patterns and is written only on the train split, so the held-out split catches a candidate that hard-codes them.
pub struct FinalMessageMatches;

const MESSAGE_CHARS: usize = 400;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    #[serde(default)]
    any: Vec<PatternParam>,
    #[serde(default)]
    all: Vec<PatternParam>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PatternParam {
    Substring(String),
    Regex { matches: String },
}

enum Pattern {
    Substring(String),
    Regex(Regex),
}

impl Pattern {
    fn parse(param: PatternParam) -> Result<Self, String> {
        match param {
            PatternParam::Substring(text) => Ok(Self::Substring(text)),
            PatternParam::Regex { matches } => Regex::new(&matches)
                .map(Self::Regex)
                .map_err(|error| error.to_string()),
        }
    }

    fn found_in(&self, text: &str) -> bool {
        match self {
            Self::Substring(needle) => text.contains(needle.as_str()),
            Self::Regex(pattern) => pattern.is_match(text),
        }
    }

    fn describe(&self) -> String {
        excerpt(
            &match self {
                Self::Substring(needle) => format!("{needle:?}"),
                Self::Regex(pattern) => format!("/{pattern}/"),
            },
            EXCERPT_CHARS,
        )
    }
}

fn list<'a>(patterns: impl Iterator<Item = &'a Pattern>) -> String {
    patterns
        .map(Pattern::describe)
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_all(params: Vec<PatternParam>) -> Result<Vec<Pattern>, String> {
    params.into_iter().map(Pattern::parse).collect()
}

impl Check for FinalMessageMatches {
    fn name(&self) -> &'static str {
        "final_message_matches"
    }

    fn version(&self) -> &'static str {
        "1"
    }

    fn describe(&self) -> CheckDescription {
        let pattern = json!({
            "oneOf": [
                {"type": "string", "description": "a substring"},
                {
                    "type": "object",
                    "properties": {"matches": {"type": "string", "description": "a regex"}},
                    "required": ["matches"],
                    "additionalProperties": false
                }
            ]
        });
        CheckDescription {
            name: self.name().into(),
            version: self.version().into(),
            summary: "Scores the fraction of `all` patterns found in the last assistant message, and 0 when `any` is given and none of it is found.".into(),
            params_schema: json!({
                "type": "object",
                "properties": {
                    "any": {"type": "array", "items": pattern},
                    "all": {"type": "array", "items": pattern}
                },
                "anyOf": [
                    {"required": ["any"], "properties": {"any": {"minItems": 1}}},
                    {"required": ["all"], "properties": {"all": {"minItems": 1}}}
                ],
                "additionalProperties": false
            }),
            reads: vec!["stage:messages".into()],
            reason_codes: graded_reason_codes(&[]),
        }
    }

    fn evaluate(&self, params: &Value, stage: &StageEvidence) -> CheckVerdict {
        let params: Params = match serde_json::from_value(params.clone()) {
            Ok(params) => params,
            Err(error) => return grader("bad_params", error.to_string()),
        };
        let (any, all) = match (parse_all(params.any), parse_all(params.all)) {
            (Ok(any), Ok(all)) => (any, all),
            (Err(detail), _) | (_, Err(detail)) => return grader("bad_params", detail),
        };
        if any.is_empty() && all.is_empty() {
            return grader("bad_params", "neither any nor all names a pattern");
        }
        let total = all.len().max(1);
        let Some(message) = stage
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "assistant" && !message.content.trim().is_empty())
        else {
            return graded(0, total, Some("no final assistant message".into()));
        };
        let text = message.content.as_str();
        let mut lines = Vec::new();
        let any_found = any.is_empty() || any.iter().any(|pattern| pattern.found_in(text));
        if !any_found {
            lines.push(format!("none of: {}", list(any.iter())));
        }
        let missing: Vec<_> = all
            .iter()
            .filter(|pattern| !pattern.found_in(text))
            .collect();
        if !missing.is_empty() {
            lines.push(format!("missing: {}", list(missing.iter().copied())));
        }
        if lines.is_empty() {
            return graded(total, total, None);
        }
        lines.push(format!(
            "final message ({} chars): {}",
            text.chars().count(),
            excerpt(text, MESSAGE_CHARS)
        ));
        let satisfied = if any_found { total - missing.len() } else { 0 };
        graded(satisfied, total, Some(lines.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::runner::embedded::observe::MessageEvidence;
    use crate::eval::runner::scripted::ScriptedExecutor;
    use crate::eval::OutcomeKind;

    fn stage(messages: &[&str]) -> StageEvidence {
        let mut evidence = ScriptedExecutor::passed_evidence("did:x", "s1", "items", vec![]);
        let mut stage = evidence.stages.remove(0);
        stage.messages = messages
            .iter()
            .map(|content| MessageEvidence {
                role: "assistant".into(),
                content: (*content).into(),
                created_at: None,
            })
            .collect();
        stage
    }

    fn feedback(verdict: &CheckVerdict) -> &str {
        verdict.feedback.as_deref().unwrap_or_default()
    }

    #[test]
    fn a_final_message_holding_every_pattern_passes_with_no_feedback() {
        let verdict = FinalMessageMatches.evaluate(
            &json!({"any": ["disk", "volume"], "all": ["3 findings", {"matches": "sev(erity)?: high"}]}),
            &stage(&["working", "Wrote 3 findings on disk; severity: high"]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::Passed, Some(10_000))
        );
        assert_eq!(verdict.feedback, None);
    }

    #[test]
    fn the_score_is_the_fraction_of_all_found_and_names_what_is_missing() {
        let verdict = FinalMessageMatches.evaluate(
            &json!({"all": ["alpha", "beta", {"matches": "gam+a"}, "delta"]}),
            &stage(&["alpha and gamma"]),
        );
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::ModelAcceptance, Some(5_000))
        );
        let text = feedback(&verdict);
        assert!(text.contains(r#"missing: "beta", "delta""#), "{text}");
        assert!(!text.contains(r#""alpha""#), "only what is missing: {text}");
        assert!(
            text.contains("alpha and gamma"),
            "the message is quoted: {text}"
        );
    }

    #[test]
    fn none_of_any_found_scores_zero_whatever_all_holds() {
        let verdict = FinalMessageMatches.evaluate(
            &json!({"any": ["disk", {"matches": "vol(ume)?"}], "all": ["done"]}),
            &stage(&["done"]),
        );
        assert_eq!(verdict.score_bp, Some(0));
        let text = feedback(&verdict);
        assert!(text.contains(r#"none of: "disk", /vol(ume)?/"#), "{text}");
    }

    #[test]
    fn the_last_non_empty_assistant_message_is_the_one_read() {
        let verdict =
            FinalMessageMatches.evaluate(&json!({"all": ["answer"]}), &stage(&["answer", "", " "]));
        assert_eq!(verdict.score_bp, Some(10_000));
        let verdict = FinalMessageMatches.evaluate(
            &json!({"all": ["answer"]}),
            &stage(&["the answer", "later"]),
        );
        assert_eq!(verdict.score_bp, Some(0));
    }

    #[test]
    fn a_stage_with_no_assistant_message_fails_and_says_so() {
        let verdict = FinalMessageMatches.evaluate(&json!({"all": ["answer"]}), &stage(&[]));
        assert_eq!(
            (verdict.kind, verdict.score_bp),
            (OutcomeKind::ModelAcceptance, Some(0))
        );
        assert!(feedback(&verdict).contains("no final assistant message"));
    }

    #[test]
    fn a_long_message_is_quoted_as_a_bounded_excerpt() {
        let long = "x".repeat(10_000);
        let verdict = FinalMessageMatches.evaluate(&json!({"all": ["answer"]}), &stage(&[&long]));
        let text = feedback(&verdict);
        assert!(text.len() < 1_000, "{}", text.len());
        assert!(text.contains("10000 chars"), "{text}");
    }

    #[test]
    fn params_that_do_not_parse_or_require_nothing_are_bad_params() {
        for params in [
            json!({}),
            json!({"all": [1]}),
            json!({"all": [{"matches": "("}]}),
            json!({"all": ["a"], "extra": 1}),
        ] {
            let verdict = FinalMessageMatches.evaluate(&params, &stage(&["a"]));
            assert_eq!(
                (verdict.kind, verdict.score_bp, &verdict.raw["reason_code"]),
                (OutcomeKind::Grader, None, &json!("bad_params")),
                "{params}"
            );
        }
    }

    #[test]
    fn the_schema_accepts_substrings_and_regexes() {
        let validator =
            jsonschema::validator_for(&FinalMessageMatches.describe().params_schema).unwrap();
        assert!(validator.is_valid(&json!({"any": ["a"], "all": [{"matches": "b+"}]})));
        assert!(!validator.is_valid(&json!({"all": [{"regex": "b"}]})));
        assert!(!validator.is_valid(&json!({"all": ["a"], "extra": 1})));
        for nothing in [json!({}), json!({"any": []}), json!({"any": [], "all": []})] {
            assert!(!validator.is_valid(&nothing), "{nothing}");
        }
        assert!(validator.is_valid(&json!({"any": [], "all": ["a"]})));
    }
}
