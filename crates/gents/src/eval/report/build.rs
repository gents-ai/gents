//! `build`: one run's documents as the operator reads them (spec 4a §1).
//!
//! Pure. The unit is the slot `(cell, case, trial_index)`, classified by its
//! latest attempt; attempts are a separate total, so a retry never inflates
//! what completed. A slot's score is read at its latest *completed* attempt
//! through `evidence::counted_slots`, the selection the optimizer pairs on,
//! so a report and a decision never disagree about what a slot scored.

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Serialize;

use crate::document_config::{EvalDefinition, EvalReducer, EvalSplit, EvalTier};
use crate::eval::report::evidence::{cell_usage, counted_slots, CellUsage, RunRows};
use crate::eval::report::refused;
use crate::eval::runner::freeze::definition_ref;
use crate::eval::{
    case_means_bp, case_trial_score, classify, exposure, headline_bp, CaseTrialScore, CellSpec,
    DefinitionRef, EvidenceClass, Invalidation, OutcomeKind, ProviderReason, RunHeader, RunRecord,
    StageCompletion, SubjectRef, TrialRecord, TrialScore, TrialUsage, VerdictRecord,
};

/// Bumped when a field's meaning changes; an added field does not bump it.
pub const REPORT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EvalReport {
    pub report_version: u32,
    pub run: RunSummary,
    pub cells: Vec<CellReport>,
    /// Non-invalidated runs of this definition version on this split,
    /// this one included.
    pub exposure: usize,
    /// The installed definition no longer digests to the one the run froze
    /// (or is gone). `build` never sets it; `report::store` does, from the
    /// run's frozen copy.
    pub definition_changed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub definition: DefinitionRef,
    pub split: EvalSplit,
    pub purpose: String,
    pub created_at: String,
    pub invalidated: Option<Invalidation>,
    pub trials_per_case: u32,
    pub seed_base: i64,
    pub concurrency: u32,
    pub max_infra_retries: u32,
    pub breaker_threshold: u32,
    pub source_commit: String,
    pub source_dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CellReport {
    pub cell_id: String,
    pub label: String,
    pub subject: SubjectRef,
    pub slots: Vec<SlotReport>,
    pub cases: Vec<CaseReport>,
    pub headline_bp: Option<u32>,
    /// Summed over each slot's counted attempt.
    pub usage: TrialUsage,
    /// `evidence::cell_usage` for this cell: the numbers the cost gate reads,
    /// computed as the optimizer computes them, so `compare` and the
    /// optimizer share one tally.
    pub cell_usage: CellUsage,
    /// Trial rows of this cell, every attempt included.
    pub attempts: u32,
    pub counts: SlotCounts,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlotReport {
    pub case_id: String,
    pub trial_index: u32,
    pub class: SlotClass,
    pub attempts: u32,
    pub score_bp: Option<u32>,
    /// What pairing reads for this slot.
    pub counted: SlotScore,
    /// The counted attempt's verdicts, in (stage, check) order; empty when
    /// no attempt completed.
    pub verdicts: Vec<SlotVerdict>,
    /// The latest row, completed or not.
    pub latest: Option<AttemptSummary>,
}

/// One verdict that counts for a slot: of its counted attempt, after regrade
/// supersession.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SlotVerdict {
    pub verdict_id: String,
    pub stage_id: String,
    pub check: String,
    pub tier: EvalTier,
    pub kind: OutcomeKind,
    pub provider_reason: Option<ProviderReason>,
    pub score_bp: Option<u32>,
    pub weight: u32,
    /// `raw.reason_code`, the check's own contract.
    pub reason_code: Option<String>,
}

/// A verdict's `raw.reason_code`, the check's own contract; `None` when the
/// check wrote none or wrote something other than a string.
pub fn reason_code(record: &VerdictRecord) -> Option<String> {
    record
        .raw
        .get("reason_code")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn slot_verdict(record: &VerdictRecord) -> SlotVerdict {
    SlotVerdict {
        verdict_id: record.verdict_id.clone(),
        stage_id: record.stage_id.clone(),
        check: record.check.clone(),
        tier: record.tier,
        kind: record.kind,
        provider_reason: record.provider_reason,
        score_bp: record.score_bp,
        weight: record.weight,
        reason_code: reason_code(record),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotClass {
    Pass,
    Fail,
    Unknown,
    NotEvidence,
    /// The latest row has a null completion: crashed or cancelled.
    Abandoned,
    /// No row yet.
    Planned,
}

/// A slot's counted case-trial score, or `Absent` when no attempt completed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "bp")]
pub enum SlotScore {
    Absent,
    NotEvidence,
    Unknown,
    Scored(u32),
}

impl SlotScore {
    fn of(score: Option<CaseTrialScore>) -> Self {
        match score {
            None => Self::Absent,
            Some(CaseTrialScore::NotEvidence) => Self::NotEvidence,
            Some(CaseTrialScore::Unknown) => Self::Unknown,
            Some(CaseTrialScore::Scored(bp)) => Self::Scored(bp),
        }
    }

    /// The score `pair_trials` reads, or `None` for a slot it never saw.
    pub fn trial_score(self) -> Option<CaseTrialScore> {
        match self {
            Self::Absent => None,
            Self::NotEvidence => Some(CaseTrialScore::NotEvidence),
            Self::Unknown => Some(CaseTrialScore::Unknown),
            Self::Scored(bp) => Some(CaseTrialScore::Scored(bp)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttemptSummary {
    pub trial_id: String,
    pub attempt: u32,
    pub trial_agent_did: String,
    pub session_id: String,
    /// Relative to `<launching home>/eval/runs`. A locator, never identity.
    pub home_hint: Option<String>,
    pub evidence_digest: Option<String>,
    /// Empty while the attempt is open.
    pub stages: Vec<StageCompletion>,
    pub usage: TrialUsage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CaseReport {
    pub case_id: String,
    pub reducer: EvalReducer,
    pub mean_bp: Option<u32>,
    pub counts: SlotCounts,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SlotCounts {
    pub pass: u32,
    pub fail: u32,
    pub unknown: u32,
    pub not_evidence: u32,
    pub abandoned: u32,
    pub planned: u32,
}

impl SlotCounts {
    fn add(&mut self, class: SlotClass) {
        match class {
            SlotClass::Pass => self.pass += 1,
            SlotClass::Fail => self.fail += 1,
            SlotClass::Unknown => self.unknown += 1,
            SlotClass::NotEvidence => self.not_evidence += 1,
            SlotClass::Abandoned => self.abandoned += 1,
            SlotClass::Planned => self.planned += 1,
        }
    }
}

/// Build `run`'s report from its rows.
///
/// `peers` are the owner's run headers, this run's included; they feed only
/// `exposure`. Refused, never guessed at: rows of another run, a trial naming
/// a cell or case the run did not freeze, and a definition that no longer
/// digests to what the run froze (its stage ids could mean something else).
pub fn build(
    run: &RunRecord,
    trials: &[TrialRecord],
    verdicts: &[VerdictRecord],
    definition: &EvalDefinition,
    peers: &[RunHeader],
) -> Result<EvalReport> {
    let run_id = run.run_id.as_str();
    let origin = &run.origin;
    if definition_ref(definition)?.digest != origin.definition.digest {
        return Err(refused(format!(
            "eval definition {:?} no longer digests to what run {run_id} froze; its report cannot be read against the installed definition",
            origin.definition.definition_id
        )));
    }
    for trial in trials {
        let identity = &trial.identity;
        if identity.run_id != run_id {
            return Err(refused(format!(
                "trial {} belongs to run {}, not {run_id}",
                identity.trial_id, identity.run_id
            )));
        }
        if !origin
            .cells
            .iter()
            .any(|cell| cell.cell_id == identity.cell_id)
        {
            return Err(refused(format!(
                "run {run_id} did not freeze cell {:?} (trial {})",
                identity.cell_id, identity.trial_id
            )));
        }
        if !origin.case_ids.contains(&identity.case_id) {
            return Err(refused(format!(
                "run {run_id} did not freeze case {:?} (trial {})",
                identity.case_id, identity.trial_id
            )));
        }
    }
    if let Some(verdict) = verdicts.iter().find(|verdict| verdict.run_id != run_id) {
        return Err(refused(format!(
            "verdict {} belongs to run {}, not {run_id}",
            verdict.verdict_id, verdict.run_id
        )));
    }

    let rows = RunRows {
        run_id: run_id.to_owned(),
        case_ids: origin.case_ids.clone(),
        invalidated: run.invalidated.is_some(),
        trials: trials.to_vec(),
        verdicts: verdicts.to_vec(),
    };
    let cells = origin
        .cells
        .iter()
        .map(|cell| cell_report(definition, &rows, cell, origin.trials_per_case))
        .collect();
    Ok(EvalReport {
        report_version: REPORT_VERSION,
        run: RunSummary {
            run_id: run_id.to_owned(),
            definition: origin.definition.clone(),
            split: origin.split,
            purpose: origin.purpose.clone(),
            created_at: run.created_at.clone(),
            invalidated: run.invalidated.clone(),
            trials_per_case: origin.trials_per_case,
            seed_base: origin.seed_base,
            concurrency: origin.concurrency,
            max_infra_retries: origin.max_infra_retries,
            breaker_threshold: origin.breaker_threshold,
            source_commit: origin.source_commit.clone(),
            source_dirty: origin.source_dirty,
        },
        cells,
        exposure: exposure(
            peers,
            &origin.definition.definition_id,
            origin.definition.comparability_version,
            origin.split,
        ),
        definition_changed: false,
    })
}

/// A slot's counted attempt: its score, whether every weighted acceptance
/// verdict passed, the row itself, and the verdicts that count.
struct Counted<'a> {
    score: CaseTrialScore,
    all_pass: bool,
    record: &'a TrialRecord,
    verdicts: Vec<SlotVerdict>,
}

fn cell_report(
    definition: &EvalDefinition,
    rows: &RunRows,
    cell: &CellSpec,
    trials_per_case: u32,
) -> CellReport {
    // Keyed by trial as well: `counted_slots` selected each view within one
    // trial, so a verdict id another trial repeats never stands in for it.
    let by_id: BTreeMap<(&str, &str), &VerdictRecord> = rows
        .verdicts
        .iter()
        .map(|verdict| {
            (
                (verdict.trial_id.as_str(), verdict.verdict_id.as_str()),
                verdict,
            )
        })
        .collect();
    let counted: BTreeMap<(&str, u32), Counted<'_>> =
        counted_slots(definition, rows, &cell.cell_id)
            .into_iter()
            .map(|(record, case, views)| {
                let all_pass = views
                    .iter()
                    .filter(|view| view.tier == EvalTier::Acceptance && view.weight > 0)
                    .all(|view| classify(view.kind, view.provider_reason) == EvidenceClass::Pass);
                (
                    (
                        record.identity.case_id.as_str(),
                        record.identity.trial_index,
                    ),
                    Counted {
                        score: case_trial_score(case.reducer, &views),
                        all_pass,
                        record,
                        verdicts: views
                            .iter()
                            .filter_map(|view| {
                                by_id.get(&(
                                    record.identity.trial_id.as_str(),
                                    view.verdict_id.as_str(),
                                ))
                            })
                            .map(|verdict| slot_verdict(verdict))
                            .collect(),
                    },
                )
            })
            .collect();
    let in_cell: Vec<&TrialRecord> = rows
        .trials
        .iter()
        .filter(|trial| trial.identity.cell_id == cell.cell_id)
        .collect();

    let mut report = CellReport {
        cell_id: cell.cell_id.clone(),
        label: cell.label.clone(),
        subject: cell.subject.clone(),
        slots: Vec::new(),
        cases: Vec::new(),
        headline_bp: None,
        usage: TrialUsage::default(),
        cell_usage: cell_usage(rows, &cell.cell_id),
        attempts: in_cell.len() as u32,
        counts: SlotCounts::default(),
    };
    let mut scores = Vec::new();
    for case_id in &rows.case_ids {
        let reducer = definition
            .cases
            .iter()
            .find(|case| &case.case_id == case_id)
            .map(|case| case.reducer)
            .unwrap_or_default();
        let mut case_counts = SlotCounts::default();
        let mut case_scores = Vec::new();
        for trial_index in 0..trials_per_case {
            let attempts: Vec<&TrialRecord> = in_cell
                .iter()
                .copied()
                .filter(|trial| {
                    &trial.identity.case_id == case_id && trial.identity.trial_index == trial_index
                })
                .collect();
            let latest = attempts
                .iter()
                .copied()
                .max_by_key(|trial| trial.identity.attempt);
            let counted_here = counted.get(&(case_id.as_str(), trial_index));
            let class = slot_class(latest, counted_here);
            case_counts.add(class);
            report.counts.add(class);
            if let Some(counted_here) = counted_here {
                case_scores.push(TrialScore {
                    case_id: case_id.clone(),
                    trial_index,
                    score: counted_here.score,
                });
                if let Some(completion) = &counted_here.record.completion {
                    let usage = &completion.usage;
                    report.usage = TrialUsage {
                        input_tokens: sum(report.usage.input_tokens, usage.input_tokens),
                        output_tokens: sum(report.usage.output_tokens, usage.output_tokens),
                    };
                }
            }
            let score = counted_here.map(|counted_here| counted_here.score);
            report.slots.push(SlotReport {
                case_id: case_id.clone(),
                trial_index,
                class,
                attempts: attempts.len() as u32,
                score_bp: match score {
                    Some(CaseTrialScore::Scored(bp)) => Some(bp),
                    _ => None,
                },
                counted: SlotScore::of(score),
                verdicts: counted_here
                    .map(|counted_here| counted_here.verdicts.clone())
                    .unwrap_or_default(),
                latest: latest.map(attempt_summary),
            });
        }
        report.cases.push(CaseReport {
            case_id: case_id.clone(),
            reducer,
            mean_bp: case_means_bp(&case_scores).get(case_id).copied(),
            counts: case_counts,
        });
        scores.extend(case_scores);
    }
    report.headline_bp = headline_bp(&scores);
    report
}

/// Spec 4a §1: no row is Planned, an open latest row is Abandoned, and
/// otherwise the case-trial class of the latest completed attempt.
fn slot_class(latest: Option<&TrialRecord>, counted: Option<&Counted<'_>>) -> SlotClass {
    let Some(latest) = latest else {
        return SlotClass::Planned;
    };
    if latest.completion.is_none() {
        return SlotClass::Abandoned;
    }
    match counted.map(|counted| (counted.score, counted.all_pass)) {
        Some((CaseTrialScore::NotEvidence, _)) => SlotClass::NotEvidence,
        Some((CaseTrialScore::Scored(_), true)) => SlotClass::Pass,
        Some((CaseTrialScore::Scored(_), false)) => SlotClass::Fail,
        // `build` has checked every case against the definition, so a
        // completed slot always has a counted attempt; without one it would
        // say nothing either way.
        Some((CaseTrialScore::Unknown, _)) | None => SlotClass::Unknown,
    }
}

fn attempt_summary(record: &TrialRecord) -> AttemptSummary {
    let identity = &record.identity;
    let completion = record.completion.as_ref();
    AttemptSummary {
        trial_id: identity.trial_id.clone(),
        attempt: identity.attempt,
        trial_agent_did: identity.trial_agent_did.clone(),
        session_id: identity.session_id.clone(),
        home_hint: identity.home_hint.clone(),
        evidence_digest: completion.and_then(|completion| completion.evidence_digest.clone()),
        stages: completion
            .map(|completion| completion.stages.clone())
            .unwrap_or_default(),
        usage: completion
            .map(|completion| completion.usage.clone())
            .unwrap_or_default(),
    }
}

/// Two token counts, absent only when both are.
fn sum(total: Option<u64>, one: Option<u64>) -> Option<u64> {
    match (total, one) {
        (None, None) => None,
        (total, one) => Some(total.unwrap_or(0) + one.unwrap_or(0)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::eval::report::fixtures::{
        at, definition, fail, pass, record, trial_id, Outcome, Rows, RUN,
    };

    #[test]
    fn slot_scores_map_to_what_pairing_reads_and_serialize_by_name() {
        assert_eq!(SlotScore::Absent.trial_score(), None);
        assert_eq!(
            SlotScore::NotEvidence.trial_score(),
            Some(CaseTrialScore::NotEvidence)
        );
        assert_eq!(
            SlotScore::Unknown.trial_score(),
            Some(CaseTrialScore::Unknown)
        );
        assert_eq!(
            SlotScore::Scored(7).trial_score(),
            Some(CaseTrialScore::Scored(7))
        );
        assert_eq!(
            serde_json::to_value(SlotScore::Scored(10_000)).unwrap(),
            json!({"kind": "scored", "bp": 10_000})
        );
        assert_eq!(
            serde_json::to_value(SlotScore::Absent).unwrap(),
            json!({"kind": "absent"})
        );
        assert_eq!(
            serde_json::to_value(SlotClass::NotEvidence).unwrap(),
            json!("not_evidence")
        );
        let mut counts = SlotCounts::default();
        for class in [
            SlotClass::Pass,
            SlotClass::Fail,
            SlotClass::Unknown,
            SlotClass::NotEvidence,
            SlotClass::Abandoned,
            SlotClass::Planned,
        ] {
            counts.add(class);
        }
        assert_eq!(
            counts,
            SlotCounts {
                pass: 1,
                fail: 1,
                unknown: 1,
                not_evidence: 1,
                abandoned: 1,
                planned: 1,
            }
        );
    }

    #[test]
    fn the_fixtures_build_one_row_per_attempt_and_one_verdict_per_completion() {
        let rows = Rows::default()
            .add(RUN, at("base", "disk", 0, 1), fail())
            .add(RUN, at("base", "disk", 0, 2), pass())
            .unmetered()
            .add(RUN, at("base", "disk", 1, 1), Outcome::Open);
        assert_eq!(rows.trials.len(), 3);
        assert_eq!(rows.verdicts.len(), 2);
        assert_eq!(
            rows.verdicts[1].trial_id,
            trial_id(RUN, at("base", "disk", 0, 2))
        );
        let usage = &rows.trials[1].completion.as_ref().unwrap().usage;
        assert_eq!((usage.input_tokens, usage.output_tokens), (None, None));
        assert!(rows.trials[2].completion.is_none());
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        assert_eq!(run.origin.case_ids, vec!["disk".to_owned()]);
    }

    use crate::document_config::EvalSplit;
    use crate::eval::report::evidence::CellUsage;
    use crate::eval::report::report_refused;
    use crate::eval::{OutcomeKind, RunHeader, TrialUsage, VerdictRecord};

    fn classes(report: &EvalReport) -> Vec<SlotClass> {
        report.cells[0]
            .slots
            .iter()
            .map(|slot| slot.class)
            .collect()
    }

    fn refusal(result: Result<EvalReport>) -> String {
        let error = result.expect_err("expected a refusal");
        report_refused(&error)
            .unwrap_or_else(|| panic!("expected a ReportRefused, got {error:#}"))
            .0
            .clone()
    }

    #[test]
    fn every_slot_class_is_reached_from_the_documents() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 6);
        let rows = Rows::default()
            .add(RUN, at("base", "disk", 0, 1), pass())
            .add(RUN, at("base", "disk", 1, 1), fail())
            .add(
                RUN,
                at("base", "disk", 2, 1),
                Outcome::Verdict(OutcomeKind::Grader, None),
            )
            .add(
                RUN,
                at("base", "disk", 3, 1),
                Outcome::Verdict(OutcomeKind::Infrastructure, None),
            )
            .add(RUN, at("base", "disk", 4, 1), Outcome::Open);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();

        assert_eq!(report.report_version, REPORT_VERSION);
        assert_eq!(
            classes(&report),
            vec![
                SlotClass::Pass,
                SlotClass::Fail,
                SlotClass::Unknown,
                SlotClass::NotEvidence,
                SlotClass::Abandoned,
                SlotClass::Planned,
            ]
        );
        let cell = &report.cells[0];
        let one_each = SlotCounts {
            pass: 1,
            fail: 1,
            unknown: 1,
            not_evidence: 1,
            abandoned: 1,
            planned: 1,
        };
        assert_eq!(cell.counts, one_each);
        assert_eq!(cell.cases[0].counts, one_each);
        assert_eq!(cell.attempts, 5, "the planned slot has no row");
        // An unknown counts as a failure in a mean; a not-evidence slot and
        // slots with no completed attempt are left out: (10000 + 0 + 0) / 3.
        assert_eq!(cell.cases[0].mean_bp, Some(3_333));
        assert_eq!(cell.headline_bp, Some(3_333));
        assert_eq!(
            cell.slots
                .iter()
                .map(|slot| slot.counted)
                .collect::<Vec<_>>(),
            vec![
                SlotScore::Scored(10_000),
                SlotScore::Scored(0),
                SlotScore::Unknown,
                SlotScore::NotEvidence,
                SlotScore::Absent,
                SlotScore::Absent,
            ]
        );
        let abandoned = cell.slots[4].latest.as_ref().expect("the open row");
        assert!(abandoned.stages.is_empty() && abandoned.evidence_digest.is_none());
        assert!(cell.slots[5].latest.is_none());
    }

    #[test]
    fn a_scored_slot_passes_only_when_every_acceptance_verdict_passes() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), pass());
        let second = VerdictRecord {
            verdict_id: "second".into(),
            check: "second_check".into(),
            kind: OutcomeKind::ModelAcceptance,
            score_bp: Some(0),
            ..rows.verdicts[0].clone()
        };
        rows.verdicts.push(second);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let slot = &report.cells[0].slots[0];
        assert_eq!(
            (slot.class, slot.score_bp),
            (SlotClass::Fail, Some(5_000)),
            "a weighted mean of 5000 is still a failing slot"
        );
    }

    #[test]
    fn a_regrade_changes_the_slots_score_and_is_not_an_attempt() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), fail());
        let before = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let slot = &before.cells[0].slots[0];
        assert_eq!((slot.class, slot.score_bp), (SlotClass::Fail, Some(0)));

        let original = rows.verdicts[0].clone();
        rows.verdicts.push(VerdictRecord {
            verdict_id: "regrade".into(),
            kind: OutcomeKind::Passed,
            score_bp: Some(10_000),
            regrade_of: Some(original.verdict_id.clone()),
            ..original
        });
        let after = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let slot = &after.cells[0].slots[0];
        assert_eq!((slot.class, slot.score_bp), (SlotClass::Pass, Some(10_000)));
        assert_eq!(after.cells[0].attempts, 1);
    }

    #[test]
    fn attempts_and_slots_are_counted_separately() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 2);
        let rows = Rows::default()
            .add(
                RUN,
                at("base", "disk", 0, 1),
                Outcome::Verdict(OutcomeKind::Infrastructure, None),
            )
            .add(RUN, at("base", "disk", 0, 2), pass())
            .add(RUN, at("base", "disk", 1, 1), pass())
            .add(RUN, at("base", "disk", 1, 2), Outcome::Open);
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let cell = &report.cells[0];
        assert_eq!(cell.attempts, 4);
        assert_eq!((cell.counts.pass, cell.counts.abandoned), (1, 1));
        assert_eq!(
            (cell.slots[0].class, cell.slots[0].attempts),
            (SlotClass::Pass, 2),
            "a retry that produced evidence is one slot, not two completions"
        );
        let abandoned = &cell.slots[1];
        assert_eq!(abandoned.class, SlotClass::Abandoned);
        assert_eq!(
            (abandoned.counted, abandoned.score_bp),
            (SlotScore::Scored(10_000), Some(10_000)),
            "the score is the latest completed attempt's, as the optimizer pairs it"
        );
        assert_eq!(
            abandoned.latest.as_ref().map(|latest| latest.attempt),
            Some(2)
        );
        assert_eq!(cell.cases[0].mean_bp, Some(10_000));
    }

    #[test]
    fn usage_sums_the_counted_attempts_and_counts_the_unmetered() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 2);
        let rows = Rows::default()
            .add(RUN, at("base", "disk", 0, 1), pass())
            .add(RUN, at("base", "disk", 1, 1), pass())
            .unmetered();
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let cell = &report.cells[0];
        assert_eq!(
            cell.usage,
            TrialUsage {
                input_tokens: Some(10),
                output_tokens: Some(10),
            }
        );
        assert_eq!(
            cell.cell_usage,
            CellUsage {
                tokens: 20,
                trials: 2,
                missing: 1,
            }
        );
    }

    #[test]
    fn exposure_counts_the_non_invalidated_runs_of_this_definition_and_split() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);
        let header = |split, invalidated| RunHeader {
            definition_id: "report-def".into(),
            comparability_version: 1,
            split,
            invalidated,
        };
        let peers = [
            header(EvalSplit::Validation, false),
            header(EvalSplit::Validation, false),
            header(EvalSplit::Validation, true),
            header(EvalSplit::Train, false),
        ];
        let report = build(&run, &[], &[], &definition, &peers).unwrap();
        assert_eq!(report.exposure, 2);
        assert_eq!(classes(&report), vec![SlotClass::Planned]);
    }

    #[test]
    fn build_refuses_documents_it_cannot_read_against_the_run() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 1);

        let mut edited = definition.clone();
        edited.comparability_version = 2;
        assert!(refusal(build(&run, &[], &[], &edited, &[])).contains("no longer digests"));

        let foreign = Rows::default().add("other", at("base", "disk", 0, 1), pass());
        let reason = refusal(build(&run, &foreign.trials, &[], &definition, &[]));
        assert!(reason.contains("belongs to run other"), "{reason}");

        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), pass());
        rows.verdicts[0].run_id = "other".into();
        let reason = refusal(build(&run, &rows.trials, &rows.verdicts, &definition, &[]));
        assert!(
            reason.starts_with("verdict ") && reason.contains("belongs to run other"),
            "{reason}"
        );

        let ghost = Rows::default().add(RUN, at("ghost", "disk", 0, 1), pass());
        let reason = refusal(build(&run, &ghost.trials, &[], &definition, &[]));
        assert!(reason.contains("did not freeze cell \"ghost\""), "{reason}");

        let elsewhere = Rows::default().add(RUN, at("base", "elsewhere", 0, 1), pass());
        let reason = refusal(build(&run, &elsewhere.trials, &[], &definition, &[]));
        assert!(
            reason.contains("did not freeze case \"elsewhere\""),
            "{reason}"
        );
        assert_eq!(
            trial_id(RUN, at("base", "elsewhere", 0, 1)),
            elsewhere.trials[0].identity.trial_id
        );
    }

    #[test]
    fn a_slot_carries_the_verdicts_that_count_for_it() {
        let definition = definition(&["disk"]);
        let run = record(RUN, &definition, &["base"], 2);
        let mut rows = Rows::default().add(RUN, at("base", "disk", 0, 1), fail());
        let original = rows.verdicts[0].clone();
        rows.verdicts.push(VerdictRecord {
            verdict_id: "regrade".into(),
            kind: OutcomeKind::Passed,
            score_bp: Some(10_000),
            regrade_of: Some(original.verdict_id.clone()),
            ..original
        });
        let report = build(&run, &rows.trials, &rows.verdicts, &definition, &[]).unwrap();
        let verdicts = &report.cells[0].slots[0].verdicts;
        assert_eq!(
            verdicts
                .iter()
                .map(|verdict| (
                    verdict.verdict_id.as_str(),
                    verdict.kind,
                    verdict.reason_code.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![("regrade", OutcomeKind::Passed, Some("fixture"))],
            "the superseded row is not counted"
        );
        assert_eq!(verdicts[0].stage_id, "check");
        assert!(
            report.cells[0].slots[1].verdicts.is_empty(),
            "a planned slot has none"
        );
    }
}
