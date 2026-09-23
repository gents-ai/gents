//! `compare`: two cells' paired statistics (spec 4a §1).
//!
//! The pairs are the optimizer's: each report's counted slot scores joined by
//! `pair_trials` on `(case_id, trial_index)`. The statistics are
//! `optimization::policy::decide`'s own numbers on those pairs, seeded as
//! `optimization show` seeds a decision over the same runs, so the p-value an
//! operator reads is the one the optimizer would compute. Without a policy
//! they are computed under the placeholder defaults, whose only influence on
//! them is the Monte Carlo sample count above twenty cases.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::eval::report::build::{
    CellReport, EvalReport, SlotClass, SlotReport, SlotScore, SlotVerdict,
};
use crate::eval::report::evidence::CellUsage;
use crate::eval::report::refused;
use crate::eval::{pair_trials, CaseTrialScore, DefinitionRef, Pair, PairedEvidence, TrialScore};
use crate::optimization::evidence::{decision_seed, totals};
use crate::optimization::policy::{
    cost_ok, decide, evidence_from_pairs, no_case_regression, sufficient, CaseEvidence,
    DecisionReport, Evidence, Mode, PolicyV2,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseComparison {
    pub case_id: String,
    pub pairs: u64,
    pub baseline_mean_bp: Option<u32>,
    pub candidate_mean_bp: Option<u32>,
    /// Mean paired difference, candidate minus baseline, truncated.
    pub diff_bp: Option<i64>,
}

/// Each gate `decide` applies, as far as it can be read from outside it.
///
/// `sufficient`, `no_case_regression` and `cost_ok` are `optimization::policy`'s
/// own gate functions on the evidence `decide` read. `decide` does not expose
/// its improvement gate, so `significant` and `min_effect` restate it from the
/// decision's numbers; a test pins them to `decide`'s outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GateView {
    pub sufficient: bool,
    pub no_case_regression: bool,
    /// `None` when too many trials reported no usage and the gate is skipped.
    pub cost_ok: Option<bool>,
    /// `p_ppm <= alpha_effective_ppm`, the first half of `decide`'s
    /// improvement gate.
    pub significant: bool,
    /// `mean_diff_bp >= min_effect_bp`, the second half of `decide`'s
    /// improvement gate; `false` with no mean. `decide` compares the
    /// untruncated total, which agrees with the truncated mean for any
    /// `min_effect_bp` of at least one. At zero it does not: a mean between
    /// -1 and 0 bp truncates to 0, and whether `decide` found it met depends
    /// on a sign the report cannot see (`decide` exposes no gate breakdown).
    /// So `None` exactly when `min_effect_bp` is 0 and `mean_diff_bp` is 0.
    pub min_effect: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PolicyOutcome {
    pub report: DecisionReport,
    pub gates: GateView,
    /// False while the policy is the placeholder defaults ([`is_placeholder`]),
    /// which only the A/A calibration replaces.
    pub calibrated: bool,
}

/// Whether `policy` is the placeholder defaults: `PolicyV2::uncalibrated()`
/// with the policy's own `max_rounds`. `max_rounds` sizes a job's budget (the
/// Bonferroni divisor), not a calibrated value, so the defaults sized to any
/// round count are still uncalibrated.
pub fn is_placeholder(policy: &PolicyV2) -> bool {
    *policy
        == PolicyV2 {
            max_rounds: policy.max_rounds,
            ..PolicyV2::uncalibrated()
        }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Comparison {
    pub comparability: DefinitionRef,
    pub baseline_run: String,
    pub baseline_cell: String,
    pub candidate_run: String,
    pub candidate_cell: String,
    pub cases: Vec<CaseComparison>,
    pub improved: u32,
    pub tied: u32,
    pub worsened: u32,
    pub mean_diff_bp: Option<i64>,
    pub pairs: usize,
    pub dropped_baseline: usize,
    pub dropped_candidate: usize,
    /// Pairs in which an unknown side was imputed worst-case.
    pub imputed: usize,
    pub p_ppm: Option<u64>,
    /// Only after [`Comparison::with_policy`]: the optimizer decides by
    /// default, never a comparison.
    pub policy: Option<PolicyOutcome>,
    /// Every `(case_id, trial_index)` either cell has, in that order, with
    /// each side's counted verdicts.
    pub trials: Vec<PairedTrial>,
    #[serde(skip)]
    paired: PairedEvidence,
    #[serde(skip)]
    case_ids: Vec<String>,
    #[serde(skip)]
    usage: [CellUsage; 2],
    #[serde(skip)]
    seed: u64,
}

/// Compare `baseline_cell` of one report with `candidate_cell` of another,
/// or of the same one. Refused across comparability versions or definition
/// digests, and for a cell a report does not have.
pub fn compare(
    baseline: &EvalReport,
    candidate: &EvalReport,
    baseline_cell: &str,
    candidate_cell: &str,
) -> Result<Comparison> {
    let (left, right) = (&baseline.run, &candidate.run);
    if left.definition.definition_id != right.definition.definition_id
        || left.definition.comparability_version != right.definition.comparability_version
    {
        return Err(refused(format!(
            "run {} froze {} comparability version {} and run {} froze {} comparability version {}; runs compare only within one comparability version",
            left.run_id,
            left.definition.definition_id,
            left.definition.comparability_version,
            right.run_id,
            right.definition.definition_id,
            right.definition.comparability_version,
        )));
    }
    if left.definition.digest != right.definition.digest {
        return Err(refused(format!(
            "runs {} and {} froze different digests of eval definition {:?}; they are not comparable",
            left.run_id, right.run_id, left.definition.definition_id
        )));
    }
    let base = cell(baseline, baseline_cell)?;
    let cand = cell(candidate, candidate_cell)?;
    let (base_scores, cand_scores) = (trial_scores(base), trial_scores(cand));
    let paired = pair_trials(&base_scores, &cand_scores);
    // Seeded over the distinct run ids, as `optimization show` seeds a
    // decision over the runs it read.
    let mut seed_runs = vec![left.run_id.clone()];
    if right.run_id != left.run_id {
        seed_runs.push(right.run_id.clone());
    }
    let mut comparison = Comparison {
        comparability: left.definition.clone(),
        baseline_run: left.run_id.clone(),
        baseline_cell: baseline_cell.to_owned(),
        candidate_run: right.run_id.clone(),
        candidate_cell: candidate_cell.to_owned(),
        cases: Vec::new(),
        improved: 0,
        tied: 0,
        worsened: 0,
        mean_diff_bp: None,
        pairs: paired.pairs.len(),
        dropped_baseline: paired.dropped_baseline,
        dropped_candidate: paired.dropped_candidate,
        imputed: imputed(&base_scores, &cand_scores),
        p_ppm: None,
        policy: None,
        trials: paired_trials(base, cand),
        paired,
        case_ids: base.cases.iter().map(|case| case.case_id.clone()).collect(),
        usage: [base.cell_usage, cand.cell_usage],
        seed: decision_seed(&seed_runs),
    };
    let defaults = PolicyV2::uncalibrated();
    comparison.cases = comparison
        .evidence(&defaults)
        .cases
        .iter()
        .map(case_comparison)
        .collect();
    comparison.restate(&defaults);
    Ok(comparison)
}

impl Comparison {
    /// The evidence `decide` reads under `policy`: the pairs, the baseline
    /// run's case list, and the two cells' token totals by
    /// `optimization::evidence::totals`.
    pub fn evidence(&self, policy: &PolicyV2) -> Evidence {
        evidence_from_pairs(
            &self.paired,
            &self.case_ids,
            totals(self.usage[0], self.usage[1], policy.max_missing_usage_bp),
        )
    }

    /// `pair_trials`' own pairs, each naming its `(case_id, trial_index)`.
    pub(crate) fn pairs(&self) -> &[Pair] {
        &self.paired.pairs
    }

    /// The Monte Carlo seed: `decision_seed` over the distinct run ids.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Decide under `policy` and restate the statistics under it.
    pub fn with_policy(mut self, policy: &PolicyV2) -> Self {
        let evidence = self.evidence(policy);
        let report = self.restate(policy);
        let gates = GateView {
            sufficient: sufficient(policy, &evidence),
            no_case_regression: no_case_regression(policy, &evidence),
            cost_ok: evidence
                .tokens
                .as_ref()
                .map(|tokens| cost_ok(policy, tokens)),
            significant: report
                .p_ppm
                .is_some_and(|p| p <= report.alpha_effective_ppm),
            min_effect: match report.mean_diff_bp {
                Some(0) if policy.min_effect_bp == 0 => None,
                mean => Some(mean.is_some_and(|mean| {
                    mean >= i64::try_from(policy.min_effect_bp).unwrap_or(i64::MAX)
                })),
            },
        };
        self.policy = Some(PolicyOutcome {
            report,
            gates,
            calibrated: !is_placeholder(policy),
        });
        self
    }

    /// Decide under `policy` and restate the statistics from that decision.
    pub(crate) fn restate(&mut self, policy: &PolicyV2) -> DecisionReport {
        let report = decide(Mode::Improve, policy, &self.evidence(policy), self.seed);
        self.improved = report.improved;
        self.tied = report.tied;
        self.worsened = report.worsened;
        self.mean_diff_bp = report.mean_diff_bp;
        self.p_ppm = report.p_ppm;
        report
    }
}

/// One cell's side of a trial key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SideTrial {
    pub class: SlotClass,
    pub score: SlotScore,
    pub verdicts: Vec<SlotVerdict>,
}

/// Both cells at one `(case_id, trial_index)`, as the breakdowns read them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PairedTrial {
    pub case_id: String,
    pub trial_index: u32,
    pub baseline: Option<SideTrial>,
    pub candidate: Option<SideTrial>,
}

fn paired_trials(baseline: &CellReport, candidate: &CellReport) -> Vec<PairedTrial> {
    let side = |slot: &SlotReport| SideTrial {
        class: slot.class,
        score: slot.counted,
        verdicts: slot.verdicts.clone(),
    };
    fn entry<'a>(
        trials: &'a mut BTreeMap<(String, u32), PairedTrial>,
        slot: &SlotReport,
    ) -> &'a mut PairedTrial {
        trials
            .entry((slot.case_id.clone(), slot.trial_index))
            .or_insert_with(|| PairedTrial {
                case_id: slot.case_id.clone(),
                trial_index: slot.trial_index,
                baseline: None,
                candidate: None,
            })
    }
    let mut trials: BTreeMap<(String, u32), PairedTrial> = BTreeMap::new();
    for slot in &baseline.slots {
        entry(&mut trials, slot).baseline = Some(side(slot));
    }
    for slot in &candidate.slots {
        entry(&mut trials, slot).candidate = Some(side(slot));
    }
    trials.into_values().collect()
}

fn cell<'a>(report: &'a EvalReport, cell_id: &str) -> Result<&'a CellReport> {
    report
        .cells
        .iter()
        .find(|cell| cell.cell_id == cell_id)
        .ok_or_else(|| refused(format!("run {} has no cell {cell_id:?}", report.run.run_id)))
}

fn trial_scores(cell: &CellReport) -> Vec<TrialScore> {
    cell.slots
        .iter()
        .filter_map(|slot| {
            slot.counted.trial_score().map(|score| TrialScore {
                case_id: slot.case_id.clone(),
                trial_index: slot.trial_index,
                score,
            })
        })
        .collect()
}

/// How many of `pair_trials`' pairs were imputed: keys both cells hold as
/// evidence where at least one side is unknown.
///
/// `pair_trials` (`eval::scoring`) reports its pairs but not which of them it
/// imputed, so this mirrors its rule: a key a cell repeats reads as unknown in
/// that cell, a not-evidence side drops the key, and an unknown side on a kept
/// key is imputed worst-case. A test pins the count to `pair_trials`' own
/// result on the same scores.
fn imputed(baseline: &[TrialScore], candidate: &[TrialScore]) -> usize {
    let index = |scores: &[TrialScore]| {
        let mut index: BTreeMap<(String, u32), CaseTrialScore> = BTreeMap::new();
        for score in scores {
            index
                .entry((score.case_id.clone(), score.trial_index))
                .and_modify(|repeated| *repeated = CaseTrialScore::Unknown)
                .or_insert(score.score);
        }
        index
    };
    let (left, right) = (index(baseline), index(candidate));
    left.iter()
        .filter(|&(key, base)| {
            right.get(key).is_some_and(|cand| {
                *base != CaseTrialScore::NotEvidence
                    && *cand != CaseTrialScore::NotEvidence
                    && (*base == CaseTrialScore::Unknown || *cand == CaseTrialScore::Unknown)
            })
        })
        .count()
}

fn case_comparison(case: &CaseEvidence) -> CaseComparison {
    let paired = case.pairs > 0;
    let mean = |sum: u64| paired.then(|| (sum / case.pairs) as u32);
    CaseComparison {
        case_id: case.case_id.clone(),
        pairs: case.pairs,
        baseline_mean_bp: mean(case.sum_baseline_bp),
        candidate_mean_bp: mean(case.sum_candidate_bp),
        diff_bp: paired.then(|| {
            (case.sum_candidate_bp as i64 - case.sum_baseline_bp as i64) / case.pairs as i64
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::EvalDefinition;
    use crate::eval::report::build::build;
    use crate::eval::report::evidence::{cell_trial_scores, RunRows};
    use crate::eval::report::fixtures::{at, definition, fail, pass, record, Outcome, Rows, RUN};
    use crate::eval::report::report_refused;
    use crate::eval::OutcomeKind;
    use crate::optimization::policy::permutation_p_ppm;

    fn report(definition: &EvalDefinition, rows: &Rows, cells: &[&str], trials: u32) -> EvalReport {
        let run = record(RUN, definition, cells, trials);
        build(&run, &rows.trials, &rows.verdicts, definition, &[]).unwrap()
    }

    fn run_rows(definition: &EvalDefinition, rows: &Rows) -> RunRows {
        let mut case_ids: Vec<String> = definition
            .cases
            .iter()
            .map(|case| case.case_id.clone())
            .collect();
        case_ids.sort();
        RunRows {
            run_id: RUN.into(),
            case_ids,
            invalidated: false,
            trials: rows.trials.clone(),
            verdicts: rows.verdicts.clone(),
        }
    }

    fn refusal(result: Result<Comparison>) -> String {
        let error = result.expect_err("expected a refusal");
        report_refused(&error)
            .unwrap_or_else(|| panic!("expected a ReportRefused, got {error:#}"))
            .0
            .clone()
    }

    /// a improves, b ties, c worsens, d's baseline is unknown (imputed a
    /// pass, so it ties), and e's baseline is not evidence (no pair).
    fn five_cases() -> (EvalDefinition, Rows) {
        let definition = definition(&["a", "b", "c", "d", "e"]);
        let rows = Rows::default()
            .add(RUN, at("baseline", "a", 0, 1), fail())
            .add(RUN, at("candidate", "a", 0, 1), pass())
            .add(RUN, at("baseline", "b", 0, 1), pass())
            .add(RUN, at("candidate", "b", 0, 1), pass())
            .add(RUN, at("baseline", "c", 0, 1), pass())
            .add(RUN, at("candidate", "c", 0, 1), fail())
            .add(
                RUN,
                at("baseline", "d", 0, 1),
                Outcome::Verdict(OutcomeKind::Grader, None),
            )
            .add(RUN, at("candidate", "d", 0, 1), pass())
            .add(
                RUN,
                at("baseline", "e", 0, 1),
                Outcome::Verdict(OutcomeKind::Infrastructure, None),
            )
            .add(RUN, at("candidate", "e", 0, 1), pass());
        (definition, rows)
    }

    #[test]
    fn cases_are_counted_by_the_sign_of_their_mean_difference() {
        let (definition, rows) = five_cases();
        let report = report(&definition, &rows, &["baseline", "candidate"], 1);
        let comparison = compare(&report, &report, "baseline", "candidate").unwrap();
        assert_eq!(
            (comparison.improved, comparison.tied, comparison.worsened),
            (1, 2, 1)
        );
        assert_eq!(comparison.mean_diff_bp, Some(0));
        assert_eq!(
            (
                comparison.pairs,
                comparison.dropped_baseline,
                comparison.dropped_candidate,
                comparison.imputed
            ),
            (4, 1, 0, 1)
        );
        let case = |id: &str| {
            comparison
                .cases
                .iter()
                .find(|case| case.case_id == id)
                .unwrap()
                .clone()
        };
        assert_eq!((case("e").pairs, case("e").diff_bp), (0, None));
        assert_eq!(
            (
                case("d").baseline_mean_bp,
                case("d").candidate_mean_bp,
                case("d").diff_bp
            ),
            (Some(10_000), Some(10_000), Some(0)),
            "an unknown baseline is imputed a pass"
        );
        assert_eq!(case("a").diff_bp, Some(10_000));
    }

    #[test]
    fn pairs_match_pair_trials_and_p_matches_the_permutation_test() {
        let (definition, rows) = five_cases();
        let report = report(&definition, &rows, &["baseline", "candidate"], 1);
        let comparison = compare(&report, &report, "baseline", "candidate").unwrap();

        let documents = run_rows(&definition, &rows);
        let paired = pair_trials(
            &cell_trial_scores(&definition, &documents, "baseline"),
            &cell_trial_scores(&definition, &documents, "candidate"),
        );
        assert_eq!(
            (
                comparison.pairs,
                comparison.dropped_baseline,
                comparison.dropped_candidate
            ),
            (
                paired.pairs.len(),
                paired.dropped_baseline,
                paired.dropped_candidate
            )
        );
        // One pair per case, so the common scale is 1 and the per-case
        // differences in case order a, b, c, d are the raw ones. One run:
        // its id once, as `optimization show` seeds a one-run decision.
        let seed = decision_seed(&[RUN.to_owned()]);
        assert_eq!(comparison.seed(), seed);
        assert_eq!(
            comparison.p_ppm,
            Some(permutation_p_ppm(
                &[10_000, 0, -10_000, 0],
                PolicyV2::uncalibrated().monte_carlo_samples,
                seed
            ))
        );
    }

    #[test]
    fn compare_refuses_runs_that_are_not_comparable_and_cells_that_do_not_exist() {
        let (definition, rows) = five_cases();
        let report = report(&definition, &rows, &["baseline", "candidate"], 1);

        let mut other_version = report.clone();
        other_version.run.definition.comparability_version = 2;
        let reason = refusal(compare(&report, &other_version, "baseline", "candidate"));
        assert!(reason.contains("one comparability version"), "{reason}");

        let mut other_digest = report.clone();
        other_digest.run.definition.digest = "sha256:elsewhere".into();
        let reason = refusal(compare(&report, &other_digest, "baseline", "candidate"));
        assert!(reason.contains("different digests"), "{reason}");

        let reason = refusal(compare(&report, &report, "ghost", "candidate"));
        assert_eq!(reason, "run run has no cell \"ghost\"");
        let reason = refusal(compare(&report, &report, "baseline", "ghost"));
        assert_eq!(reason, "run run has no cell \"ghost\"");
    }

    /// `imputed` mirrors `pair_trials`' imputation rule: removing every
    /// imputed key from the input (an unknown or repeated key made not
    /// evidence) loses exactly the pairs `imputed` counts.
    #[test]
    fn imputed_counts_the_pairs_pair_trials_imputes() {
        use CaseTrialScore::{NotEvidence, Scored, Unknown};
        let score = |case: &str, trial_index, score| TrialScore {
            case_id: case.into(),
            trial_index,
            score,
        };
        let baseline = vec![
            score("a", 0, Scored(0)),
            score("b", 0, Unknown),
            score("c", 0, Scored(10_000)),
            score("d", 0, Unknown),
            score("e", 0, NotEvidence),
            score("f", 0, Scored(5_000)),
            score("f", 0, Scored(5_000)),
            score("g", 0, Unknown),
            score("h", 0, Scored(0)),
        ];
        let candidate = vec![
            score("a", 0, Scored(10_000)),
            score("b", 0, Scored(10_000)),
            score("c", 0, Unknown),
            score("d", 0, Unknown),
            score("e", 0, Unknown),
            score("f", 0, Scored(5_000)),
            score("g", 0, NotEvidence),
            score("i", 0, Unknown),
        ];
        let strip = |scores: &[TrialScore]| -> Vec<TrialScore> {
            let mut seen = BTreeMap::<(String, u32), usize>::new();
            for trial in scores {
                *seen
                    .entry((trial.case_id.clone(), trial.trial_index))
                    .or_default() += 1;
            }
            let mut stripped: Vec<TrialScore> = Vec::new();
            for trial in scores {
                let key = (trial.case_id.clone(), trial.trial_index);
                if stripped
                    .iter()
                    .any(|kept| (kept.case_id.clone(), kept.trial_index) == key)
                {
                    continue;
                }
                let unknown = trial.score == Unknown || seen[&key] > 1;
                stripped.push(TrialScore {
                    score: if unknown { NotEvidence } else { trial.score },
                    ..trial.clone()
                });
            }
            stripped
        };
        let all = pair_trials(&baseline, &candidate).pairs.len();
        let certain = pair_trials(&strip(&baseline), &strip(&candidate))
            .pairs
            .len();
        assert_eq!(imputed(&baseline, &candidate), all - certain);
        assert_eq!(
            imputed(&baseline, &candidate),
            4,
            "b, c, d and the repeated f"
        );
    }

    use crate::optimization::evidence::{decision_evidence, BASELINE_CELL, CANDIDATE_CELL};
    use crate::optimization::policy::{Decision, RejectReason};

    const SIX: [&str; 6] = ["a", "b", "c", "d", "e", "f"];

    /// Twenty-one case ids, for the Monte Carlo branch above twenty cases.
    const MANY: [&str; 21] = [
        "c00", "c01", "c02", "c03", "c04", "c05", "c06", "c07", "c08", "c09", "c10", "c11", "c12",
        "c13", "c14", "c15", "c16", "c17", "c18", "c19", "c20",
    ];

    /// `cases`, two trials each: the baseline fails and the candidate passes
    /// every one. The last `unmetered` candidate trials report no usage.
    fn improving(cases: &[&'static str], unmetered: usize) -> (EvalDefinition, Rows) {
        let definition = definition(cases);
        let mut rows = Rows::default();
        let mut left = cases.len() * 2 - unmetered;
        for &case in cases {
            for index in 0..2 {
                rows = rows.add(RUN, at(BASELINE_CELL, case, index, 1), fail());
                rows = rows.add(RUN, at(CANDIDATE_CELL, case, index, 1), pass());
                if left == 0 {
                    rows = rows.unmetered();
                } else {
                    left -= 1;
                }
            }
        }
        (definition, rows)
    }

    /// The comparison's policy outcome and `decide` on the optimizer's own
    /// evidence for the same documents.
    fn against_the_optimizer(
        cases: &[&'static str],
        unmetered: usize,
        policy: &PolicyV2,
    ) -> (Comparison, DecisionReport) {
        let (definition, rows) = improving(cases, unmetered);
        let report = report(&definition, &rows, &[BASELINE_CELL, CANDIDATE_CELL], 2);
        let comparison = compare(&report, &report, BASELINE_CELL, CANDIDATE_CELL)
            .unwrap()
            .with_policy(policy);
        let expected = decide(
            Mode::Improve,
            policy,
            &decision_evidence(
                &definition,
                &[run_rows(&definition, &rows)],
                policy.max_missing_usage_bp,
            ),
            decision_seed(&[RUN.to_owned()]),
        );
        (comparison, expected)
    }

    #[test]
    fn the_policy_outcome_is_decide_on_the_optimizers_evidence() {
        let policy = PolicyV2::uncalibrated();
        let (comparison, expected) = against_the_optimizer(&SIX, 0, &policy);
        let outcome = comparison.policy.as_ref().expect("with_policy sets it");
        assert_eq!(outcome.report, expected);
        assert_eq!(expected.decision, Decision::Accept);
        assert!(!expected.cost_skipped);
        assert!(
            !outcome.calibrated,
            "the placeholder defaults are uncalibrated"
        );
        assert_eq!(
            outcome.gates,
            GateView {
                sufficient: true,
                no_case_regression: true,
                cost_ok: Some(true),
                significant: true,
                min_effect: Some(true),
            }
        );
        assert_eq!(comparison.p_ppm, expected.p_ppm);
    }

    #[test]
    fn the_cost_gate_is_skipped_exactly_when_the_optimizer_skips_it() {
        // Five of 24 trials unmetered is past the 2000 bp tolerance.
        let policy = PolicyV2::uncalibrated();
        let (comparison, expected) = against_the_optimizer(&SIX, 5, &policy);
        let outcome = comparison.policy.as_ref().unwrap();
        assert_eq!(outcome.report, expected);
        assert!(expected.cost_skipped);
        assert_eq!(outcome.gates.cost_ok, None);
    }

    #[test]
    fn a_policy_other_than_the_defaults_is_calibrated() {
        let policy = PolicyV2 {
            min_pairs: 1,
            ..PolicyV2::uncalibrated()
        };
        let (comparison, expected) = against_the_optimizer(&SIX, 0, &policy);
        let outcome = comparison.policy.unwrap();
        assert_eq!(outcome.report, expected);
        assert!(outcome.calibrated);
    }

    /// The defaults sized to another round count are still the placeholder.
    #[test]
    fn the_defaults_with_other_max_rounds_are_a_placeholder() {
        let policy = PolicyV2 {
            max_rounds: 2,
            ..PolicyV2::uncalibrated()
        };
        assert!(super::is_placeholder(&policy));
        assert!(!super::is_placeholder(&PolicyV2 {
            min_pairs: 1,
            ..policy.clone()
        }));
        let (comparison, _) = against_the_optimizer(&SIX, 0, &policy);
        assert!(!comparison.policy.unwrap().calibrated);
    }

    /// Above twenty cases `decide` samples, so the seed matters.
    /// The comparison of one run's two cells seeds as `optimization show`
    /// recomputes that run's decision: over the run id once.
    #[test]
    fn above_twenty_cases_the_p_value_is_the_one_optimization_show_computes() {
        let policy = PolicyV2::uncalibrated();
        let (comparison, expected) = against_the_optimizer(&MANY, 0, &policy);
        assert_eq!(comparison.policy.as_ref().unwrap().report, expected);
        assert_eq!(comparison.p_ppm, expected.p_ppm);
        assert!(expected.p_ppm.is_some());
    }

    /// Before `with_policy` a comparison carries no decision: the optimizer
    /// decides, never a comparison.
    #[test]
    fn a_comparison_has_no_policy_outcome_until_asked() {
        let (definition, rows) = five_cases();
        let report = report(&definition, &rows, &["baseline", "candidate"], 1);
        let comparison = compare(&report, &report, "baseline", "candidate").unwrap();
        assert!(comparison.policy.is_none());
    }

    /// `significant` and `min_effect` restate `decide`'s improvement gate,
    /// which `policy.rs` does not expose: whenever every other gate passes,
    /// `decide` accepts exactly when both hold. Each combination is reached.
    #[test]
    fn significance_and_effect_restate_decides_improvement_gate() {
        // Per case, two trials each: (baseline passes, candidate passes).
        let scenario = |outcomes: &[(&'static str, bool, bool)], policy: &PolicyV2| {
            let cases: Vec<&str> = outcomes.iter().map(|(case, ..)| *case).collect();
            let definition = definition(&cases);
            let mut rows = Rows::default();
            for &(case, base, cand) in outcomes {
                for index in 0..2 {
                    let outcome = |passes: bool| if passes { pass() } else { fail() };
                    rows = rows.add(RUN, at(BASELINE_CELL, case, index, 1), outcome(base));
                    rows = rows.add(RUN, at(CANDIDATE_CELL, case, index, 1), outcome(cand));
                }
            }
            let report = report(&definition, &rows, &[BASELINE_CELL, CANDIDATE_CELL], 2);
            let outcome = compare(&report, &report, BASELINE_CELL, CANDIDATE_CELL)
                .unwrap()
                .with_policy(policy)
                .policy
                .unwrap();
            let expected = decide(
                Mode::Improve,
                policy,
                &decision_evidence(
                    &definition,
                    &[run_rows(&definition, &rows)],
                    policy.max_missing_usage_bp,
                ),
                decision_seed(&[RUN.to_owned()]),
            );
            assert_eq!(outcome.report, expected);
            let gates = &outcome.gates;
            assert!(gates.sufficient && gates.no_case_regression);
            assert_eq!(gates.cost_ok, Some(true));
            let improves = gates.significant && gates.min_effect == Some(true);
            assert_eq!(
                expected.decision,
                if improves {
                    Decision::Accept
                } else {
                    Decision::Reject(RejectReason::NoImprovement)
                }
            );
            (gates.significant, gates.min_effect)
        };
        let defaults = PolicyV2::uncalibrated();
        let improving: Vec<_> = SIX.iter().map(|&case| (case, false, true)).collect();
        let tied: Vec<_> = SIX.iter().map(|&case| (case, true, true)).collect();
        let one_better: Vec<_> = SIX.iter().map(|&case| (case, case != "f", true)).collect();
        assert_eq!(scenario(&improving, &defaults), (true, Some(true)));
        assert_eq!(
            scenario(
                &improving,
                &PolicyV2 {
                    min_effect_bp: 20_000,
                    ..defaults.clone()
                }
            ),
            (true, Some(false))
        );
        assert_eq!(scenario(&one_better, &defaults), (false, Some(true)));
        assert_eq!(scenario(&tied, &defaults), (false, Some(false)));
    }

    /// At `min_effect_bp: 0` a mean difference between -1 and 0 bp truncates
    /// to a `mean_diff_bp` of 0, whose sign the report cannot see; `decide`
    /// compares the untruncated total and finds the effect short.
    #[test]
    fn a_sub_basis_point_mean_under_a_zero_effect_threshold_is_not_restated() {
        let policy = PolicyV2 {
            min_effect_bp: 0,
            ..PolicyV2::uncalibrated()
        };
        let definition = definition(&SIX);
        let mut rows = Rows::default();
        for &case in &SIX {
            for index in 0..2 {
                // Case a loses one basis point per trial; the rest tie.
                let candidate_bp = if case == "a" { 9_999 } else { 10_000 };
                rows = rows.add(RUN, at(BASELINE_CELL, case, index, 1), pass());
                rows = rows.add(
                    RUN,
                    at(CANDIDATE_CELL, case, index, 1),
                    Outcome::Verdict(OutcomeKind::Passed, Some(candidate_bp)),
                );
            }
        }
        let report = report(&definition, &rows, &[BASELINE_CELL, CANDIDATE_CELL], 2);
        let outcome = compare(&report, &report, BASELINE_CELL, CANDIDATE_CELL)
            .unwrap()
            .with_policy(&policy)
            .policy
            .unwrap();
        let expected = decide(
            Mode::Improve,
            &policy,
            &decision_evidence(
                &definition,
                &[run_rows(&definition, &rows)],
                policy.max_missing_usage_bp,
            ),
            decision_seed(&[RUN.to_owned()]),
        );
        assert_eq!(outcome.report, expected);
        assert_eq!(expected.mean_diff_bp, Some(0), "-1/6 bp truncates to 0");
        assert_eq!(expected.worsened, 1);
        assert_eq!(
            expected.decision,
            Decision::Reject(RejectReason::NoImprovement)
        );
        assert!(outcome.gates.sufficient && outcome.gates.no_case_regression);
        assert_eq!(outcome.gates.min_effect, None);
    }
}
