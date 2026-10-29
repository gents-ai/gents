use super::*;

#[test]
fn calibration_scales_up_and_never_down() {
    assert_eq!(calibrated_input_tokens(561_000, 18_000), 1_009_800);
    assert_eq!(calibrated_input_tokens(3, 15_000), 5, "rounds up");
    assert_eq!(
        calibrated_input_tokens(1_000, 5_000),
        1_000,
        "floored at one"
    );
    let calibration = ContextCalibration::with_ratio(1);
    assert_eq!(
        calibration.ratio_basis_points(),
        UNIT_INPUT_RATIO_BASIS_POINTS
    );
    assert_eq!(
        calibration.observe(500, 1_000),
        Some(UNIT_INPUT_RATIO_BASIS_POINTS)
    );
    assert_eq!(
        calibration.observe(1_000_000, 1),
        Some(MAX_INPUT_RATIO_BASIS_POINTS)
    );
    assert_eq!(calibration.observe(0, 1_000), None);
    assert_eq!(calibration.observe(1_000, 0), None);
    assert_eq!(
        calibration.ratio_basis_points(),
        MAX_INPUT_RATIO_BASIS_POINTS
    );
}

#[test]
fn live_claude_observations_calibrate_the_estimate() {
    let calibration =
        ContextCalibration::prior_for(super::super::ProviderInputProfile::ClaudeMessages);
    assert_eq!(
        calibration.ratio_basis_points(),
        CLAUDE_MESSAGES_PRIOR_INPUT_RATIO_BASIS_POINTS
    );
    let threshold = effective_input_budget(1_000_000, 0.75);
    assert!(
        calibration.calibrated(561_000) > threshold,
        "the live ~561k estimate is over a 1M window's threshold before any observation"
    );
    let mut highest = 0;
    for (estimated, observed) in [
        (65_755, 109_881),
        (80_986, 129_572),
        (81_424, 131_227),
        (131_259, 205_599),
        (132_293, 207_656),
    ] {
        let ratio = observed_input_ratio_basis_points(observed, estimated).unwrap();
        highest = highest.max(ratio);
        assert_eq!(calibration.observe(observed, estimated), Some(highest));
        let calibrated = calibration.calibrated(estimated) as u64;
        assert!(
            calibrated >= observed,
            "the estimate never undercounts a recent observation: {estimated} -> {observed}: {calibrated}"
        );
    }
    assert_eq!(
        calibration.ratio_basis_points(),
        observed_input_ratio_basis_points(109_881, 65_755).unwrap(),
        "the live sequence's highest ratio (1.67x) holds while it is in the window"
    );
    assert_eq!(
        ContextCalibration::prior_for(super::super::ProviderInputProfile::OpenAiChatCompletions)
            .ratio_basis_points(),
        UNIT_INPUT_RATIO_BASIS_POINTS
    );
}

#[test]
fn estimator_budget_admits_exactly_the_calibrated_budget() {
    for ratio in [10_000, 13_333, 17_855, 18_000, 40_000] {
        for budget in [0, 1, 7, 4_875, 96_000, 750_000] {
            let estimator_budget = estimator_input_budget(budget, ratio);
            for estimate in estimator_budget.saturating_sub(2)..=estimator_budget + 2 {
                assert_eq!(
                    calibrated_input_tokens(estimate, ratio) > budget,
                    estimate > estimator_budget,
                    "ratio={ratio} budget={budget} estimate={estimate}"
                );
            }
        }
    }
}

#[test]
fn compaction_budget_reserves_the_configured_output() {
    assert_eq!(compaction_input_budget(1_000_000, 0.75, 32_000), 750_000);
    assert_eq!(compaction_input_budget(200_000, 0.95, 32_000), 168_000);
    assert_eq!(compaction_input_budget(128_000, 0.9, 32_000), 96_000);
    assert_eq!(compaction_input_budget(128_000, 0.75, usize::MAX), 96_000);
    assert_eq!(compaction_input_budget(128_000, 1.0, 8_192), 119_808);
}

#[test]
fn only_contract_usage_is_an_input_observation() {
    use crate::provider_usage::contract_input_tokens;
    assert_eq!(
        contract_input_tokens(131_227, 120_000, 5_000),
        Some(131_227),
        "a cache-inclusive input is the whole count"
    );
    assert_eq!(
        contract_input_tokens(3, 125_000, 6_224),
        None,
        "a pre-normalization uncached input is not an observation"
    );
    assert_eq!(contract_input_tokens(0, 0, 0), None);
}

fn anchor(input_tokens: u64, output_tokens: u64, estimated_input_tokens: usize) -> ContextAnchor {
    ContextAnchor {
        input_tokens,
        output_tokens,
        estimated_input_tokens,
    }
}

#[test]
fn anchored_context_is_provider_total_plus_calibrated_growth() {
    let ratio = observed_input_ratio_basis_points(100_000, 60_000).unwrap();
    let calibration = ContextCalibration::with_anchor(ratio, anchor(100_000, 2_000, 60_000));

    let unchanged = calibration.context(60_000);
    assert_eq!(
        unchanged.anchored,
        Some(102_000),
        "the response is counted once"
    );
    assert_eq!(unchanged.tokens, 102_000);

    let grown = calibration.context(70_000);
    let growth = calibrated_input_tokens(10_000, ratio);
    assert_eq!(grown.anchored, Some(100_000 + 2_000 + (growth - 2_000)));
    assert_eq!(grown.tokens, grown.anchored.unwrap().max(grown.calibrated));
    assert_eq!(grown.ratio_basis_points, ratio);

    let reduced = calibration.context(50_000);
    assert_eq!(
        reduced.anchored, None,
        "a request smaller than the anchor's was reduced since"
    );
    assert_eq!(reduced.tokens, reduced.calibrated);

    calibration.invalidate_anchor();
    assert_eq!(calibration.context(70_000).anchored, None);
}

#[test]
fn anchor_raises_the_count_and_never_lowers_it() {
    let calibration = ContextCalibration::with_anchor(12_000, anchor(100_000, 0, 60_000));
    let context = calibration.context(60_000);
    assert_eq!(context.calibrated, 72_000);
    assert_eq!(
        context.tokens, 100_000,
        "the provider's count beats a stale low ratio"
    );

    let calibration = ContextCalibration::with_anchor(30_000, anchor(100_000, 0, 60_000));
    let context = calibration.context(60_000);
    assert_eq!(context.anchored, Some(100_000));
    assert_eq!(
        context.tokens, 180_000,
        "the calibrated floor wins when higher"
    );
}

#[test]
fn ratio_only_copies_never_anchor_or_share_state() {
    let session = ContextCalibration::with_anchor(17_000, anchor(100_000, 0, 60_000));
    let summary = session.ratio_only();
    assert_eq!(summary.ratio_basis_points(), 17_000);
    assert_eq!(summary.context(60_000).anchored, None);
    summary.anchor_accepted(anchor(5_000, 0, 4_000));
    summary.observe(40_000, 10_000);
    assert_eq!(summary.anchor(), None);
    assert_eq!(session.ratio_basis_points(), 17_000);
    assert_eq!(session.anchor(), Some(anchor(100_000, 0, 60_000)));

    let shared = session.clone();
    shared.anchor_accepted(anchor(120_000, 10, 70_000));
    assert_eq!(
        session.anchor(),
        Some(anchor(120_000, 10, 70_000)),
        "clones share state"
    );
}

#[test]
fn estimator_units_invert_the_calibration_conservatively() {
    let calibration = ContextCalibration::with_ratio(17_855);
    for tokens in [0, 1, 7, 750_000, 1_001_685] {
        let units = calibration.estimator_units(tokens);
        assert!(calibration.calibrated(units) >= tokens, "{tokens}");
        assert!(
            units <= estimator_input_budget(tokens, 17_855) + 1,
            "{tokens}"
        );
    }
}

#[test]
fn post_compaction_request_fits_the_recovery_band() {
    assert_eq!(
        compaction_retention_target(usize::MAX, 750_000, 50_000, 32_000),
        518_000,
        "0.8 x 750k - 50k fixed - 32k summary reserve"
    );
    for (budget, fixed, reserve) in [
        (750_000, 0, 0),
        (750_000, 50_000, 32_000),
        (168_000, 12_345, 8_192),
        (96_000, 76_799, 1),
        (6_001, 0, 0),
    ] {
        let band = recovery_band_budget(budget);
        let tail = compaction_retention_target(usize::MAX, budget, fixed, reserve);
        assert_eq!(
            fixed + reserve + tail,
            band,
            "budget={budget} fixed={fixed} reserve={reserve}: the band is filled exactly"
        );
        assert!(band <= budget * 4 / 5);
    }
    assert_eq!(
        compaction_retention_target(20_000, 750_000, 50_000, 32_000),
        20_000,
        "keep_recent binds below the band"
    );
    assert_eq!(
        compaction_retention_target(usize::MAX, 750_000, 590_000, 32_000),
        0,
        "fixed layers and reserve past the band floor the tail at zero"
    );
}

#[test]
fn ratio_rises_at_once_and_falls_only_as_high_observations_age_out() {
    let calibration =
        ContextCalibration::with_ratio(CLAUDE_MESSAGES_PRIOR_INPUT_RATIO_BASIS_POINTS);
    assert_eq!(
        calibration.observe(260, 100),
        Some(26_000),
        "2.6x takes effect at once"
    );
    assert_eq!(
        calibration.observe(130, 100),
        Some(26_000),
        "a 1.3x observation after 2.6x still yields 2.6x"
    );
    for _ in 2..CALIBRATION_OBSERVATION_WINDOW {
        assert_eq!(calibration.observe(130, 100), Some(26_000));
    }
    assert_eq!(
        calibration.observe(130, 100),
        Some(13_000),
        "the 2.6x observation ages out after {CALIBRATION_OBSERVATION_WINDOW} newer ones"
    );
    assert_eq!(
        calibration.observe(170, 100),
        Some(17_000),
        "and a rise is immediate again"
    );
}

#[test]
fn seeded_observations_share_the_window_with_new_ones() {
    let calibration = ContextCalibration::with_observations(18_000, [26_000, 13_000]);
    assert_eq!(calibration.ratio_basis_points(), 26_000);
    for _ in 0..CALIBRATION_OBSERVATION_WINDOW - 2 {
        calibration.observe(130, 100);
    }
    assert_eq!(calibration.ratio_basis_points(), 26_000);
    calibration.observe(130, 100);
    assert_eq!(calibration.ratio_basis_points(), 13_000);
    assert_eq!(
        ContextCalibration::with_observations(18_000, []).ratio_basis_points(),
        18_000,
        "without observations the prior applies"
    );
    assert_eq!(
        ContextCalibration::with_observations(18_000, [13_000]).ratio_basis_points(),
        13_000,
        "an observation replaces the prior, it does not floor at it"
    );
    let summary = calibration.ratio_only();
    summary.observe(400, 100);
    assert_eq!(summary.ratio_basis_points(), 40_000);
    assert_eq!(
        calibration.ratio_basis_points(),
        13_000,
        "copies do not share the window"
    );
}
