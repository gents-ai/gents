//! Configuration closure over the authenticated actor's transaction snapshot.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::store::config_client::{config_projection, ConfigApplyTxn};
use crate::store::graphql::escape_graphql_string;
use crate::Collection;

use super::*;

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

/// One canonical reference edge: `field` of the source document names `id` in
/// `collection`. Blank IDs are reported so callers can reject them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RequiredReference {
    pub(crate) collection: Collection,
    pub(crate) id: String,
    pub(crate) field: &'static str,
}

impl ConfigReferences {
    pub(crate) fn documents(&self) -> impl Iterator<Item = (&(Collection, String), &Value)> {
        self.documents.iter()
    }

    pub fn from_documents(
        node_did: &str,
        documents: impl IntoIterator<Item = (Collection, Value)>,
    ) -> Result<Self> {
        ensure!(
            !node_did.trim().is_empty(),
            "configuration requires node_did"
        );
        let mut snapshot = Self {
            node_did: node_did.to_owned(),
            documents: BTreeMap::new(),
        };
        for (collection, value) in documents {
            let value = config_projection(collection, Some(&value))?
                .1
                .context("missing canonical projection")?;
            ensure!(
                value["node_did"].as_str() == Some(node_did),
                "{} configuration owner does not match {node_did:?}",
                collection.graphql_type()
            );
            let id = value[collection.unique_field()]
                .as_str()
                .filter(|id| !id.trim().is_empty())
                .context("configuration requires logical ID")?
                .to_owned();
            ensure!(
                snapshot
                    .documents
                    .insert((collection, id.clone()), value)
                    .is_none(),
                "multiple live {} documents share owner {node_did:?} and ID {id:?}",
                collection.graphql_type()
            );
        }
        Ok(snapshot)
    }

    pub async fn load_in_txn(txn: &ConfigApplyTxn<'_>, node_did: &str) -> Result<Self> {
        ensure!(
            !node_did.trim().is_empty(),
            "configuration requires node_did"
        );
        let mut documents = Vec::new();
        for collection in Collection::ALL {
            let name = collection.graphql_type();
            let fields = if collection == Collection::InferenceBackend {
                super::backend_metadata_fields()?
            } else {
                config_projection(collection, None)?.0
            }
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
                .with_context(|| format!("{name} configuration query did not return rows"))?;
            for row in rows {
                let value = if collection == Collection::InferenceBackend {
                    let metadata: InferenceBackendMetadata = serde_json::from_value(row.clone())
                        .context("decoding backend metadata in configuration references")?;
                    serde_json::to_value(metadata.into_backend()?)?
                } else {
                    row.clone()
                };
                documents.push((collection, value));
            }
        }
        Self::from_documents(node_did, documents)
    }

    pub(crate) fn profile_with_backend(
        &self,
        profile_id: &str,
    ) -> Result<Option<(InferenceProfile, InferenceBackend)>> {
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
            .with_context(|| format!("profile {profile_id} references a missing backend"))?;
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

    pub(crate) fn agent_profiles(&self, agent_did: &str) -> Vec<String> {
        let get = |collection: Collection, id: Option<&str>| {
            self.documents.get(&(collection, id?.to_owned()))
        };
        let Some(agent) = get(Collection::Agent, Some(agent_did)) else {
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

    /// Validate changed documents and their visible direct consumers. Native
    /// actor-scoped reads do not establish whether hidden consumers exist.
    pub(crate) fn validate_affected(&self, changed: &BTreeSet<(Collection, String)>) -> Result<()> {
        for ((collection, id), document) in &self.documents {
            let references = self.required_references(*collection, document)?;
            if changed.contains(&(*collection, id.clone()))
                || references.iter().any(|reference| {
                    changed.contains(&(reference.collection, reference.id.clone()))
                })
            {
                self.validate_document(*collection, document)?;
            }
        }
        Ok(())
    }

    fn required_references(
        &self,
        collection: Collection,
        value: &Value,
    ) -> Result<Vec<RequiredReference>> {
        references_of(collection, value, &self.node_did)
    }

    pub fn validate_removal(&self, collection: Collection, id: &str) -> Result<()> {
        let mut candidate = self.clone();
        ensure!(
            candidate
                .documents
                .remove(&(collection, id.to_owned()))
                .is_some(),
            "configuration selected for removal does not exist"
        );
        for ((kind, _), document) in &self.documents {
            candidate.validate_document(*kind, document)?;
        }
        Ok(())
    }

    pub(crate) fn validate_document(&self, collection: Collection, value: &Value) -> Result<()> {
        ensure!(
            value["node_did"].as_str() == Some(&self.node_did),
            "{} configuration owner does not match reference scope",
            collection.graphql_type()
        );
        let id = value[collection.unique_field()]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .context("configuration requires logical ID")?;
        for reference in self.required_references(collection, value)? {
            let target = reference.collection;
            let target_id = &reference.id;
            let field = reference.field;
            ensure!(
                !target_id.trim().is_empty(),
                "{} {id} field {field} has a blank reference ID",
                collection.graphql_type()
            );
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
        }
        match collection {
            Collection::Node => {
                let doc: Node = decode(value)?;
                if let Some(default) = doc.default_agent_did.as_deref() {
                    let agent: Agent =
                        decode(&self.documents[&(Collection::Agent, default.to_owned())])?;
                    ensure!(
                        agent.enabled,
                        "Agent {default:?} is the default agent of {} and must be \
                         enabled; enable it or choose another default first",
                        self.node_did
                    );
                }
            }
            Collection::Agent | Collection::AgentContext => {}
            Collection::Compaction => {
                let doc: CompactionConfig = decode(value)?;
                doc.validate()?;
            }
            Collection::Tools => {
                let doc: Tools = decode(value)?;
                doc.validate()?;
                let surfaces = self
                    .documents
                    .iter()
                    .filter(|((collection, _), _)| *collection == Collection::DatastoreToolSurface)
                    .map(|(_, value)| decode::<DatastoreToolSurfaceDocument>(value))
                    .collect::<Result<Vec<_>>>()?;
                let merged = merge_datastore_tool_surfaces(&doc, &surfaces)?;
                let cli_names = doc
                    .host
                    .as_ref()
                    .into_iter()
                    .flat_map(|host| host.cli.iter().map(|tool| tool.name.clone()))
                    .collect::<Vec<_>>();
                validate_surface_tool_names(
                    &merged.write_tools,
                    &merged.query_tools,
                    &cli_names,
                    &[],
                )?;
                let targets = self
                    .documents
                    .iter()
                    .filter(|((collection, _), _)| *collection == Collection::AgentTarget)
                    .map(|(_, value)| decode::<AgentTargetDocument>(value))
                    .collect::<Result<Vec<_>>>()?;
                crate::tools::surface::AgentToolConfig::from_document_with_targets(&doc, &targets)?;
            }
            Collection::AgentTarget => {
                let doc: AgentTargetDocument = decode(value)?;
                ensure!(
                    !doc.target_node_did.trim().is_empty() && !doc.agent_did.trim().is_empty(),
                    "AgentTarget {id} requires target_node_did and agent_did"
                );
            }
            Collection::InferenceProfile => {
                let doc: InferenceProfile = decode(value)?;
                doc.validate()?;
            }
            Collection::InferenceExecution => {
                let doc: InferenceExecution = decode(value)?;
                doc.validate()?;
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
            }
            Collection::Task => {
                let task: Task = decode(value)?;
                task.validate()?;
            }
            Collection::Trigger => {
                let doc: Trigger = decode(value)?;
                let task: Task = decode(
                    self.documents
                        .get(&(Collection::Task, doc.task_id.clone()))
                        .context("trigger task missing")?,
                )?;
                let forbidden: &[&str] = match &doc.source {
                    TriggerSource::Schedule { .. } => {
                        // The fire-time owner, so publication refuses exactly
                        // what every schedule fire would error on (#2080).
                        crate::triggers::engine::validate_task_delivery_options(
                            crate::triggers::engine::TriggerKind::Schedule,
                            task.emit_outcome,
                            doc.concurrency.unwrap_or_default(),
                            doc.session_id_template.is_some(),
                        )
                        .with_context(|| {
                            format!(
                                "trigger {} has a schedule source; use an event source or remove the option",
                                doc.trigger_id
                            )
                        })?;
                        &["doc", "args", "group"]
                    }
                    TriggerSource::Event { event_source_id } => {
                        let source: EventSource = decode(
                            self.documents
                                .get(&(Collection::EventSource, event_source_id.clone()))
                                .context("trigger event source missing")?,
                        )?;
                        ensure!(
                            super::event_trigger::outcome_source_allowed(
                                &source.source_collection,
                                task.emit_outcome,
                            ),
                            "trigger {} sources FireOutcome but task {} enables emit_outcome",
                            doc.trigger_id,
                            task.task_id
                        );
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
                    for reference in
                        crate::documents::template::parse_template_for_validation(template)?
                    {
                        ensure!(
                            !reference
                                .root()
                                .is_some_and(|root| forbidden.contains(&root)),
                            "trigger {} task {} {field} references unavailable source scope {}",
                            doc.trigger_id,
                            task.task_id,
                            reference.path.join(".")
                        );
                    }
                }
            }
            Collection::Callback => match decode::<Callback>(value)?.handler {
                CallbackHandler::Module { .. } => {}
                CallbackHandler::Plugin {
                    plugin,
                    digest,
                    correlation_field,
                    outputs,
                    ..
                } => crate::triggers::callback::plugin::validate_handler(
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
                let source: EventSource = decode(
                    self.documents
                        .get(&(Collection::EventSource, doc.event_source_id.clone()))
                        .context("callback event source missing")?,
                )?;
                crate::triggers::callback::reject_secret_bearing_callback_fields(
                    &doc.binding_id,
                    source.filter.as_deref(),
                    None,
                )?;
            }
            Collection::Schedule => decode::<Schedule>(value)?.cadence.validate()?,
            Collection::EventSource => decode::<EventSource>(value)?.validate()?,
            Collection::ToolServiceRegistry => decode::<ToolServiceRegistry>(value)?.validate()?,
            Collection::RepositoryPlacement => decode::<RepositoryPlacement>(value)?.validate()?,
            Collection::EvalDefinition => decode::<EvalDefinition>(value)?.validate()?,
            Collection::Skill | Collection::CallbackModule => {}
        }
        Ok(())
    }
}

/// Every reference edge of one canonical document, decoded through its typed
/// configuration struct. `node_did` is the owner whose AgentTargets are local.
pub(crate) fn references_of(
    collection: Collection,
    value: &Value,
    node_did: &str,
) -> Result<Vec<RequiredReference>> {
    let mut references = Vec::new();
    let mut push = |collection, id: &str, field| {
        references.push(RequiredReference {
            collection,
            id: id.to_owned(),
            field,
        })
    };
    match collection {
        Collection::Node => {
            let doc: Node = decode(value)?;
            if let Some(id) = doc.default_agent_did.as_deref() {
                push(Collection::Agent, id, "default_agent_did");
            }
        }
        Collection::Agent => {
            let doc: Agent = decode(value)?;
            if let Some(id) = doc.context_id.as_deref() {
                push(Collection::AgentContext, id, "context_id");
            }
            push(
                Collection::InferenceProfile,
                &doc.inference_profile_id,
                "inference_profile_id",
            );
        }
        Collection::AgentContext => {
            let doc: AgentContext = decode(value)?;
            if let Some(id) = doc.tools_id.as_deref() {
                push(Collection::Tools, id, "tools_id");
            }
            if let Some(id) = doc.compaction_id.as_deref() {
                push(Collection::Compaction, id, "compaction_id");
            }
            for id in &doc.skill_ids {
                push(Collection::Skill, id, "skill_ids");
            }
        }
        Collection::Compaction => {
            let doc: CompactionConfig = decode(value)?;
            if let Some(id) = doc.inference_profile_id.as_deref() {
                push(Collection::InferenceProfile, id, "inference_profile_id");
            }
        }
        Collection::Tools => {
            let doc: Tools = decode(value)?;
            if let Some(remote) = &doc.remote {
                for service in &remote.services {
                    push(
                        Collection::ToolServiceRegistry,
                        &service.mcp_service_id,
                        "remote.services.mcp_service_id",
                    );
                }
            }
            if let Some(agents) = &doc.agents {
                for id in &agents.target_ids {
                    push(Collection::AgentTarget, id, "agents.target_ids");
                }
            }
            if let Some(datastore) = &doc.datastore {
                for id in datastore.datastore_tool_surface_ids.iter().flatten() {
                    push(
                        Collection::DatastoreToolSurface,
                        id,
                        "datastore.datastore_tool_surface_ids",
                    );
                }
            }
        }
        Collection::AgentTarget => {
            let doc: AgentTargetDocument = decode(value)?;
            if doc.target_node_did == node_did {
                push(Collection::Agent, &doc.agent_did, "agent_did");
            }
        }
        Collection::InferenceProfile => {
            let doc: InferenceProfile = decode(value)?;
            push(Collection::InferenceBackend, &doc.backend_id, "backend_id");
            if let Some(id) = doc.sampling_id.as_deref() {
                push(Collection::InferenceSampling, id, "sampling_id");
            }
            if let Some(id) = doc.execution_id.as_deref() {
                push(Collection::InferenceExecution, id, "execution_id");
            }
        }
        Collection::InferenceExecution => {
            let doc: InferenceExecution = decode(value)?;
            if let Some(id) = doc.retry_policy_id.as_deref() {
                push(Collection::InferenceRetryPolicy, id, "retry_policy_id");
            }
        }
        Collection::ProjectionAcpBinding => {
            let doc: ProjectionAcpBinding = decode(value)?;
            if let Some(id) = doc.agent_did.as_deref() {
                push(Collection::Agent, id, "agent_did");
            }
        }
        Collection::Task => {
            let doc: Task = decode(value)?;
            push(Collection::Agent, &doc.agent_did, "agent_did");
        }
        Collection::Trigger => {
            let doc: Trigger = decode(value)?;
            push(Collection::Task, &doc.task_id, "task_id");
            match &doc.source {
                TriggerSource::Schedule { schedule_id } => {
                    push(Collection::Schedule, schedule_id, "source.schedule_id")
                }
                TriggerSource::Event { event_source_id } => push(
                    Collection::EventSource,
                    event_source_id,
                    "source.event_source_id",
                ),
            }
        }
        Collection::Callback => {
            let doc: Callback = decode(value)?;
            if let CallbackHandler::Module { module_id, .. } = &doc.handler {
                push(Collection::CallbackModule, module_id, "handler.module_id");
            }
            let selection = match &doc.handler {
                CallbackHandler::BuiltIn {
                    workspace_agents, ..
                }
                | CallbackHandler::Module {
                    workspace_agents, ..
                } => workspace_agents.as_ref(),
                CallbackHandler::Plugin { .. } => None,
            };
            if let Some(selection) = selection {
                push(
                    Collection::Agent,
                    &selection.writer_agent_did,
                    "handler.workspace_agents.writer_agent_did",
                );
                push(
                    Collection::Agent,
                    &selection.integrator_agent_did,
                    "handler.workspace_agents.integrator_agent_did",
                );
                for reader in &selection.readers {
                    push(
                        Collection::Agent,
                        &reader.agent_did,
                        "handler.workspace_agents.readers.agent_did",
                    );
                }
            }
        }
        Collection::CallbackBinding => {
            let doc: CallbackBinding = decode(value)?;
            push(Collection::Agent, &doc.agent_did, "agent_did");
            push(
                Collection::EventSource,
                &doc.event_source_id,
                "event_source_id",
            );
            push(Collection::Callback, &doc.callback_id, "callback_id");
        }
        Collection::InferenceBackend
        | Collection::InferenceSampling
        | Collection::InferenceRetryPolicy
        | Collection::DatastoreToolSurface
        | Collection::Schedule
        | Collection::EventSource
        | Collection::ToolServiceRegistry
        | Collection::RepositoryPlacement
        | Collection::EvalDefinition
        | Collection::Skill
        | Collection::CallbackModule => {}
    }
    Ok(references)
}

fn decode<T>(value: &Value) -> Result<T>
where
    T: DeserializeOwned,
{
    Ok(serde_json::from_value(value.clone())?)
}

#[cfg(test)]
mod tests;
