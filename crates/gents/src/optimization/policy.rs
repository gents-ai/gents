//! The pure promotion policy `PolicyV2`: the integer gates that mirror
//! `Proofs/Optimization.lean`, the paired sign-flip permutation test, and
//! `decide`, which folds them into one journalled decision.
//!
//! Nothing here performs I/O, reads a clock, or runs an eval: every input
//! arrives as evidence, every quantity is an integer, and the Monte Carlo seed
//! is an argument. Running the trials that produce the evidence, and acting on
//! the decision, belong to the driver.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::eval::PairedEvidence;

pub const POLICY_VERSION: &str = "v2";

/// Cases at or below this count are tested by exact enumeration.
const EXACT_CASE_LIMIT: usize = 20;

// The exact branch packs one sign bit per case into a single `u64` mask, so the
// limit has to stay inside that word.
const _: () = assert!(EXACT_CASE_LIMIT < 64);

/// Frozen into a job at start. A rule change is a new version, never an edit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyV2 {
    pub min_pairs: u64,
    pub max_not_evidence_bp: u64,
    pub max_asymmetry_bp: u64,
    pub case_tolerance_bp: u64,
    pub min_effect_bp: u64,
    pub alpha_ppm: u64,
    pub max_rounds: u32,
    pub max_token_increase_bp: u64,
    /// Share of trials with null usage above which the caller passes
    /// `tokens: None` and the cost sub-gate is skipped. Consumed by the driver,
    /// not by `decide`.
    pub max_missing_usage_bp: u64,
    pub max_reruns: u32,
    pub monte_carlo_samples: u32,
}

impl PolicyV2 {
    /// Placeholder defaults. They are NOT calibrated; the A/A calibration run
    /// sets defensible values for a given definition and model.
    pub fn uncalibrated() -> Self {
        Self {
            min_pairs: 2,
            max_not_evidence_bp: 2000,
            max_asymmetry_bp: 1000,
            case_tolerance_bp: 5000,
            min_effect_bp: 500,
            alpha_ppm: 50_000,
            max_rounds: 3,
            max_token_increase_bp: 2500,
            max_missing_usage_bp: 2000,
            max_reruns: 1,
            monte_carlo_samples: 100_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// All gates, on a validation run.
    Improve,
    /// Sufficiency and non-regression only, once, on the held-out run.
    Confirm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    CaseRegression,
    CostRegression,
    NoImprovement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InconclusiveReason {
    Insufficient,
    /// `2^-n_cases` exceeds the effective significance level: no score pattern
    /// could ever be accepted with this many cases.
    TooFewCases,
    /// The per-case differences could not be placed on a common integer scale
    /// within the integer width, so the improvement test could not be run.
    UnscalableEvidence,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "decision", content = "reason")]
pub enum Decision {
    Accept,
    Reject(RejectReason),
    Inconclusive(InconclusiveReason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaseEvidence {
    pub case_id: String,
    pub pairs: u64,
    pub sum_baseline_bp: u64,
    pub sum_candidate_bp: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenTotals {
    pub baseline_tokens: u64,
    pub baseline_trials: u64,
    pub candidate_tokens: u64,
    pub candidate_trials: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence {
    pub cases_match: bool,
    pub cases: Vec<CaseEvidence>,
    pub keys: u64,
    pub dropped_baseline: u64,
    pub dropped_candidate: u64,
    /// `None` when too many trials lack usage; the cost gate is then skipped.
    pub tokens: Option<TokenTotals>,
}

/// One entry per expected case, in the given order. A case with no pairs stays
/// with `pairs: 0` and fails gate 1. A pair for an unexpected case clears
/// `cases_match`.
pub fn evidence_from_pairs(
    paired: &PairedEvidence,
    expected_cases: &[String],
    tokens: Option<TokenTotals>,
) -> Evidence {
    let mut by_case: BTreeMap<&str, CaseEvidence> = expected_cases
        .iter()
        .map(|id| {
            (
                id.as_str(),
                CaseEvidence {
                    case_id: id.clone(),
                    pairs: 0,
                    sum_baseline_bp: 0,
                    sum_candidate_bp: 0,
                },
            )
        })
        .collect();
    let mut cases_match = true;
    for pair in &paired.pairs {
        match by_case.get_mut(pair.case_id.as_str()) {
            Some(case) => {
                case.pairs += 1;
                case.sum_baseline_bp += pair.baseline_bp as u64;
                case.sum_candidate_bp += pair.candidate_bp as u64;
            }
            None => cases_match = false,
        }
    }
    Evidence {
        cases_match,
        cases: expected_cases
            .iter()
            .map(|id| by_case[id.as_str()].clone())
            .collect(),
        keys: paired.keys as u64,
        dropped_baseline: paired.dropped_baseline as u64,
        dropped_candidate: paired.dropped_candidate as u64,
        tokens,
    }
}

/// `Optimization.alphaEffectivePpm`: Bonferroni over the job's rounds.
pub fn alpha_effective_ppm(policy: &PolicyV2) -> u64 {
    policy.alpha_ppm / policy.max_rounds.max(1) as u64
}

fn enough_cases(policy: &PolicyV2, cases: usize) -> bool {
    // 1_000_000 <= alpha_eff * 2^n, saturating so large n is simply true.
    let power = 1u128.checked_shl(cases as u32).unwrap_or(u128::MAX);
    1_000_000u128 <= (alpha_effective_ppm(policy) as u128).saturating_mul(power)
}

/// Gate 1, `Optimization.sufficient`.
pub fn sufficient(policy: &PolicyV2, evidence: &Evidence) -> bool {
    let keys = evidence.keys as u128;
    let (db, dc) = (
        evidence.dropped_baseline as u128,
        evidence.dropped_candidate as u128,
    );
    evidence.cases_match
        && !evidence.cases.is_empty()
        && evidence
            .cases
            .iter()
            .all(|case| policy.min_pairs <= case.pairs)
        && db * 10_000 <= policy.max_not_evidence_bp as u128 * keys
        && dc * 10_000 <= policy.max_not_evidence_bp as u128 * keys
        && db.abs_diff(dc) * 10_000 <= policy.max_asymmetry_bp as u128 * keys
        && enough_cases(policy, evidence.cases.len())
}

/// Gate 2a, `Optimization.noCaseRegression`.
pub fn no_case_regression(policy: &PolicyV2, evidence: &Evidence) -> bool {
    evidence.cases.iter().all(|case| {
        case.sum_baseline_bp as u128
            <= case.sum_candidate_bp as u128 + policy.case_tolerance_bp as u128 * case.pairs as u128
    })
}

/// Gate 2b, `Optimization.costOk`.
pub fn cost_ok(policy: &PolicyV2, tokens: &TokenTotals) -> bool {
    tokens.candidate_tokens as u128 * tokens.baseline_trials as u128 * 10_000
        <= tokens.baseline_tokens as u128
            * tokens.candidate_trials as u128
            * (10_000 + policy.max_token_increase_bp as u128)
}

fn gcd(a: u128, b: u128) -> u128 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Per-case mean differences on a common integer scale: `diff_c * (L / pairs_c)`
/// where `L` is the least common multiple of the pair counts. Returns the scaled
/// differences and `L`, or `None` when that scale does not exist within the
/// integer width.
///
/// Every step is checked, because `decide` is total: the least common multiple,
/// each scaled difference, the combined magnitude of the differences, and
/// `cases * L`. Bounding the sum of the magnitudes bounds both the observed
/// total and every sign-flipped sum the permutation test forms from them, and
/// the last check is what lets `decide` divide by `cases * L`.
fn scaled_case_differences(evidence: &Evidence) -> Option<(Vec<i128>, i128)> {
    let counted = || evidence.cases.iter().filter(|case| case.pairs > 0);
    let mut lcm = 1u128;
    for case in counted() {
        let pairs = case.pairs as u128;
        lcm = (lcm / gcd(lcm, pairs)).checked_mul(pairs)?;
    }
    let lcm = i128::try_from(lcm).ok()?;
    let mut diffs = Vec::with_capacity(evidence.cases.len());
    let mut magnitude = 0u128;
    for case in counted() {
        let diff = (case.sum_candidate_bp as i128 - case.sum_baseline_bp as i128)
            .checked_mul(lcm / case.pairs as i128)?;
        magnitude = magnitude.checked_add(diff.unsigned_abs())?;
        diffs.push(diff);
    }
    if magnitude > i128::MAX as u128 {
        return None;
    }
    // Discarded: this only has to be known to fit, for `decide`'s mean.
    (diffs.len() as i128).checked_mul(lcm)?;
    Some((diffs, lcm))
}

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// One-sided paired sign-flip permutation test, in parts per million, rounded up.
/// The share of sign vectors whose signed sum is at least the observed sum. Exact
/// by enumeration up to `EXACT_CASE_LIMIT` cases; a seeded Monte Carlo above it,
/// with the usual `(count + 1) / (samples + 1)` correction.
pub fn permutation_p_ppm(diffs: &[i128], samples: u32, seed: u64) -> u64 {
    if diffs.is_empty() {
        return 1_000_000;
    }
    let observed: i128 = diffs.iter().sum();
    let signed_sum = |mask: u64| -> i128 {
        diffs
            .iter()
            .enumerate()
            .map(|(i, d)| if mask >> i & 1 == 1 { -*d } else { *d })
            .sum()
    };
    let (count, total): (u128, u128) = if diffs.len() <= EXACT_CASE_LIMIT {
        let total = 1u64 << diffs.len();
        (
            (0..total)
                .filter(|mask| signed_sum(*mask) >= observed)
                .count() as u128,
            total as u128,
        )
    } else {
        let mut state = seed;
        let mut count = 1u128;
        for _ in 0..samples {
            // More than 64 cases need more than one word of sign bits.
            let sum: i128 = diffs
                .chunks(64)
                .map(|chunk| {
                    let word = splitmix64(&mut state);
                    chunk
                        .iter()
                        .enumerate()
                        .map(|(i, d)| if word >> i & 1 == 1 { -*d } else { *d })
                        .sum::<i128>()
                })
                .sum();
            if sum >= observed {
                count += 1;
            }
        }
        (count, samples as u128 + 1)
    };
    ((count * 1_000_000 + total - 1) / total) as u64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gates {
    pub sufficient: bool,
    pub no_case_regression: bool,
    pub cost_ok: bool,
    pub improves: bool,
}

/// `Optimization.decideGates`. The first failing gate decides.
pub fn decide_gates(mode: Mode, gates: Gates, insufficient: InconclusiveReason) -> Decision {
    if !gates.sufficient {
        Decision::Inconclusive(insufficient)
    } else if !gates.no_case_regression {
        Decision::Reject(RejectReason::CaseRegression)
    } else if !gates.cost_ok {
        Decision::Reject(RejectReason::CostRegression)
    } else if mode == Mode::Confirm || gates.improves {
        Decision::Accept
    } else {
        Decision::Reject(RejectReason::NoImprovement)
    }
}

/// A decision and the numbers behind it, for the journal and for `show`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionReport {
    pub decision: Decision,
    pub policy_version: String,
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
    /// Mean per-case difference in basis points, rounded toward zero.
    pub mean_diff_bp: Option<i64>,
    pub p_ppm: Option<u64>,
    pub alpha_effective_ppm: u64,
    pub cost_skipped: bool,
}

/// `seed` feeds the Monte Carlo branch only. The driver derives it from the run
/// id, so a recomputed decision is identical.
pub fn decide(mode: Mode, policy: &PolicyV2, evidence: &Evidence, seed: u64) -> DecisionReport {
    let Some((diffs, lcm)) = scaled_case_differences(evidence) else {
        // No common scale, so no improvement test and no mean. The sign of a
        // case's difference is scale-free, so the counts still stand.
        let signs = |ordering: Ordering| {
            evidence
                .cases
                .iter()
                .filter(|case| {
                    case.pairs > 0 && case.sum_candidate_bp.cmp(&case.sum_baseline_bp) == ordering
                })
                .count() as u32
        };
        return DecisionReport {
            decision: Decision::Inconclusive(InconclusiveReason::UnscalableEvidence),
            policy_version: POLICY_VERSION.to_owned(),
            improved: signs(Ordering::Greater),
            tied: signs(Ordering::Equal),
            worsened: signs(Ordering::Less),
            mean_diff_bp: None,
            p_ppm: None,
            alpha_effective_ppm: alpha_effective_ppm(policy),
            cost_skipped: evidence.tokens.is_none(),
        };
    };
    let count = |predicate: fn(&i128) -> bool| diffs.iter().filter(|d| predicate(d)).count() as u32;
    let cases = diffs.len() as i128;
    let total: i128 = diffs.iter().sum();
    let p_ppm = (mode == Mode::Improve && !diffs.is_empty())
        .then(|| permutation_p_ppm(&diffs, policy.monte_carlo_samples, seed));
    let alpha = alpha_effective_ppm(policy);
    // Saturating: every factor is non-negative, so an out-of-range threshold is
    // simply one no total can reach.
    let effect_bp = (policy.min_effect_bp as i128)
        .saturating_mul(cases)
        .saturating_mul(lcm);
    let improves = p_ppm.is_some_and(|p| p <= alpha) && total >= effect_bp;
    let gates = Gates {
        sufficient: sufficient(policy, evidence),
        no_case_regression: no_case_regression(policy, evidence),
        cost_ok: evidence
            .tokens
            .as_ref()
            .is_none_or(|tokens| cost_ok(policy, tokens)),
        improves,
    };
    let insufficient = if evidence.cases_match
        && !evidence.cases.is_empty()
        && !enough_cases(policy, evidence.cases.len())
    {
        InconclusiveReason::TooFewCases
    } else {
        InconclusiveReason::Insufficient
    };
    DecisionReport {
        decision: decide_gates(mode, gates, insufficient),
        policy_version: POLICY_VERSION.to_owned(),
        improved: count(|d| *d > 0),
        tied: count(|d| *d == 0),
        worsened: count(|d| *d < 0),
        mean_diff_bp: (cases > 0).then(|| (total / (cases * lcm)) as i64),
        p_ppm,
        alpha_effective_ppm: alpha,
        cost_skipped: evidence.tokens.is_none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::{Pair, PairedEvidence};
    use proptest::prelude::*;

    fn params() -> PolicyV2 {
        PolicyV2 {
            min_pairs: 2,
            max_not_evidence_bp: 2000,
            max_asymmetry_bp: 1000,
            case_tolerance_bp: 3000,
            min_effect_bp: 500,
            alpha_ppm: 50_000,
            max_rounds: 3,
            max_token_increase_bp: 2500,
            max_missing_usage_bp: 2000,
            max_reruns: 1,
            monte_carlo_samples: 100_000,
        }
    }

    fn paired(cases: &[(&str, &[(u32, u32)])]) -> PairedEvidence {
        let mut pairs = Vec::new();
        for (case, scores) in cases {
            for (index, (baseline_bp, candidate_bp)) in scores.iter().enumerate() {
                pairs.push(Pair {
                    case_id: (*case).into(),
                    trial_index: index as u32,
                    baseline_bp: *baseline_bp,
                    candidate_bp: *candidate_bp,
                });
            }
        }
        PairedEvidence {
            keys: pairs.len(),
            pairs,
            dropped_baseline: 0,
            dropped_candidate: 0,
        }
    }

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("case-{i}")).collect()
    }

    fn uniform(n: usize, baseline: u32, candidate: u32) -> (PairedEvidence, Vec<String>) {
        let ids = names(n);
        let scores = [(baseline, candidate), (baseline, candidate)];
        let cases: Vec<(&str, &[(u32, u32)])> =
            ids.iter().map(|id| (id.as_str(), &scores[..])).collect();
        (paired(&cases), ids)
    }

    #[test]
    fn the_permutation_test_is_exact_on_hand_computed_values() {
        // Six positive differences: only the all-plus sign vector reaches the
        // observed sum, so p = 1/64.
        assert_eq!(permutation_p_ppm(&[1, 2, 3, 4, 5, 6], 100_000, 7), 15_625);
        // All zero: every sign vector ties, so p = 1.
        assert_eq!(permutation_p_ppm(&[0, 0, 0, 0], 100_000, 7), 1_000_000);
        // {3,1}: sums are 4, 2, -2, -4; one of four is >= 4.
        assert_eq!(permutation_p_ppm(&[3, 1], 100_000, 7), 250_000);
        // {-3,-1}: observed -4; all four sums are >= -4.
        assert_eq!(permutation_p_ppm(&[-3, -1], 100_000, 7), 1_000_000);
        assert_eq!(permutation_p_ppm(&[], 100_000, 7), 1_000_000);
    }

    #[test]
    fn the_monte_carlo_branch_is_deterministic_for_a_seed() {
        let diffs: Vec<i128> = (1..=24).collect();
        let a = permutation_p_ppm(&diffs, 20_000, 42);
        assert_eq!(a, permutation_p_ppm(&diffs, 20_000, 42));
        assert!(
            a < 1_000,
            "24 aligned differences are overwhelmingly significant: {a}"
        );
    }

    #[test]
    fn six_improving_cases_are_accepted_and_five_cannot_be() {
        let (six, ids) = uniform(6, 5_000, 8_000);
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&six, &ids, None),
            1,
        );
        assert_eq!(report.decision, Decision::Accept);
        assert_eq!((report.improved, report.tied, report.worsened), (6, 0, 0));
        assert_eq!(report.p_ppm, Some(15_625));
        assert_eq!(report.mean_diff_bp, Some(3_000));

        let (five, ids) = uniform(5, 5_000, 8_000);
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&five, &ids, None),
            1,
        );
        assert_eq!(
            report.decision,
            Decision::Inconclusive(InconclusiveReason::TooFewCases)
        );
    }

    #[test]
    fn a_tie_is_rejected_and_confirm_ignores_improvement() {
        let (tie, ids) = uniform(6, 5_000, 5_000);
        let evidence = evidence_from_pairs(&tie, &ids, None);
        assert_eq!(
            decide(Mode::Improve, &params(), &evidence, 1).decision,
            Decision::Reject(RejectReason::NoImprovement)
        );
        assert_eq!(
            decide(Mode::Confirm, &params(), &evidence, 1).decision,
            Decision::Accept
        );
    }

    #[test]
    fn a_broken_case_rejects_even_when_the_mean_improves() {
        let ids = names(6);
        let good = [(5_000u32, 9_000u32), (5_000, 9_000)];
        let broken = [(9_000u32, 1_000u32), (9_000, 1_000)];
        let mut cases: Vec<(&str, &[(u32, u32)])> = ids
            .iter()
            .skip(1)
            .map(|id| (id.as_str(), &good[..]))
            .collect();
        cases.push((ids[0].as_str(), &broken[..]));
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&paired(&cases), &ids, None),
            1,
        );
        assert_eq!(
            report.decision,
            Decision::Reject(RejectReason::CaseRegression)
        );
    }

    #[test]
    fn a_more_expensive_candidate_is_rejected_and_missing_usage_skips_the_gate() {
        let (six, ids) = uniform(6, 5_000, 8_000);
        let costly = TokenTotals {
            baseline_tokens: 1_000,
            baseline_trials: 10,
            candidate_tokens: 2_000,
            candidate_trials: 10,
        };
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&six, &ids, Some(costly)),
            1,
        );
        assert_eq!(
            report.decision,
            Decision::Reject(RejectReason::CostRegression)
        );
        assert!(!report.cost_skipped);
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&six, &ids, None),
            1,
        );
        assert!(report.cost_skipped);
        assert_eq!(report.decision, Decision::Accept);
    }

    #[test]
    fn an_unexpected_or_missing_case_is_insufficient() {
        let (six, ids) = uniform(6, 5_000, 8_000);
        let mut expected = ids.clone();
        expected.push("case-missing".into());
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&six, &expected, None),
            1,
        );
        assert_eq!(
            report.decision,
            Decision::Inconclusive(InconclusiveReason::Insufficient)
        );
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&six, &ids[..5], None),
            1,
        );
        assert_eq!(
            report.decision,
            Decision::Inconclusive(InconclusiveReason::Insufficient)
        );
    }

    #[test]
    fn asymmetric_exclusion_is_insufficient() {
        let (mut six, ids) = uniform(6, 5_000, 8_000);
        six.keys += 2;
        six.dropped_candidate = 2;
        let report = decide(
            Mode::Improve,
            &params(),
            &evidence_from_pairs(&six, &ids, None),
            1,
        );
        assert_eq!(
            report.decision,
            Decision::Inconclusive(InconclusiveReason::Insufficient)
        );
    }

    #[test]
    fn pairwise_coprime_pair_counts_are_unscalable_not_a_panic() {
        // Thirty distinct primes: their least common multiple is their product,
        // which is far wider than 128 bits, so there is no common scale.
        let primes: [u64; 30] = [
            2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83,
            89, 97, 101, 103, 107, 109, 113,
        ];
        let evidence = Evidence {
            cases_match: true,
            cases: primes
                .iter()
                .enumerate()
                .map(|(i, pairs)| CaseEvidence {
                    case_id: format!("case-{i}"),
                    pairs: *pairs,
                    sum_baseline_bp: 5_000 * pairs,
                    sum_candidate_bp: 8_000 * pairs,
                })
                .collect(),
            keys: primes.iter().sum(),
            dropped_baseline: 0,
            dropped_candidate: 0,
            tokens: None,
        };
        let report = decide(Mode::Improve, &params(), &evidence, 1);
        assert_eq!(
            report.decision,
            Decision::Inconclusive(InconclusiveReason::UnscalableEvidence)
        );
        assert_eq!(report.p_ppm, None);
        assert_eq!(report.mean_diff_bp, None);
        assert_eq!((report.improved, report.tied, report.worsened), (30, 0, 0));
    }

    #[test]
    fn gates_costs_and_decisions_match_lean() {
        let lean = crate::lean_vocab_test::lean_optimization_cases();
        let policy = PolicyV2 {
            min_pairs: lean.params.min_pairs,
            max_not_evidence_bp: lean.params.max_not_evidence_bp,
            max_asymmetry_bp: lean.params.max_asymmetry_bp,
            case_tolerance_bp: lean.params.case_tolerance_bp,
            alpha_ppm: lean.params.alpha_ppm,
            max_rounds: lean.params.max_rounds,
            max_token_increase_bp: lean.params.max_token_increase_bp,
            ..params()
        };
        assert_eq!(
            alpha_effective_ppm(&policy),
            lean.params.alpha_effective_ppm
        );
        for row in &lean.gates {
            let evidence = Evidence {
                cases_match: row.cases_match,
                cases: row
                    .cases
                    .iter()
                    .enumerate()
                    .map(|(i, c)| CaseEvidence {
                        case_id: format!("case-{i}"),
                        pairs: c.pairs,
                        sum_baseline_bp: c.sum_baseline,
                        sum_candidate_bp: c.sum_candidate,
                    })
                    .collect(),
                keys: row.keys,
                dropped_baseline: row.dropped_baseline,
                dropped_candidate: row.dropped_candidate,
                tokens: None,
            };
            assert_eq!(
                sufficient(&policy, &evidence),
                row.sufficient,
                "{}",
                row.name
            );
            assert_eq!(
                no_case_regression(&policy, &evidence),
                row.no_case_regression,
                "{}",
                row.name
            );
        }
        for row in &lean.costs {
            let totals = TokenTotals {
                baseline_tokens: row.baseline_tokens,
                baseline_trials: row.baseline_trials,
                candidate_tokens: row.candidate_tokens,
                candidate_trials: row.candidate_trials,
            };
            assert_eq!(cost_ok(&policy, &totals), row.cost_ok, "{}", row.name);
        }
        for row in &lean.decisions {
            let mode = if row.mode == "improve" {
                Mode::Improve
            } else {
                Mode::Confirm
            };
            let decision = decide_gates(
                mode,
                Gates {
                    sufficient: row.sufficient,
                    no_case_regression: row.no_case_regression,
                    cost_ok: row.cost_ok,
                    improves: row.improves,
                },
                InconclusiveReason::Insufficient,
            );
            let got = match decision {
                Decision::Accept => "accept",
                Decision::Inconclusive(_) => "inconclusive",
                Decision::Reject(RejectReason::CaseRegression) => "reject_case_regression",
                Decision::Reject(RejectReason::CostRegression) => "reject_cost_regression",
                Decision::Reject(RejectReason::NoImprovement) => "reject_no_improvement",
            };
            assert_eq!(got, row.decision, "{row:?}");
        }
    }

    proptest! {
        /// Lowering one candidate score never produces an Accept that was not there.
        #[test]
        fn lowering_a_candidate_score_never_creates_an_accept(
            scores in proptest::collection::vec((0u32..=10_000, 0u32..=10_000), 12),
            victim in 0usize..12,
            drop in 1u32..=10_000,
        ) {
            let ids = names(6);
            let build = |scores: &[(u32, u32)]| {
                let cases: Vec<(&str, &[(u32, u32)])> = ids
                    .iter()
                    .enumerate()
                    .map(|(i, id)| (id.as_str(), &scores[i * 2..i * 2 + 2]))
                    .collect();
                evidence_from_pairs(&paired(&cases), &ids, None)
            };
            let mut worse = scores.clone();
            worse[victim].1 = worse[victim].1.saturating_sub(drop);
            for mode in [Mode::Improve, Mode::Confirm] {
                if decide(mode, &params(), &build(&worse), 9).decision == Decision::Accept {
                    prop_assert_eq!(decide(mode, &params(), &build(&scores), 9).decision, Decision::Accept);
                }
            }
        }
    }
}
