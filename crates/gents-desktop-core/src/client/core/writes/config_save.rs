use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;

use super::ClientCore;
use crate::client::mutations;

impl ClientCore {
    pub async fn save_agent(&self, row: &gents::AgentDocument) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        let result = mutations::upsert_agent_on(&access, row).await;
        match result {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "agent",
                    row_id = %row.agent_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save agent", error)),
        }
    }

    pub async fn patch_config_components(
        &self,
        node_did: &str,
        patches: &[(
            gents::config_client::patch::SelfConfigTarget,
            String,
            gents::config_client::patch::SelfConfigPatch,
        )],
    ) -> Result<()> {
        let access = self.operator_access(node_did)?;
        match mutations::patch_config_components_on(&access, node_did, patches).await {
            Ok(()) => {
                self.refresh_config_source(node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("patch config components", error)),
        }
    }

    pub async fn read_config_editor(
        &self,
        node_did: &str,
        target: gents::config_client::patch::SelfConfigTarget,
        id: &str,
    ) -> Result<mutations::ConfigEditorDocument> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before editing its configuration.")?;
        mutations::read_config_editor_on(&access, node_did, target, id).await
    }

    pub async fn read_backend_authentication(
        &self,
        node_did: &str,
        id: &str,
    ) -> Result<(mutations::ConfigEditorRevision, gents::InferenceBackend)> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before changing authentication.")?;
        mutations::read_backend_authentication_on(&access, node_did, id).await
    }

    pub async fn save_backend_authentication(
        &self,
        node_did: &str,
        id: &str,
        revision: &mutations::ConfigEditorRevision,
        auth: &gents::document_config::BackendAuth,
    ) -> Result<()> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before changing authentication.")?;
        mutations::save_backend_authentication_on(&access, node_did, id, revision, auth).await?;
        if self.refresh_config_source(node_did).await.is_err() {
            tracing::warn!("Authentication saved; client observation will resynchronize");
            self.store.request_resync(Some(node_did));
        }
        Ok(())
    }

    pub async fn save_config_editor(
        &self,
        node_did: &str,
        target: gents::config_client::patch::SelfConfigTarget,
        id: &str,
        revision: &mutations::ConfigEditorRevision,
        content: &str,
    ) -> Result<Option<mutations::ConfigEditorDocument>> {
        let access = self
            .operator_graphql(node_did)
            .map(ConfigAccess::Graphql)
            .context("Connect this local runtime before editing its configuration.")?;
        let saved =
            mutations::save_config_editor_on(&access, node_did, target, id, revision, content)
                .await?;
        if self.refresh_config_source(node_did).await.is_err() {
            tracing::warn!("Configuration saved; client observation will resynchronize");
            self.store.request_resync(Some(node_did));
        }
        Ok(saved)
    }

    pub async fn apply_config_components(
        &self,
        document: &gents::document_config::PackConfig,
    ) -> Result<()> {
        let access = self.operator_access(&document.node.node_did)?;
        match mutations::apply_config_components_on(&access, document).await {
            Ok(()) => {
                self.refresh_config_source(&document.node.node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("apply config components", error)),
        }
    }

    pub async fn save_node(&self, row: &gents::document_config::Node) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        let result = mutations::upsert_node_on(&access, row).await;
        match result {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "node",
                    row_id = %row.node_did,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save node", error)),
        }
    }

    pub async fn set_default_agent(&self, node_did: &str, agent_id: &str) -> Result<()> {
        let access = self.operator_access(node_did)?;
        match mutations::set_default_agent_on(&access, node_did, agent_id).await {
            Ok(()) => {
                self.refresh_config_source(node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("set default agent", error)),
        }
    }

    pub async fn save_backend(&self, row: &gents::InferenceBackend) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_inference_backend_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "backend",
                    row_id = %row.backend_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save backend", error)),
        }
    }

    pub async fn save_tools(&self, row: &gents::Tools) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        let result = mutations::upsert_tools_on(&access, row).await;
        match result {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "tools",
                    row_id = %row.tools_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tools", error)),
        }
    }

    pub async fn save_tool_service_registry(
        &self,
        row: &gents::document_config::ToolServiceRegistry,
    ) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_tool_service_registry_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "tool_service_registry",
                    row_id = %row.service_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save tool service registry", error)),
        }
    }

    pub async fn select_agent_model(
        &self,
        node_did: &str,
        agent_id: &str,
        backend_id: &str,
        model_name: &str,
        reasoning_effort: Option<gents::config::ReasoningEffort>,
        service_tier: Option<gents::config::ServiceTier>,
    ) -> Result<gents::InferenceProfile> {
        let access = self.operator_access(node_did)?;
        let saved = mutations::select_agent_model_on(
            &access,
            node_did,
            agent_id,
            backend_id,
            model_name,
            reasoning_effort,
            service_tier,
        )
        .await?;
        if self.refresh_config_source(node_did).await.is_err() {
            tracing::warn!("Model saved; client observation will resynchronize");
            self.store.request_resync(Some(node_did));
        }
        Ok(saved)
    }

    pub async fn save_inference_profile(&self, row: &gents::InferenceProfile) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_inference_profile_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "inference_profile",
                    row_id = %row.profile_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save inference profile", error)),
        }
    }

    /// A disconnected runtime must not stage its reviewer in the client replica.
    /// Preparing this Task also requires an already available signer; it never
    /// unlocks a runtime identity as a side effect of selecting permissions.
    pub async fn prepare_access_review_task(
        &self,
        node: &str,
        agent: &str,
        preferred: Option<&str>,
    ) -> Result<gents::plugin::approval::workspace::ReviewTaskSelection> {
        let record = self
            .sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node)
            .context("Connect this runtime before setting up access review.")?;
        anyhow::ensure!(gents::identity::can_mint_defradb_bearer(node),
            "Open Runtime Settings to connect this runtime's configuration before choosing Approve for me.");
        let access = crate::local_runtime::operator_endpoint(&record)
            .map(ConfigAccess::Graphql)
            .context("Access review setup requires this runtime's operator endpoint.")?;
        mutations::prepare_access_review_task_on(&access, node, agent, preferred).await
    }

    pub async fn save_task(&self, row: &gents::document_config::Task) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_task_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "task",
                    row_id = %row.task_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save task", error)),
        }
    }

    pub async fn save_skill(&self, row: &gents::document_config::SkillDocument) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_skill_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "skill",
                    row_id = %row.skill_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save skill", error)),
        }
    }

    pub async fn save_schedule(&self, row: &gents::document_config::Schedule) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_schedule_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "schedule",
                    row_id = %row.schedule_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save schedule", error)),
        }
    }

    pub async fn save_trigger(&self, row: &gents::document_config::Trigger) -> Result<()> {
        let access = self.operator_access(&row.node_did)?;
        match mutations::upsert_trigger_on(&access, row).await {
            Ok(()) => {
                self.refresh_config_source(&row.node_did).await?;
                self.clear_mutation_error();
                tracing::info!(
                    target: "gents_desktop_core::writes",
                    doc_type = "trigger",
                    row_id = %row.trigger_id,
                    "desktop write saved"
                );
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save trigger", error)),
        }
    }

    pub async fn save_event_source(
        &self,
        document: &gents::document_config::EventSource,
    ) -> Result<()> {
        let access = self.operator_access(&document.node_did)?;
        match mutations::upsert_event_source_on(&access, document).await {
            Ok(()) => {
                self.refresh_config_source(&document.node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("save event source", error)),
        }
    }

    pub async fn delete_event_source(&self, event_source_id: &str, node_did: &str) -> Result<()> {
        let access = self.operator_access(node_did)?;
        match mutations::delete_event_source_on(&access, node_did, event_source_id).await {
            Ok(0) => Err(self.record_mutation_error(
                "delete event source",
                anyhow::anyhow!("no EventSource document with event_source_id {event_source_id:?}"),
            )),
            Ok(_) => {
                self.refresh_config_source(node_did).await?;
                self.clear_mutation_error();
                Ok(())
            }
            Err(error) => Err(self.record_mutation_error("delete event source", error)),
        }
    }
}
