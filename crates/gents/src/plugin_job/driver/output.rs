use gents_protocol::job::{DataWrite, JobProgress, JobScratch, JobStepOutput};

use super::settle::Settlement;

pub(in crate::plugin_job) fn settlement_of(output: &JobStepOutput) -> Option<Settlement> {
    match output {
        JobStepOutput::Result { result, .. } => Some(Settlement::Result(result.clone())),
        JobStepOutput::Fail { fail, .. } => Some(Settlement::Fail(fail.clone())),
        JobStepOutput::Suspend { suspend, .. } => Some(Settlement::Suspend(suspend.clone())),
        JobStepOutput::Effects { .. } => None,
    }
}

pub(in crate::plugin_job) fn writes_of(output: &JobStepOutput) -> &[DataWrite] {
    match output {
        JobStepOutput::Result { writes, .. }
        | JobStepOutput::Fail { writes, .. }
        | JobStepOutput::Suspend { writes, .. } => writes,
        JobStepOutput::Effects { effects } => &effects.writes,
    }
}

pub(in crate::plugin_job) fn scratch_of(output: &JobStepOutput) -> &[JobScratch] {
    match output {
        JobStepOutput::Result { scratch, .. } | JobStepOutput::Suspend { scratch, .. } => scratch,
        JobStepOutput::Effects { effects } => &effects.scratch,
        JobStepOutput::Fail { .. } => &[],
    }
}

/// The guest state a step carries for its next step: on `effects`, or on a
/// `suspend` so a resume starts from it. Terminal results carry none.
pub(in crate::plugin_job) fn guest_state_of(output: &JobStepOutput) -> Option<&serde_json::Value> {
    match output {
        JobStepOutput::Effects { effects } => effects.state.as_ref(),
        JobStepOutput::Suspend { state, .. } => state.as_ref(),
        JobStepOutput::Result { .. } | JobStepOutput::Fail { .. } => None,
    }
}

pub(in crate::plugin_job) fn progress_of(output: &JobStepOutput) -> Option<&JobProgress> {
    match output {
        JobStepOutput::Result { progress, .. }
        | JobStepOutput::Fail { progress, .. }
        | JobStepOutput::Suspend { progress, .. } => progress.as_ref(),
        JobStepOutput::Effects { effects } => effects.progress.as_ref(),
    }
}
