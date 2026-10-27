use anyhow::{Context, Result};
use gents_desktop_core::client::{ClientCore, RetainedGoalSubmission};
use gents_protocol::goal_control::GoalControlOutcome;

use crate::error::BridgeError;
use crate::types::{
    GoalControlRequest, GoalControlResult, GoalSubmissionOutcome, GoalSubmissionQuery,
    GoalSubmissionReceipt, GoalSubmissionRecovery,
};

fn check_client(core: &ClientCore, client_did: &str) -> Result<()> {
    anyhow::ensure!(
        client_did == core.node_identity().did(),
        "Goal submission belongs to another native client"
    );
    Ok(())
}

pub(super) fn reference(
    core: &ClientCore,
    retained: &RetainedGoalSubmission,
) -> GoalSubmissionReceipt {
    GoalSubmissionReceipt {
        client_did: core.node_identity().did().to_owned(),
        node_did: retained.node_did.clone(),
        session_id: retained.session_id.clone(),
        reference: retained.reference.clone(),
    }
}

pub(super) fn applied(
    retained: &RetainedGoalSubmission,
    submission: GoalSubmissionReceipt,
) -> Result<GoalControlResult, BridgeError> {
    let receipt = retained.receipt.as_ref().ok_or_else(|| {
        BridgeError::untyped("Goal command is retained; its outcome is not yet confirmed")
    })?;
    let mut result = super::goal::goal_control_result(
        retained.session_id.clone(),
        retained.reference.initial_request_id.is_some(),
        receipt.outcome.clone(),
    )?;
    result.submission_receipt = Some(submission);
    Ok(result)
}

pub async fn recover_goal_submission(
    core: &ClientCore,
    query: GoalSubmissionQuery,
) -> Result<GoalSubmissionRecovery> {
    check_client(core, &query.client_did)?;
    let Some(retained) = core
        .recover_goal_submission(&query.node_did, &query.command_id)
        .await?
    else {
        return Ok(GoalSubmissionRecovery::Unconfirmed {
            node_did: query.node_did,
            command_id: query.command_id,
        });
    };
    let submission = reference(core, &retained);
    let outcome = match retained.receipt.as_ref().map(|receipt| &receipt.outcome) {
        None => None,
        Some(GoalControlOutcome::Applied { .. }) => Some(GoalSubmissionOutcome::Applied {
            result: applied(&retained, submission.clone())?,
        }),
        Some(GoalControlOutcome::Rejected { reason }) => Some(GoalSubmissionOutcome::Rejected {
            reason: reason.clone(),
        }),
    };
    Ok(GoalSubmissionRecovery::Prepared {
        receipt: submission,
        outcome,
        acknowledged: retained.acknowledged,
    })
}

async fn exact(
    core: &ClientCore,
    submission: &GoalSubmissionReceipt,
) -> Result<RetainedGoalSubmission> {
    check_client(core, &submission.client_did)?;
    let retained = core
        .recover_goal_submission(&submission.node_did, &submission.reference.command_id)
        .await?
        .context("Goal caller preparation is unavailable")?;
    anyhow::ensure!(
        retained.session_id == submission.session_id && retained.reference == submission.reference,
        "Goal submission receipt crossed its retained destination or signed command"
    );
    Ok(retained)
}

pub async fn resume_goal_submission(
    core: &ClientCore,
    request: GoalControlRequest,
    submission: GoalSubmissionReceipt,
) -> Result<GoalControlResult, BridgeError> {
    super::goal::control_goal_with_reference(core, request, Some(submission)).await
}

pub async fn acknowledge_goal_submission(
    core: &ClientCore,
    submission: GoalSubmissionReceipt,
) -> Result<()> {
    exact(core, &submission).await?;
    core.acknowledge_goal_submission(&submission.node_did, &submission.reference)
        .await
}

#[cfg(test)]
#[path = "goal_submission/tests.rs"]
mod tests;
