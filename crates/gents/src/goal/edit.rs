use super::*;

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct GoalEdit {
    pub objective: Option<String>,
    pub budget: Option<i64>,
}

impl GoalEdit {
    pub(crate) fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.objective.is_some() || self.budget.is_some(),
            "provide objective or token_budget to edit the active goal"
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
        Box::pin(async move { stage_active_goal_edit(txn, observed, fields).await })
    })
    .await
}

pub(super) async fn stage_active_goal_edit(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    observed: &GoalDocument,
    fields: &GoalEdit,
) -> Result<Option<GoalDocument>> {
    fields.validate()?;
    let Some(current) =
        load_canonical_goal_in_txn(txn, &observed.node_did, &observed.session_id).await?
    else {
        return Ok(None);
    };
    if current.doc_id != observed.doc_id
        || current.goal_id != observed.goal_id
        || current.parsed_status() != Some(GoalStatus::Active)
        || observed.parsed_status() != Some(GoalStatus::Active)
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
