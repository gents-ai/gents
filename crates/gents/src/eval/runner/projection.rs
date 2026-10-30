use sha2::{Digest, Sha256};

use crate::document_config::{EvalCase, EvalSplit};
use crate::eval::runner::{TrialEvidence, VerdictRow};
use crate::eval::{OutcomeKind, StageCompletion, TrialCompletion, VerdictDraft};

pub(super) fn verdict_draft(
    run_id: &str,
    trial_id: &str,
    index: usize,
    row: &VerdictRow,
    split: EvalSplit,
) -> VerdictDraft {
    VerdictDraft {
        verdict_id: verdict_id(trial_id, &row.stage_id, &row.check, index),
        run_id: run_id.to_owned(),
        trial_id: trial_id.to_owned(),
        stage_id: row.stage_id.clone(),
        check: row.check.clone(),
        check_version: row.check_version.clone(),
        tier: row.tier,
        kind: row.kind,
        provider_reason: row.provider_reason,
        score_bp: row.score_bp,
        weight: row.weight,
        raw: row.raw.clone(),
        feedback: (split == EvalSplit::Train)
            .then(|| row.feedback.clone())
            .flatten(),
        regrade_of: None,
    }
}

fn verdict_id(trial_id: &str, stage_id: &str, check: &str, index: usize) -> String {
    let material = format!(
        "{trial_id}
{stage_id}
{check}
{index}"
    );
    format!("{:x}", Sha256::digest(material.as_bytes()))
}

pub(super) fn completion(case: &EvalCase, evidence: &TrialEvidence) -> TrialCompletion {
    let mut stages: Vec<StageCompletion> = evidence
        .stages
        .iter()
        .map(|stage| StageCompletion {
            stage_id: stage.stage_id.clone(),
            request_id: stage.request_id.clone(),
            terminal_state: stage.terminal_state,
            failure_kind: stage.failure_kind,
            provider_reason: stage.provider_reason,
        })
        .collect();
    for stage in &case.stages {
        if !evidence
            .stages
            .iter()
            .any(|observed| observed.stage_id == stage.stage_id)
        {
            stages.push(StageCompletion {
                stage_id: stage.stage_id.clone(),
                request_id: None,
                terminal_state: None,
                failure_kind: Some(OutcomeKind::SkippedPrerequisite),
                provider_reason: None,
            });
        }
    }
    TrialCompletion {
        ended_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        stages,
        usage: evidence.usage.clone(),
        anchor: evidence.anchor.clone(),
        evidence_digest: Some(evidence.evidence_digest.clone()),
    }
}
