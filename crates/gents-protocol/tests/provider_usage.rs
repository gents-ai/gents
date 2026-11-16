use gents_lean_vocab::families::provider_usage_observations as wire;
use gents_lean_vocab::family::recode;
use gents_protocol::usage::*;

#[test]
fn usage_observations_and_summaries_follow_the_executable_model() {
    gents_lean_vocab::families::provider_usage_observations::replay::<UsageCompleteness>(
        |input: &wire::UsageInput| {
            let observations: Vec<TokenUsage> = input.observations.iter().map(recode).collect();
            let expected = observations
                .iter()
                .map(|observation| {
                    let charged_tokens = match observation.charged_tokens() {
                        Ok(charged) => charged,
                        Err(error) => {
                            assert!(
                                observation.validate().is_err(),
                                "{observation:?}: an uncharged valid observation: {error}"
                            );
                            None
                        }
                    };
                    wire::ObservationOutcome {
                        valid: observation.validate().is_ok(),
                        completeness: observation.completeness(),
                        charged_tokens,
                        for_storage: recode(&observation.for_storage()),
                        separate_reasoning: recode(&observation.with_separate_reasoning()),
                        storage_completeness: observation.for_storage().completeness(),
                    }
                })
                .collect();
            let summary = TokenUsageSummary::from_observations(observations).ok();
            wire::UsageOutput {
                expected,
                charged_total: summary.and_then(|summary| summary.charged_total()),
                fresh_input: summary.and_then(|summary| summary.fresh_input()),
                summary: summary.map(|summary| recode(&summary)),
            }
        },
    );
}

#[test]
fn usage_completeness_names_follow_the_executable_model() {
    gents_lean_vocab::families::inference_call_usage_completeness_vocabulary::replay(&[
        UsageCompleteness::Missing,
        UsageCompleteness::Partial,
        UsageCompleteness::Complete,
    ]);
}
