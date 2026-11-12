use super::*;
use gents_session::session::output_render::ordered;

impl ConfigCommandTool {
    pub(super) async fn automation_inventory(
        &self,
        target: SelfConfigTarget,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<String> {
        let rows = self.automation_rows(target, None).await?;
        let total = rows.len();
        let mut selected = rows
            .iter()
            .filter(|row| {
                row.get(target.unique_field())
                    .and_then(Value::as_str)
                    .is_some_and(|id| cursor.is_none_or(|cursor| id > cursor))
            })
            .take(limit + 1)
            .cloned()
            .collect::<Vec<_>>();
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        let next_cursor = truncated
            .then(|| {
                selected
                    .last()?
                    .get(target.unique_field())?
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .flatten();
        ordered! {
            "resource": target.collection_name(),
            "items": selected,
            "page": json!({
                "limit": limit,
                "total": total,
                "returned": selected.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            }),
            "note": "Inventory is read-only and scoped to the calling agent.",
        }
        .pretty()
    }

    pub(super) async fn automation_exact_read(
        &self,
        target: SelfConfigTarget,
        id: &str,
    ) -> Result<String> {
        let rows = self
            .automation_rows(target, (target == SelfConfigTarget::Task).then_some(id))
            .await?;
        let document = rows
            .into_iter()
            .find(|row| row.get(target.unique_field()).and_then(Value::as_str) == Some(id))
            .ok_or(super::super::ops::MissingConfigDocument {
                target,
                id: id.to_owned(),
            })?;
        ordered! {"resource": target.collection_name(), "document": document}.pretty()
    }

    async fn automation_rows(
        &self,
        target: SelfConfigTarget,
        task_id: Option<&str>,
    ) -> Result<Vec<Value>> {
        let access = self.core.access()?;
        let inventory = access
            .transact_readonly("self_config.automation_inventory", |txn| {
                Box::pin(async move {
                    gents_documents::views::tasks::inventory(txn, &self.node_did, true, task_id)
                        .await
                        .map_err(anyhow::Error::from)
                })
            })
            .await?;
        match target {
            SelfConfigTarget::Task => inventory
                .tasks
                .into_iter()
                .map(|row| {
                    let mut value = serde_json::to_value(&row)?;
                    value["enabled"] = json!(row.task.enabled);
                    Ok(value)
                })
                .collect(),
            SelfConfigTarget::Trigger => inventory
                .triggers
                .into_iter()
                .map(|row| {
                    let mut value = serde_json::to_value(&row)?;
                    value["enabled"] = json!(row.trigger.enabled);
                    Ok(value)
                })
                .collect(),
            SelfConfigTarget::Schedule => inventory
                .schedules
                .into_iter()
                .map(|row| serde_json::to_value(row).map_err(Into::into))
                .collect(),
            SelfConfigTarget::EventSource => inventory
                .event_sources
                .into_iter()
                .map(|row| serde_json::to_value(row).map_err(Into::into))
                .collect(),
            _ => unreachable!("automation inventory target"),
        }
    }
}
