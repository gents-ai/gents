use super::*;

#[test]
fn generated_run_addressing_matches_lean() {
    gents_lean_vocab::families::eval_run_addressing::replay(|input| {
        run_addressed(
            &RunSelector {
                trigger: input.selector.trigger.clone(),
                collection: input.selector.collection.clone(),
                of_seed: input.selector.of_seed,
            },
            input.seed.as_deref(),
            &FiredRun {
                trigger: input.run.trigger.clone(),
                watched: input.run.watched.clone(),
                source: input.run.source.clone(),
                prior_to_stage: input.run.prior_to_stage,
            },
        )
    });
}
