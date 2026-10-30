use anyhow::{ensure, Context, Result};
use gents::config_client::config_projection;
use gents::documents::{Schedule, Task};
use gents::graphql::escape_graphql_string;
use gents::Collection;
use gents_desktop_core::client::ClientCore;

use crate::types::{ConfiguredActionKind, ConfiguredActionView};

pub(super) async fn catalog(
    core: &ClientCore,
    node_did: &str,
) -> Result<Vec<ConfiguredActionView>> {
    let (task_fields, _) = config_projection(Collection::Task, None)?;
    let (schedule_fields, _) = config_projection(Collection::Schedule, None)?;
    let owner = escape_graphql_string(node_did);
    let query = format!(
        r#"{{
            Task(filter: {{node_did: {{_eq: "{owner}"}}}}) {{ {} }}
            Schedule(filter: {{node_did: {{_eq: "{owner}"}}}}) {{ {} }}
        }}"#,
        task_fields.join(" "),
        schedule_fields.join(" "),
    );
    let access = core
        .operator_graphql(node_did)
        .map(gents::ConfigAccess::Graphql)
        .context("Connect this local runtime before choosing its configured actions.")?;
    let response = access.execute(&query).await?;
    let tasks: Vec<Task> = serde_json::from_value(
        response
            .pointer("/data/Task")
            .context("The action catalog returned no tasks.")?
            .clone(),
    )?;
    let schedules: Vec<Schedule> = serde_json::from_value(
        response
            .pointer("/data/Schedule")
            .context("The action catalog returned no schedules.")?
            .clone(),
    )?;
    let mut identities = rapidhash::RapidHashSet::default();
    let mut actions = Vec::with_capacity(tasks.len() + schedules.len());
    for task in tasks {
        ensure!(
            task.node_did == node_did && !task.task_id.trim().is_empty(),
            "Task catalog crossed its selected runtime scope."
        );
        ensure!(
            identities.insert(("Task", task.task_id.clone())),
            "The action catalog contains duplicate task identities."
        );
        if task.enabled {
            actions.push(ConfiguredActionView {
                kind: ConfiguredActionKind::Task,
                label: label(task.display_name, &task.task_id),
                id: task.task_id,
                description: task.description,
            });
        }
    }
    for schedule in schedules {
        ensure!(
            schedule.node_did == node_did && !schedule.schedule_id.trim().is_empty(),
            "Schedule catalog crossed its selected runtime scope."
        );
        ensure!(
            identities.insert(("Schedule", schedule.schedule_id.clone())),
            "The action catalog contains duplicate schedule identities."
        );
        actions.push(ConfiguredActionView {
            kind: ConfiguredActionKind::Schedule,
            label: label(schedule.display_name, &schedule.schedule_id),
            id: schedule.schedule_id,
            description: None,
        });
    }
    actions.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(actions)
}

fn label(name: Option<String>, id: &str) -> String {
    name.filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| id.to_owned())
}
