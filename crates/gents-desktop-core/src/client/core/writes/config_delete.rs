use std::sync::RwLock as StdRwLock;

use anyhow::{bail, Result};

use super::ClientCore;
use crate::client::mutations;
use crate::client::observe::ObservedStore;
use crate::client::store::{ClientStore, ClientStoreRows};

#[cfg(test)]
mod tests;

impl ClientCore {
    pub async fn delete_skill(&self, skill_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_skill_on(&access, source_node_did, skill_id).await?;
            if deleted == 0 {
                bail!("no Skill document with skill_id {skill_id:?} for {source_node_did}");
            }
            Ok(())
        }
        .await;

        match result {
            Ok(()) => {
                let refresh_result = self.refresh_config_source(source_node_did).await;
                complete_confirmed_delete(
                    self.store.as_ref(),
                    &self.last_mutation_error,
                    refresh_result,
                    "delete skill",
                    "config_skill_delete",
                    skill_id,
                    |rows| prune_deleted_skill_rows(rows, source_node_did, skill_id),
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("delete skill", error)),
        }
    }

    pub async fn delete_task(&self, task_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_task_on(&access, source_node_did, task_id).await?;
            if deleted == 0 {
                bail!("no Task document with task_id {task_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete task",
            "config_task_delete",
            task_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.tasks,
                    &mut rows.task_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.task_id == task_id,
                );
            },
        )
        .await
    }

    pub async fn delete_schedule(&self, schedule_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_schedule_on(&access, source_node_did, schedule_id).await?;
            if deleted == 0 {
                bail!("no Schedule document with schedule_id {schedule_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete schedule",
            "config_schedule_delete",
            schedule_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.schedules,
                    &mut rows.schedule_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.schedule_id == schedule_id,
                );
            },
        )
        .await
    }

    pub async fn delete_trigger(&self, trigger_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_trigger_on(&access, source_node_did, trigger_id).await?;
            if deleted == 0 {
                bail!("no Trigger document with trigger_id {trigger_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete trigger",
            "config_trigger_delete",
            trigger_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.triggers,
                    &mut rows.trigger_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.trigger_id == trigger_id,
                );
            },
        )
        .await
    }

    pub async fn delete_inference_backend(
        &self,
        backend_id: &str,
        source_node_did: &str,
    ) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_inference_backend_on(&access, source_node_did, backend_id)
                    .await?;
            if deleted == 0 {
                bail!("no InferenceBackend document with backend_id {backend_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete inference backend",
            "config_backend_delete",
            backend_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.inference_backends,
                    &mut rows.inference_backend_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.backend_id == backend_id,
                );
            },
        )
        .await
    }

    pub async fn delete_inference_profile(
        &self,
        profile_id: &str,
        source_node_did: &str,
    ) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_inference_profile_on(&access, source_node_did, profile_id)
                    .await?;
            if deleted == 0 {
                bail!("no InferenceProfile document with profile_id {profile_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete inference profile",
            "config_profile_delete",
            profile_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.inference_profiles,
                    &mut rows.inference_profile_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.profile_id == profile_id,
                );
            },
        )
        .await
    }

    pub async fn delete_tools(&self, tools_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_tools_on(&access, source_node_did, tools_id).await?;
            if deleted == 0 {
                bail!("no Tools document with tools_id {tools_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete tools",
            "config_tools_delete",
            tools_id,
            source_node_did,
            |rows| {
                rows.tools
                    .retain(|row| row.tools_id != tools_id || row.node_did != source_node_did);
            },
        )
        .await
    }

    pub async fn delete_tool_service(&self, service_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_tool_service_registry_on(&access, source_node_did, service_id)
                    .await?;
            if deleted == 0 {
                bail!("no ToolServiceRegistry document with service_id {service_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete tool service",
            "config_tool_service_delete",
            service_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.tool_service_registries,
                    &mut rows.tool_service_registry_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.service_id == service_id,
                );
            },
        )
        .await
    }

    pub async fn delete_agent(&self, agent_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted = mutations::delete_agent_on(&access, source_node_did, agent_id).await?;
            if deleted == 0 {
                bail!("no Agent document with agent_id {agent_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete agent",
            "config_agent_delete",
            agent_id,
            source_node_did,
            |rows| {
                rows.agents
                    .retain(|row| row.agent_id != agent_id || row.node_did != source_node_did);
            },
        )
        .await
    }

    pub async fn delete_context(&self, context_id: &str, source_node_did: &str) -> Result<()> {
        let access = self.operator_access(source_node_did)?;
        let result = async {
            let deleted =
                mutations::delete_agent_context_on(&access, source_node_did, context_id).await?;
            if deleted == 0 {
                bail!("no AgentContext document with context_id {context_id:?}");
            }
            Ok(())
        }
        .await;
        self.finish_automation_delete(
            result,
            "delete context",
            "config_context_delete",
            context_id,
            source_node_did,
            |rows| {
                retain_sourced_rows(
                    &mut rows.contexts,
                    &mut rows.context_source_node_dids,
                    source_node_did,
                    false,
                    |row| row.context_id == context_id,
                );
            },
        )
        .await
    }

    async fn finish_automation_delete(
        &self,
        result: Result<()>,
        action_label: &str,
        action: &str,
        row_id: &str,
        source_node_did: &str,
        prune: impl FnOnce(&mut ClientStoreRows),
    ) -> Result<()> {
        match result {
            Ok(()) => {
                let refresh_result = self.refresh_config_source(source_node_did).await;
                complete_confirmed_delete(
                    self.store.as_ref(),
                    &self.last_mutation_error,
                    refresh_result,
                    action_label,
                    action,
                    row_id,
                    prune,
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error(action_label, error)),
        }
    }
}

fn row_matches_source(
    sources: &[Option<String>],
    index: usize,
    source_node_did: &str,
    is_remote_source: bool,
) -> bool {
    match sources.get(index).and_then(|source| source.as_deref()) {
        Some(source) => source == source_node_did,
        None => !is_remote_source,
    }
}

fn retain_sourced_rows<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    source_node_did: &str,
    is_remote_source: bool,
    should_delete: impl Fn(&T) -> bool,
) {
    let previous_rows = std::mem::take(rows);
    let previous_sources = std::mem::take(sources);

    for (index, row) in previous_rows.into_iter().enumerate() {
        if should_delete(&row)
            && row_matches_source(&previous_sources, index, source_node_did, is_remote_source)
        {
            continue;
        }
        rows.push(row);
        sources.push(previous_sources.get(index).cloned().unwrap_or_default());
    }
}

fn retain_rows_with_sources<T>(
    rows: &mut Vec<T>,
    sources: &mut Vec<Option<String>>,
    mut keep: impl FnMut(&T) -> bool,
) {
    let mut kept_rows = Vec::with_capacity(rows.len());
    let mut kept_sources = Vec::with_capacity(rows.len());

    for (index, row) in rows.drain(..).enumerate() {
        if keep(&row) {
            kept_rows.push(row);
            kept_sources.push(sources.get(index).cloned().unwrap_or_default());
        }
    }

    *rows = kept_rows;
    *sources = kept_sources;
}

fn prune_deleted_skill_rows(rows: &mut ClientStoreRows, node_did: &str, skill_id: &str) {
    retain_rows_with_sources(&mut rows.skills, &mut rows.skill_source_node_dids, |row| {
        !(row.skill_id == skill_id && row.node_did == node_did)
    });
}

fn complete_confirmed_delete(
    store: &ObservedStore,
    last_mutation_error: &StdRwLock<Option<String>>,
    refresh_result: Result<u64>,
    action_label: &str,
    action: &str,
    row_id: &str,
    prune: impl FnOnce(&mut ClientStoreRows),
) {
    match refresh_result {
        Ok(_) => {
            *last_mutation_error
                .write()
                .expect("mutation error lock poisoned") = None;
        }
        Err(error) => {
            let warning = format!(
                "{action_label} succeeded, but refreshing the source snapshot failed: {error}"
            );
            *last_mutation_error
                .write()
                .expect("mutation error lock poisoned") = Some(warning);
            tracing::warn!(
                target: "gents_desktop_core::writes",
                action = %action,
                row_id = %row_id,
                error = %error,
                "desktop write saved, but refreshing the source snapshot failed"
            );
        }
    }

    let mut rows = store.snapshot().to_rows();
    prune(&mut rows);
    store.replace_snapshot(ClientStore::from_rows(rows));

    tracing::info!(
        target: "gents_desktop_core::writes",
        action = %action,
        row_id = %row_id,
        "desktop write saved"
    );
}
