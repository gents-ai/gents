//! A command Task runs host code on the node each time its Trigger fires, so a
//! pack that brings one, or changes one's command, installs only with the
//! operator's `--grant-authority`.

use anyhow::Result;

use gents_documents::{PackConfig, Task};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

/// The command Tasks among `configs` whose command the node does not already
/// run under the same `task_id`: each needs the operator's grant.
pub async fn unapproved_command_tasks(
    access: &ConfigAccess,
    owner: &str,
    configs: &[&PackConfig],
) -> Result<Vec<String>> {
    let shipped = configs
        .iter()
        .flat_map(|config| &config.tasks)
        .filter(|task| task.command.is_some())
        .cloned()
        .collect::<Vec<_>>();
    if shipped.is_empty() {
        return Ok(Vec::new());
    }
    access
        .transact_readonly("pack.command_tasks", |txn| {
            let shipped = &shipped;
            Box::pin(async move {
                let mut unapproved = Vec::new();
                for task in shipped {
                    let stored = gents_documents::read_desired_state_record_in_txn(
                        txn,
                        Collection::Task,
                        owner,
                        &task.task_id,
                    )
                    .await?
                    .map(|(_, value)| serde_json::from_value::<Task>(value))
                    .transpose()?;
                    if stored.and_then(|stored| stored.command) != task.command {
                        unapproved.push(task.task_id.clone());
                    }
                }
                Ok::<_, anyhow::Error>(unapproved)
            })
        })
        .await
}

/// The refusal an install without the grant reports, writing nothing.
pub fn refusal(coordinate: &str, tasks: &[String]) -> anyhow::Error {
    anyhow::anyhow!(
        "{coordinate} ships command Tasks ({}) that run host commands on this node each time their Triggers fire, with no agent in between. Nothing was installed. Install it with --grant-authority to allow that",
        tasks.join(", ")
    )
}
