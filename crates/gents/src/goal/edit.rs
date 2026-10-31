use super::*;

#[derive(Debug, Clone, Copy)]
#[cfg_attr(test, derive(Deserialize))]
#[cfg_attr(test, serde(rename_all = "snake_case"))]
pub(super) enum GoalEditOrigin {
    Agent,
    Client,
}

impl GoalEditOrigin {
    pub(super) fn can_edit_status(self, status: GoalStatus) -> bool {
        status == GoalStatus::Active || matches!(self, Self::Client) && status == GoalStatus::Paused
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct GoalEdit {
    pub objective: Option<String>,
    pub budget: Option<i64>,
}

impl GoalEdit {
    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.objective.is_some() || self.budget.is_some(),
            "provide objective or token_budget to edit the goal"
        );
        anyhow::ensure!(
            self.objective
                .as_deref()
                .is_none_or(|value| !value.trim().is_empty()),
            "goal objective must be non-empty"
        );
        anyhow::ensure!(
            self.budget.is_none_or(|budget| budget > 0),
            "goal token budget must be positive"
        );
        Ok(())
    }
}

pub(crate) async fn edit_active_goal(
    node: &std::sync::Arc<EmbeddedNode>,
    observed: &GoalDocument,
    fields: &GoalEdit,
) -> Result<Option<GoalDocument>> {
    fields.validate()?;
    crate::ConfigAccess::transact_local_write(node, None, "goal.edit_active", |txn| {
        Box::pin(async move { stage_goal_edit(txn, observed, fields, GoalEditOrigin::Agent).await })
    })
    .await
}

pub(super) async fn stage_goal_edit(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    observed: &GoalDocument,
    fields: &GoalEdit,
    origin: GoalEditOrigin,
) -> Result<Option<GoalDocument>> {
    fields.validate()?;
    let Some(current) =
        load_canonical_goal_in_txn(txn, &observed.node_did, &observed.session_id).await?
    else {
        return Ok(None);
    };
    let status = current.parsed_status();
    if !same_goal_assignment(&current, observed)
        || !status.is_some_and(|status| origin.can_edit_status(status))
        || observed.parsed_status() != status
        || current.continuation_sequence() != observed.continuation_sequence()
    {
        return Ok(None);
    }
    let objective = fields.objective.as_deref().map(str::trim);
    if objective.is_none_or(|objective| objective == current.objective)
        && fields
            .budget
            .is_none_or(|budget| current.token_budget == Some(budget))
    {
        return Ok(Some(current));
    }
    set_goal_in_txn(
        txn,
        &observed.node_did,
        &observed.session_id,
        objective,
        None,
        fields.budget.map(Some),
        None,
    )
    .await
    .map(Some)
}

#[cfg(test)]
mod contract_tests;
