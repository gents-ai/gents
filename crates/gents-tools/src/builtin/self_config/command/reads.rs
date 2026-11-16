use super::*;
use gents_session::session::output_render::ordered;

impl ConfigCommandTool {
    pub(super) async fn bound_read(
        &self,
        core: &SelfConfigCore,
        target: SelfConfigTarget,
    ) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.preview)
            .await?;
        let value = match target {
            SelfConfigTarget::Tools => effective.pointer("/documents/Tools"),
            SelfConfigTarget::InferenceProfile => effective.get("inference_profile"),
            SelfConfigTarget::InferenceBackend => {
                effective.pointer("/documents/InferenceBackendMetadata")
            }
            SelfConfigTarget::InferenceSampling => {
                effective.pointer("/documents/InferenceSampling")
            }
            SelfConfigTarget::InferenceExecution => {
                effective.pointer("/documents/InferenceExecution")
            }
            SelfConfigTarget::InferenceRetryPolicy => {
                effective.pointer("/documents/InferenceRetryPolicy")
            }
            SelfConfigTarget::Compaction => effective.pointer("/documents/Compaction"),
            _ => None,
        }
        .with_context(|| {
            format!(
                "selected agent does not reference a {}; inspect config agent get {} to see its exact reference chain",
                target.collection_name(),
                core.agent_did()
            )
        })?;
        if target == SelfConfigTarget::InferenceBackend {
            let observation = self.backend_document_observation(value).await?;
            return ordered! {"resource": target.collection_name(), "document": value, "observation": observation}.pretty();
        }
        if target == SelfConfigTarget::InferenceExecution {
            return ordered! {"resource": target.collection_name(), "effective": super::read::execution_settings(Some(value))?, "document": value}.pretty();
        }
        ordered! {"resource": target.collection_name(), "document": value}.pretty()
    }

    pub(super) async fn backend_observation_view(
        &self,
        backend_id: &str,
        provider_kind: gents_inference::backends::provider::BackendProviderKind,
    ) -> Result<Value> {
        let rows = gents_inference::views::backends::read_inventory(
            &self.core.access()?,
            &gents_inference::views::backends::BackendList::new(&self.node_did)
                .backend_id(backend_id),
        )
        .await?;
        let observation = rows.into_iter().next().and_then(|row| row.observation);
        gents_inference::backends::registry::scoped_observation_view(
            observation.as_ref(),
            &self.node_did,
            provider_kind,
        )
    }

    async fn backend_document_observation(&self, document: &Value) -> Result<Value> {
        let backend_id = document
            .get("backend_id")
            .and_then(Value::as_str)
            .context("backend document has no backend_id")?;
        let provider_kind = serde_json::from_value(
            document
                .get("provider_kind")
                .cloned()
                .context("backend document has no provider_kind")?,
        )
        .context("decoding backend provider_kind")?;
        self.backend_observation_view(backend_id, provider_kind)
            .await
    }

    pub(super) async fn exact_read(&self, target: SelfConfigTarget, id: &str) -> Result<String> {
        if target == SelfConfigTarget::ToolServiceRegistry {
            return self.mcp_exact_read(id).await;
        }
        if matches!(
            target,
            SelfConfigTarget::Task
                | SelfConfigTarget::Trigger
                | SelfConfigTarget::Schedule
                | SelfConfigTarget::EventSource
        ) {
            return self.automation_exact_read(target, id).await;
        }
        if target == SelfConfigTarget::InferenceBackend {
            let rows = gents_inference::views::backends::read_inventory(
                &self.core.access()?,
                &gents_inference::views::backends::BackendList::new(&self.node_did).backend_id(id),
            )
            .await?;
            let row = rows
                .into_iter()
                .next()
                .ok_or(super::ops::MissingConfigDocument {
                    target,
                    id: id.to_owned(),
                })?;
            let mut document = super::inference_inventory::backend_value(row)?;
            let object = document
                .as_object_mut()
                .context("backend document must be an object")?;
            let mut observation = serde_json::Map::new();
            for field in ["catalog", "probe_status", "last_probe"] {
                observation.insert(field.into(), object.remove(field).unwrap_or(Value::Null));
            }
            object.insert(
                "auth".into(),
                json!({"redacted": true, "owner": "operator credential/login flow"}),
            );
            return ordered! {"resource": target.collection_name(), "document": document, "observation": observation}.pretty();
        }
        let document = gents_store::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.command_read",
            |txn| {
                Box::pin(async move {
                    macro_rules! read {
                        ($view:expr, $field:ident) => {
                            txn.read_view(&$view)
                                .await?
                                .into_iter()
                                .next()
                                .map(|row| serde_json::to_value(row.$field))
                                .transpose()
                                .map_err(anyhow::Error::from)
                        };
                    }
                    use gents_documents::views::config;
                    use gents_inference::views::{
                        profiles::ProfileList,
                        settings::{ExecutionList, RetryPolicyList, SamplingList},
                    };
                    match target {
                        SelfConfigTarget::Agent => read!(
                            gents_documents::views::agents::AgentList::new(&self.node_did)
                                .agent_did(id),
                            agent
                        ),
                        SelfConfigTarget::AgentContext => read!(
                            config::AgentContextList::new(&self.node_did).id(id),
                            document
                        ),
                        SelfConfigTarget::Compaction => read!(
                            config::CompactionConfigList::new(&self.node_did).id(id),
                            document
                        ),
                        SelfConfigTarget::Tools => {
                            read!(config::ToolsList::new(&self.node_did).id(id), document)
                        }
                        SelfConfigTarget::AgentTarget => read!(
                            config::AgentTargetList::new(&self.node_did).id(id),
                            document
                        ),
                        SelfConfigTarget::DatastoreToolSurface => read!(
                            config::DatastoreToolSurfaceList::new(&self.node_did).id(id),
                            document
                        ),
                        SelfConfigTarget::Skill => {
                            read!(config::SkillList::new(&self.node_did).id(id), document)
                        }
                        SelfConfigTarget::Page => {
                            read!(config::PageList::new(&self.node_did).id(id), document)
                        }
                        SelfConfigTarget::PluginSource => read!(
                            config::PluginSourceList::new(&self.node_did).id(id),
                            document
                        ),
                        SelfConfigTarget::InferenceProfile => {
                            read!(ProfileList::new(&self.node_did).profile_id(id), profile)
                        }
                        SelfConfigTarget::InferenceSampling => {
                            read!(SamplingList::new(&self.node_did).id(id), settings)
                        }
                        SelfConfigTarget::InferenceExecution => {
                            read!(ExecutionList::new(&self.node_did).id(id), settings)
                        }
                        SelfConfigTarget::InferenceRetryPolicy => {
                            read!(RetryPolicyList::new(&self.node_did).id(id), settings)
                        }
                        _ => unreachable!(
                            "specialized exact read dispatched before configuration views"
                        ),
                    }
                })
            },
        )
        .await?
        .ok_or(super::ops::MissingConfigDocument {
            target,
            id: id.to_owned(),
        })?;
        if target == SelfConfigTarget::AgentTarget {
            return ordered! {"resource": target.collection_name(), "connection": ops::target_destination(document.as_object().context("agent target must be an object")?, &self.node_did), "document": document}.pretty();
        }
        if target == SelfConfigTarget::InferenceExecution {
            return ordered! {"resource": target.collection_name(), "effective": super::read::execution_settings(Some(&document))?, "document": document}.pretty();
        }
        ordered! {"resource": target.collection_name(), "document": document}.pretty()
    }

    pub(super) async fn automation_read(
        &self,
        core: &SelfConfigCore,
        target: SelfConfigTarget,
        id: &str,
    ) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.preview)
            .await?;
        let document = effective
            .pointer(&format!("/automation/{}", target.collection_name()))
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get(target.unique_field()).and_then(Value::as_str) == Some(id))
            })
            .with_context(|| {
                format!(
                    "no {} {id:?} is reachable from selected agent {:?}; use options.agent to select its owning agent, then get to read its automation IDs",
                    target.collection_name(),
                    core.agent_did()
                )
            })?;
        ordered! {"resource": target.collection_name(), "document": document}.pretty()
    }
}
