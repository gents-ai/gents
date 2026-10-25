//! Same-owner configuration closure over the complete transaction snapshot.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config_client::{config_projection, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::Collection;

use super::*;

type Result<T, E = DocumentConfigError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
#[error(
    "{} {id} field {field} references missing {} {target_id:?} within node_did {node_did}",
    collection.graphql_type(),
    target.graphql_type()
)]
pub struct MissingReference {
    pub collection: Collection,
    pub id: String,
    pub field: String,
    pub target: Collection,
    pub target_id: String,
    pub node_did: String,
}

/// Canonical configuration visible for one node; observations are never selected.
#[derive(Debug, Clone, Default)]
pub struct ConfigReferences {
    node_did: String,
    documents: BTreeMap<(Collection, String), Value>,
}

impl ConfigReferences {
    pub(crate) fn documents(&self) -> impl Iterator<Item = (&(Collection, String), &Value)> {
        self.documents.iter()
    }

    pub fn from_documents(
        node_did: &str,
        documents: impl IntoIterator<Item = (Collection, Value)>,
    ) -> std::result::Result<Self, DocumentConfigError> {
        if node_did.trim().is_empty() {
            return Err(DocumentConfigError::ConfigurationRequiresNodeDid);
        }
        let mut snapshot = Self {
            node_did: node_did.to_owned(),
            documents: BTreeMap::new(),
        };
        for (collection, value) in documents {
            let value = config_projection(collection, Some(&value))
                .map_err(|error| DocumentConfigError::DesiredState(Box::new(error)))?
                .1
                .ok_or(DocumentConfigError::MissingCanonicalProjection)?;
            if value["node_did"].as_str() != Some(node_did) {
                return Err(DocumentConfigError::ConfigurationOwnerNotNode {
                    collection,
                    node_did: node_did.to_owned(),
                });
            }
            let id = value[collection.unique_field()]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .ok_or(DocumentConfigError::ConfigurationRequiresLogicalId)?
                .to_owned();
            if snapshot
                .documents
                .insert((collection, id.clone()), value)
                .is_some()
            {
                return Err(DocumentConfigError::DuplicateLiveDocument {
                    collection,
                    node_did: node_did.to_owned(),
                    id,
                });
            }
        }
        Ok(snapshot)
    }

    pub async fn load_in_txn(txn: &ConfigApplyTxn<'_>, node_did: &str) -> Result<Self> {
        if node_did.trim().is_empty() {
            return Err(DocumentConfigError::ConfigurationRequiresNodeDid);
        }
        let mut documents = Vec::new();
        for collection in Collection::ALL {
            let name = collection.graphql_type();
            let fields = config_projection(collection, None)
                .map_err(|error| DocumentConfigError::DesiredState(Box::new(error)))?
                .0
                .join(" ");
            let response = txn
                .execute(&format!(
                    r#"{{ {name}(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
                    escape_graphql_string(node_did)
                ))
                .await?;
            let rows = response
                .get("data")
                .and_then(|data| data.get(name))
                .and_then(Value::as_array)
                .ok_or_else(|| DocumentConfigError::ConfigurationRowsMissing {
                    name: name.to_owned(),
                })?;
            documents.extend(rows.iter().cloned().map(|row| (collection, row)));
        }
        Self::from_documents(node_did, documents)
    }

    pub(crate) fn profile_with_backend(
        &self,
        profile_id: &str,
    ) -> std::result::Result<Option<(InferenceProfile, InferenceBackend)>, DocumentConfigError>
    {
        let Some(profile) = self
            .documents
            .get(&(Collection::InferenceProfile, profile_id.to_owned()))
        else {
            return Ok(None);
        };
        let profile: InferenceProfile = decode(profile)?;
        let backend = self
            .documents
            .get(&(Collection::InferenceBackend, profile.backend_id.clone()))
            .ok_or_else(|| DocumentConfigError::ProfileMissingBackend {
                profile_id: profile_id.to_owned(),
            })?;
        Ok(Some((profile, decode(backend)?)))
    }

    pub(crate) fn profiles_on_backend(&self, backend_id: &str) -> Vec<String> {
        self.documents
            .iter()
            .filter(|((collection, _), value)| {
                *collection == Collection::InferenceProfile
                    && value["backend_id"].as_str() == Some(backend_id)
            })
            .map(|((_, id), _)| id.clone())
            .collect()
    }

    pub(crate) fn agent_profiles(&self, agent_id: &str) -> Vec<String> {
        let get = |collection: Collection, id: Option<&str>| {
            self.documents.get(&(collection, id?.to_owned()))
        };
        let Some(agent) = get(Collection::Agent, Some(agent_id)) else {
            return Vec::new();
        };
        let compaction = get(Collection::AgentContext, agent["context_id"].as_str())
            .and_then(|context| get(Collection::Compaction, context["compaction_id"].as_str()))
            .and_then(|compaction| compaction["inference_profile_id"].as_str());
        let mut profiles: Vec<String> = agent["inference_profile_id"]
            .as_str()
            .into_iter()
            .map(str::to_owned)
            .collect();
        if let Some(compaction) = compaction.filter(|id| !profiles.iter().any(|own| own == id)) {
            profiles.push(compaction.to_owned());
        }
        profiles
    }

    pub(crate) fn agents_on_profile(&self, profile_id: &str) -> Vec<String> {
        self.documents
            .keys()
            .filter(|(collection, id)| {
                *collection == Collection::Agent
                    && self.agent_profiles(id).iter().any(|own| own == profile_id)
            })
            .map(|(_, id)| id.clone())
            .collect()
    }

    /// Unresolvable links are omitted here; publication validates the full
    /// retained candidate before this snapshot is read.
    pub(crate) fn outcome_event_deliveries(&self) -> Vec<(String, String, String, String, bool)> {
        self.documents
            .iter()
            .filter(|((collection, _), _)| *collection == Collection::Trigger)
            .filter_map(|(_, value)| {
                let trigger: Trigger = decode(value).ok()?;
                let TriggerSource::Event { event_source_id } = &trigger.source else {
                    return None;
                };
                let task: Task = decode(
                    self.documents
                        .get(&(Collection::Task, trigger.task_id.clone()))?,
                )
                .ok()?;
                let source: EventSource = decode(
                    self.documents
                        .get(&(Collection::EventSource, event_source_id.clone()))?,
                )
                .ok()?;
                let per_document = source.group.is_none();
                task.emit_outcome.then(|| {
                    (
                        trigger.trigger_id.clone(),
                        task.task_id.clone(),
                        event_source_id.clone(),
                        source.source_collection,
                        per_document,
                    )
                })
            })
            .collect()
    }

    pub fn validate(&self) -> Result<()> {
        for ((collection, _), document) in &self.documents {
            self.validate_document(*collection, document)?;
        }
        Ok(())
    }

    pub fn validate_removal(&self, collection: Collection, id: &str) -> Result<()> {
        let mut candidate = self.clone();
        if candidate
            .documents
            .remove(&(collection, id.to_owned()))
            .is_none()
        {
            return Err(DocumentConfigError::RemovalTargetMissing);
        }
        for ((kind, _), document) in &self.documents {
            candidate.validate_document(*kind, document)?;
        }
        Ok(())
    }

    pub(crate) fn validate_document(&self, collection: Collection, value: &Value) -> Result<()> {
        if value["node_did"].as_str() != Some(&self.node_did) {
            return Err(DocumentConfigError::ConfigOwnerMismatch { collection });
        }
        let id = value[collection.unique_field()]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .ok_or(DocumentConfigError::ConfigurationRequiresLogicalId)?;
        let require = |target: Collection,
                       target_id: &str,
                       field: &str|
         -> std::result::Result<(), DocumentConfigError> {
            if target_id.trim().is_empty() {
                return Err(DocumentConfigError::BlankReferenceId {
                    collection,
                    id: id.to_owned(),
                    field: field.to_owned(),
                });
            }
            if !self.documents.contains_key(&(target, target_id.to_owned())) {
                return Err(MissingReference {
                    collection,
                    id: id.to_owned(),
                    field: field.to_owned(),
                    target,
                    target_id: target_id.to_owned(),
                    node_did: self.node_did.clone(),
                }
                .into());
            }
            Ok(())
        };
        let optional = |target,
                        target_id: Option<&str>,
                        field|
         -> std::result::Result<(), DocumentConfigError> {
            if let Some(target_id) = target_id {
                require(target, target_id, field)?;
            }
            Ok(())
        };
        match collection {
            Collection::Node => {
                let doc: Node = decode(value)?;
                optional(
                    Collection::Agent,
                    doc.default_agent_id.as_deref(),
                    "default_agent_id",
                )?;
                if let Some(default) = doc.default_agent_id.as_deref() {
                    let agent: Agent =
                        decode(&self.documents[&(Collection::Agent, default.to_owned())])?;
                    if !agent.enabled {
                        return Err(DocumentConfigError::DefaultAgentDisabled {
                            agent_id: default.to_owned(),
                            node_did: self.node_did.clone(),
                        });
                    }
                }
            }
            Collection::Agent => {
                let doc: Agent = decode(value)?;
                optional(
                    Collection::AgentContext,
                    doc.context_id.as_deref(),
                    "context_id",
                )?;
                require(
                    Collection::InferenceProfile,
                    &doc.inference_profile_id,
                    "inference_profile_id",
                )?;
            }
            Collection::AgentContext => {
                let doc: AgentContext = decode(value)?;
                optional(Collection::Tools, doc.tools_id.as_deref(), "tools_id")?;
                optional(
                    Collection::Compaction,
                    doc.compaction_id.as_deref(),
                    "compaction_id",
                )?;
                for skill in doc.skill_ids {
                    require(Collection::Skill, &skill, "skill_ids")?;
                }
            }
            Collection::Compaction => {
                let doc: CompactionConfig = decode(value)?;
                doc.validate()?;
                optional(
                    Collection::InferenceProfile,
                    doc.inference_profile_id.as_deref(),
                    "inference_profile_id",
                )?;
            }
            Collection::ChainKeyBinding => decode::<ChainKeyBindingDocument>(value)?.validate()?,
            Collection::EthTool => {
                let doc: EthToolDocument = decode(value)?;
                doc.validate()?;
                optional(
                    Collection::ChainKeyBinding,
                    doc.key_binding_id.as_deref(),
                    "key_binding_id",
                )?;
            }
            Collection::Tools => {
                let doc: Tools = decode(value)?;
                doc.validate()?;
                let surfaces = self
                    .documents
                    .iter()
                    .filter(|((collection, _), _)| *collection == Collection::DatastoreToolSurface)
                    .map(|(_, value)| decode::<DatastoreToolSurfaceDocument>(value))
                    .collect::<Result<Vec<_>, DocumentConfigError>>()?;
                let merged = merge_datastore_tool_surfaces(&doc, &surfaces)?;
                let cli_names = doc
                    .host
                    .as_ref()
                    .into_iter()
                    .flat_map(|host| host.cli.iter().map(|tool| tool.name.clone()))
                    .collect::<Vec<_>>();
                let eth_tools = self
                    .documents
                    .iter()
                    .filter(|((collection, _), _)| *collection == Collection::EthTool)
                    .map(|(_, value)| decode::<EthToolDocument>(value))
                    .collect::<Result<Vec<_>, DocumentConfigError>>()?;
                for tool_id in doc
                    .integrations
                    .as_ref()
                    .and_then(|group| group.eth_tool_ids.as_deref())
                    .unwrap_or(&[])
                {
                    require(Collection::EthTool, tool_id, "integrations.eth_tool_ids")?;
                }
                let expanded =
                    crate::agent::document_view::expand_eth_tools_from_docs(&doc, &eth_tools)?;
                let eth_names = expanded
                    .queries
                    .iter()
                    .map(|query| query.tool_name())
                    .chain(expanded.calls.iter().map(|call| call.tool_name.clone()))
                    .collect::<Vec<_>>();
                validate_surface_tool_names(
                    &merged.write_tools,
                    &merged.query_tools,
                    &cli_names,
                    &eth_names,
                )?;
                if let Some(remote) = &doc.remote {
                    for service in &remote.services {
                        require(
                            Collection::ToolServiceRegistry,
                            &service.mcp_service_id,
                            "remote.services.mcp_service_id",
                        )?;
                    }
                }
                if let Some(agents) = &doc.agents {
                    for target in &agents.target_ids {
                        require(Collection::AgentTarget, target, "agents.target_ids")?;
                    }
                }
                let targets = self
                    .documents
                    .iter()
                    .filter(|((collection, _), _)| *collection == Collection::AgentTarget)
                    .map(|(_, value)| decode::<AgentTargetDocument>(value))
                    .collect::<Result<Vec<_>, DocumentConfigError>>()?;
                crate::tool_surface::AgentToolConfig::from_document_with_targets(&doc, &targets)?;
                if let Some(datastore) = doc.datastore {
                    for surface in datastore.datastore_tool_surface_ids.unwrap_or_default() {
                        require(
                            Collection::DatastoreToolSurface,
                            &surface,
                            "datastore.datastore_tool_surface_ids",
                        )?;
                    }
                }
            }
            Collection::AgentTarget => {
                let doc: AgentTargetDocument = decode(value)?;
                if doc.target_node_did.trim().is_empty() || doc.agent_id.trim().is_empty() {
                    return Err(DocumentConfigError::AgentTargetRequiresIds { id: id.to_owned() });
                }
                // Same-owner targets close legal agent/context/tools cycles;
                // foreign targets are checked by delegation admission under ACP.
                if doc.target_node_did == self.node_did {
                    require(Collection::Agent, &doc.agent_id, "agent_id")?;
                }
            }
            Collection::InferenceProfile => {
                let doc: InferenceProfile = decode(value)?;
                doc.validate()?;
                require(Collection::InferenceBackend, &doc.backend_id, "backend_id")?;
                optional(
                    Collection::InferenceSampling,
                    doc.sampling_id.as_deref(),
                    "sampling_id",
                )?;
                optional(
                    Collection::InferenceExecution,
                    doc.execution_id.as_deref(),
                    "execution_id",
                )?;
            }
            Collection::InferenceExecution => {
                let doc: InferenceExecution = decode(value)?;
                doc.validate()?;
                optional(
                    Collection::InferenceRetryPolicy,
                    doc.retry_policy_id.as_deref(),
                    "retry_policy_id",
                )?;
            }
            Collection::InferenceBackend => decode::<InferenceBackend>(value)?.validate()?,
            Collection::InferenceSampling => decode::<InferenceSampling>(value)?.validate()?,
            Collection::InferenceRetryPolicy => {
                decode::<InferenceRetryPolicy>(value)?.validate()?
            }
            Collection::DatastoreToolSurface => {
                for entry in decode::<DatastoreToolSurfaceDocument>(value)?
                    .entries
                    .unwrap_or_default()
                {
                    entry.validate()?;
                }
            }
            Collection::ProjectionAcpBinding => {
                let doc: ProjectionAcpBinding = decode(value)?;
                doc.validate()?;
                optional(Collection::Agent, doc.agent_id.as_deref(), "agent_id")?;
            }
            Collection::Task => {
                let task: Task = decode(value)?;
                task.validate()?;
                require(Collection::Agent, &task.agent_id, "agent_id")?;
            }
            Collection::Trigger => {
                let doc: Trigger = decode(value)?;
                require(Collection::Task, &doc.task_id, "task_id")?;
                let task: Task = decode(
                    self.documents
                        .get(&(Collection::Task, doc.task_id.clone()))
                        .ok_or(DocumentConfigError::TriggerTaskMissing)?,
                )?;
                let forbidden: &[&str] = match &doc.source {
                    TriggerSource::Schedule { schedule_id } => {
                        require(Collection::Schedule, schedule_id, "source.schedule_id")?;
                        // The fire-time owner, so publication refuses exactly
                        // what every schedule fire would error on (#2080).
                        crate::trigger_engine::validate_task_delivery_options(
                            crate::trigger_engine::TriggerKind::Schedule,
                            task.emit_outcome,
                            doc.concurrency.unwrap_or_default(),
                            doc.session_id_template.is_some(),
                        )
                        .map_err(|source| {
                            DocumentConfigError::TriggerScheduleOption {
                                trigger_id: doc.trigger_id.clone(),
                                source,
                            }
                        })?;
                        &["doc", "args", "group"]
                    }
                    TriggerSource::Event { event_source_id } => {
                        require(
                            Collection::EventSource,
                            event_source_id,
                            "source.event_source_id",
                        )?;
                        let source: EventSource = decode(
                            self.documents
                                .get(&(Collection::EventSource, event_source_id.clone()))
                                .ok_or(DocumentConfigError::TriggerEventSourceMissing)?,
                        )?;
                        if !crate::trigger_engine::durable::outcome_source_allowed(
                            &source.source_collection,
                            task.emit_outcome,
                        ) {
                            return Err(DocumentConfigError::TriggerFireOutcomeSource {
                                trigger_id: doc.trigger_id.clone(),
                                task_id: task.task_id.clone(),
                            });
                        }
                        if source.group.is_some() {
                            &["args"]
                        } else {
                            &["args", "group"]
                        }
                    }
                };
                for (field, template) in
                    std::iter::once(("prompt_template", task.prompt_template.as_str()))
                        .chain(
                            task.goal_objective_template
                                .as_deref()
                                .map(|value| ("goal_objective_template", value)),
                        )
                        .chain(
                            doc.session_id_template
                                .as_deref()
                                .map(|value| ("session_id_template", value)),
                        )
                {
                    for reference in crate::template::parse_template_for_validation(template)? {
                        if reference
                            .root()
                            .is_some_and(|root| forbidden.contains(&root))
                        {
                            return Err(DocumentConfigError::TriggerUnavailableScope {
                                trigger_id: doc.trigger_id.clone(),
                                task_id: task.task_id.clone(),
                                field,
                                scope: reference.path.join("."),
                            });
                        }
                    }
                }
            }
            Collection::Callback => match decode::<Callback>(value)?.handler {
                CallbackHandler::Module { module_id } => {
                    require(Collection::CallbackModule, &module_id, "handler.module_id")?;
                }
                CallbackHandler::Plugin {
                    plugin,
                    digest,
                    correlation_field,
                    outputs,
                    ..
                } => crate::callback::plugin::validate_handler(
                    &plugin,
                    &digest,
                    correlation_field.as_deref(),
                    &outputs,
                )?,
                CallbackHandler::BuiltIn { .. } => {}
            },
            Collection::CallbackBinding => {
                let doc: CallbackBinding = decode(value)?;
                doc.projected_fields()?;
                require(
                    Collection::EventSource,
                    &doc.event_source_id,
                    "event_source_id",
                )?;
                let source: EventSource = decode(
                    self.documents
                        .get(&(Collection::EventSource, doc.event_source_id.clone()))
                        .ok_or(DocumentConfigError::CallbackEventSourceMissing)?,
                )?;
                crate::callback::reject_secret_bearing_callback_fields(
                    &doc.binding_id,
                    source.filter.as_deref(),
                    None,
                )?;
                require(Collection::Callback, &doc.callback_id, "callback_id")?;
            }
            Collection::Schedule => decode::<Schedule>(value)?.cadence.validate()?,
            Collection::EventSource => decode::<EventSource>(value)?.validate()?,
            Collection::ToolServiceRegistry => decode::<ToolServiceRegistry>(value)?.validate()?,
            Collection::RepositoryPlacement => decode::<RepositoryPlacement>(value)?.validate()?,
            Collection::EvalDefinition => decode::<EvalDefinition>(value)?.validate()?,
            Collection::Skill | Collection::CallbackModule | Collection::GraphDefinition => {}
        }
        Ok(())
    }
}

fn decode<T>(value: &Value) -> std::result::Result<T, DocumentConfigError>
where
    T: DeserializeOwned,
{
    Ok(serde_json::from_value(value.clone())?)
}

#[cfg(test)]
mod tests;
