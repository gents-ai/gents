use gents_protocol::usage::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    name: String,
    observations: Vec<TokenUsage>,
    expected: Vec<Expected>,
    summary: Option<TokenUsageSummary>,
    charged_total: Option<u64>,
    fresh_input: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    valid: bool,
    completeness: UsageCompleteness,
    charged_tokens: Option<u64>,
    for_storage: TokenUsage,
    storage_completeness: UsageCompleteness,
    separate_reasoning: TokenUsage,
}

#[test]
fn usage_observations_and_summaries_follow_the_executable_model() {
    let snapshot: serde_json::Value =
        serde_json::from_str(&gents_lean_contract::load_contract_json().unwrap()).unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["provider_usage_cases"].clone()).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(
            case.observations.len(),
            case.expected.len(),
            "{}",
            case.name
        );
        for (observation, expected) in case.observations.iter().zip(&case.expected) {
            assert_eq!(
                observation.with_separate_reasoning(),
                expected.separate_reasoning,
                "{}",
                case.name
            );
            assert_eq!(
                observation.for_storage(),
                expected.for_storage,
                "{}",
                case.name
            );
            assert_eq!(
                observation.for_storage().completeness(),
                expected.storage_completeness,
                "{}",
                case.name
            );
            assert_eq!(
                observation.validate().is_ok(),
                expected.valid,
                "{}",
                case.name
            );
            assert_eq!(
                observation.completeness(),
                expected.completeness,
                "{}",
                case.name
            );
            match observation.charged_tokens() {
                Ok(charged) => assert_eq!(charged, expected.charged_tokens, "{}", case.name),
                Err(_) => assert!(!expected.valid, "{}", case.name),
            }
        }
        let summary = TokenUsageSummary::from_observations(case.observations).ok();
        assert_eq!(summary, case.summary, "{}", case.name);
        assert_eq!(
            summary.and_then(|summary| summary.charged_total()),
            case.charged_total,
            "{}",
            case.name
        );
        assert_eq!(
            summary.and_then(|summary| summary.fresh_input()),
            case.fresh_input,
            "{}",
            case.name
        );
    }
}
