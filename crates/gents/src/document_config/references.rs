//! Same-owner configuration closure over the complete transaction snapshot.
//! Ordinary references never fall back to foreign documents or old snapshots.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::config_client::{config_projection, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::Collection;

use super::*;

/// Canonical configuration visible for one principal. Retained documents and
/// staged replacements share this snapshot; observations are never selected.
#[derive(Debug, Clone, Default)]
pub struct ConfigReferences {
    agent_did: String,
    documents: BTreeMap<(Collection, String), Value>,
}

impl ConfigReferences {
    pub(crate) fn documents(&self) -> impl Iterator<Item = (&(Collection, String), &Value)> {
        self.documents.iter()
    }

    /// Build the same closure check from canonical pack documents. Duplicates
    /// and foreign roots are errors, including collections without references.
    pub fn from_documents(
        agent_did: &str,
        documents: impl IntoIterator<Item = (Collection, Value)>,
    ) -> Result<Self> {
        ensure!(
            !agent_did.trim().is_empty(),
            "configuration requires agent_did"
        );
        let mut snapshot = Self {
            agent_did: agent_did.to_owned(),
            documents: BTreeMap::new(),
        };
        for (collection, value) in documents {
            let value = config_projection(collection, Some(&value))?
                .1
                .context("missing canonical projection")?;
            ensure!(
                value["agent_did"].as_str() == Some(agent_did),
                "{} configuration owner does not match {agent_did:?}",
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
                "multiple live {} documents share owner {agent_did:?} and ID {id:?}",
                collection.graphql_type()
            );
        }
        Ok(snapshot)
    }

    /// Read every retained config row after the transaction's staged writes and
    /// removals. The transaction owns commit/discard and the complete read set.
    pub async fn load_in_txn(txn: &ConfigApplyTxn<'_>, agent_did: &str) -> Result<Self> {
        ensure!(
            !agent_did.trim().is_empty(),
            "configuration requires agent_did"
        );
        let mut documents = Vec::new();
        for collection in Collection::ALL {
            let name = collection.graphql_type();
            let fields = config_projection(collection, None)?.0.join(" ");
            let response = txn
                .execute(&format!(
                    r#"{{ {name}(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{ {fields} }} }}"#,
                    escape_graphql_string(agent_did)
                ))
                .await?;
            let rows = response
                .get("data")
                .and_then(|data| data.get(name))
                .and_then(Value::as_array)
                .with_context(|| format!("{name} configuration query did not return rows"))?;
            documents.extend(rows.iter().cloned().map(|row| (collection, row)));
        }
        Self::from_documents(agent_did, documents)
    }

    /// Validate the full retained candidate, including unchanged inbound links.
    /// Membership checks permit cycles and impose no collection ordering.
    pub fn validate(&self) -> Result<()> {
        for ((collection, _), document) in &self.documents {
            self.validate_document(*collection, document)?;
        }
        self.validate_behavior_scopes()
    }

    /// Check a single conservative prune against every current referrer,
    /// including the removed document itself. This preserves the apply model's
    /// delete-safe rule while sharing the canonical reference definitions.
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
        candidate.validate_behavior_scopes()
    }

    /// Validate the behavior-owned configuration graph after ordinary
    /// references and document-local invariants have succeeded.
    ///
    /// A behavior closure is either wholly legacy-unscoped or wholly scoped to
    /// that behavior. Only the seven mutable owned collections participate;
    /// backend, skill, service, subagent, datastore, and integration references
    /// remain reusable leaves. Every scoped document must also be reachable by
    /// an owned edge from the behavior named by `scope_behavior_id`.
    fn validate_behavior_scopes(&self) -> Result<()> {
        let behaviors = self
            .documents
            .iter()
            .filter(|((collection, _), _)| *collection == Collection::AgentBehavior)
            .map(|(_, value)| decode::<AgentBehavior>(value))
            .collect::<Result<Vec<_>>>()?;
        let behavior_ids = behaviors
            .iter()
            .map(|behavior| behavior.behavior_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut reachable = BTreeSet::new();

        for behavior in &behaviors {
            let profile: InferenceProfile =
                self.owned_document(Collection::InferenceProfile, &behavior.inference_profile_id)?;
            ensure_root_scope(
                &behavior.behavior_id,
                Collection::InferenceProfile,
                &profile.profile_id,
                profile.scope_behavior_id.as_deref(),
            )?;
            let closure_scope = profile.scope_behavior_id.as_deref();

            if let Some(context_id) = behavior.context_id.as_deref() {
                let context: AgentContext =
                    self.owned_document(Collection::AgentContext, context_id)?;
                ensure_root_scope(
                    &behavior.behavior_id,
                    Collection::AgentContext,
                    &context.context_id,
                    context.scope_behavior_id.as_deref(),
                )?;
                ensure_owned_scope(
                    Collection::AgentContext,
                    &context.context_id,
                    context.scope_behavior_id.as_deref(),
                    closure_scope,
                )?;
                self.walk_context(&context, closure_scope, &mut reachable)?;
            }

            self.walk_profile(&profile, closure_scope, &mut reachable)?;
        }

        for ((collection, id), value) in &self.documents {
            if !is_behavior_owned(*collection) {
                continue;
            }
            let Some(scope_behavior_id) = value.get("scope_behavior_id").and_then(Value::as_str)
            else {
                continue;
            };
            ensure!(
                behavior_ids.contains(scope_behavior_id),
                "{} {id} scope_behavior_id {scope_behavior_id:?} names a missing AgentBehavior",
                collection.graphql_type()
            );
            ensure!(
                reachable.contains(&(*collection, id.clone())),
                "{} {id} is scoped to behavior {scope_behavior_id:?} but is not reachable from that behavior",
                collection.graphql_type()
            );
        }

        Ok(())
    }

    fn walk_context(
        &self,
        context: &AgentContext,
        expected_scope: Option<&str>,
        reachable: &mut BTreeSet<(Collection, String)>,
    ) -> Result<()> {
        ensure_owned_scope(
            Collection::AgentContext,
            &context.context_id,
            context.scope_behavior_id.as_deref(),
            expected_scope,
        )?;
        reachable.insert((Collection::AgentContext, context.context_id.clone()));

        if let Some(tools_id) = context.tools_id.as_deref() {
            let tools: Tools = self.owned_document(Collection::Tools, tools_id)?;
            ensure_owned_scope(
                Collection::Tools,
                &tools.tools_id,
                tools.scope_behavior_id.as_deref(),
                expected_scope,
            )?;
            reachable.insert((Collection::Tools, tools.tools_id));
        }
        if let Some(compaction_id) = context.compaction_id.as_deref() {
            let compaction: CompactionConfig =
                self.owned_document(Collection::Compaction, compaction_id)?;
            self.walk_compaction(&compaction, expected_scope, reachable)?;
        }
        Ok(())
    }

    fn walk_compaction(
        &self,
        compaction: &CompactionConfig,
        expected_scope: Option<&str>,
        reachable: &mut BTreeSet<(Collection, String)>,
    ) -> Result<()> {
        ensure_owned_scope(
            Collection::Compaction,
            &compaction.compaction_id,
            compaction.scope_behavior_id.as_deref(),
            expected_scope,
        )?;
        reachable.insert((Collection::Compaction, compaction.compaction_id.clone()));
        if let Some(profile_id) = compaction.inference_profile_id.as_deref() {
            let profile: InferenceProfile =
                self.owned_document(Collection::InferenceProfile, profile_id)?;
            self.walk_profile(&profile, expected_scope, reachable)?;
        }
        Ok(())
    }

    fn walk_profile(
        &self,
        profile: &InferenceProfile,
        expected_scope: Option<&str>,
        reachable: &mut BTreeSet<(Collection, String)>,
    ) -> Result<()> {
        ensure_owned_scope(
            Collection::InferenceProfile,
            &profile.profile_id,
            profile.scope_behavior_id.as_deref(),
            expected_scope,
        )?;
        reachable.insert((Collection::InferenceProfile, profile.profile_id.clone()));

        if let Some(sampling_id) = profile.sampling_id.as_deref() {
            let sampling: InferenceSampling =
                self.owned_document(Collection::InferenceSampling, sampling_id)?;
            ensure_owned_scope(
                Collection::InferenceSampling,
                &sampling.sampling_id,
                sampling.scope_behavior_id.as_deref(),
                expected_scope,
            )?;
            reachable.insert((Collection::InferenceSampling, sampling.sampling_id));
        }
        if let Some(execution_id) = profile.execution_id.as_deref() {
            let execution: InferenceExecution =
                self.owned_document(Collection::InferenceExecution, execution_id)?;
            self.walk_execution(&execution, expected_scope, reachable)?;
        }
        Ok(())
    }

    fn walk_execution(
        &self,
        execution: &InferenceExecution,
        expected_scope: Option<&str>,
        reachable: &mut BTreeSet<(Collection, String)>,
    ) -> Result<()> {
        ensure_owned_scope(
            Collection::InferenceExecution,
            &execution.execution_id,
            execution.scope_behavior_id.as_deref(),
            expected_scope,
        )?;
        reachable.insert((
            Collection::InferenceExecution,
            execution.execution_id.clone(),
        ));
        if let Some(retry_policy_id) = execution.retry_policy_id.as_deref() {
            let retry: InferenceRetryPolicy =
                self.owned_document(Collection::InferenceRetryPolicy, retry_policy_id)?;
            ensure_owned_scope(
                Collection::InferenceRetryPolicy,
                &retry.retry_policy_id,
                retry.scope_behavior_id.as_deref(),
                expected_scope,
            )?;
            reachable.insert((Collection::InferenceRetryPolicy, retry.retry_policy_id));
        }
        Ok(())
    }

    fn owned_document<T: DeserializeOwned>(&self, collection: Collection, id: &str) -> Result<T> {
        decode(
            self.documents
                .get(&(collection, id.to_owned()))
                .with_context(|| {
                    format!(
                        "behavior scope traversal missing {} {id:?}",
                        collection.graphql_type()
                    )
                })?,
        )
    }

    pub(crate) fn validate_document(&self, collection: Collection, value: &Value) -> Result<()> {
        ensure!(
            value["agent_did"].as_str() == Some(&self.agent_did),
            "{} configuration owner does not match reference scope",
            collection.graphql_type()
        );
        let id = value[collection.unique_field()]
            .as_str()
            .filter(|id| !id.trim().is_empty())
            .context("configuration requires logical ID")?;
        let require = |target: Collection, target_id: &str, field: &str| -> Result<()> {
            ensure!(
                !target_id.trim().is_empty(),
                "{} {id} field {field} has a blank reference ID",
                collection.graphql_type()
            );
            ensure!(
                self.documents.contains_key(&(target, target_id.to_owned())),
                "{} {id} field {field} references missing {} {target_id:?} within agent_did {}",
                collection.graphql_type(),
                target.graphql_type(),
                self.agent_did
            );
            Ok(())
        };
        let optional = |target, target_id: Option<&str>, field| -> Result<()> {
            if let Some(target_id) = target_id {
                require(target, target_id, field)?;
            }
            Ok(())
        };
        // Use canonical fields directly; this is the only ordinary-reference
        // rule table. Defra ACP and explicit foreign delegation retain their
        // existing owners and are not converted into ordinary references.
        match collection {
            Collection::AgentPrincipal => {
                let doc: AgentPrincipal = decode(value)?;
                optional(
                    Collection::AgentBehavior,
                    doc.default_behavior_id.as_deref(),
                    "default_behavior_id",
                )?;
            }
            Collection::AgentBehavior => {
                let doc: AgentBehavior = decode(value)?;
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
                    .collect::<Result<Vec<_>>>()?;
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
                    .collect::<Result<Vec<_>>>()?;
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
                if let Some(remote) = doc.remote {
                    for service in remote.services {
                        require(
                            Collection::ToolServiceRegistry,
                            &service.mcp_service_id,
                            "remote.services.mcp_service_id",
                        )?;
                    }
                }
                if let Some(subagents) = doc.subagents {
                    for target in subagents.target_ids {
                        require(Collection::SubagentTarget, &target, "subagents.target_ids")?;
                    }
                }
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
            Collection::SubagentTarget => {
                let doc: SubagentTargetDocument = decode(value)?;
                ensure!(
                    !doc.target_agent_did.trim().is_empty() && !doc.behavior_id.trim().is_empty(),
                    "SubagentTarget {id} requires target_agent_did and behavior_id"
                );
                // Same-owner targets close legal behavior/context/tools cycles.
                // Explicit foreign targets are checked by delegation admission
                // under ACP, never satisfied by a global config lookup.
                if doc.target_agent_did == self.agent_did {
                    require(Collection::AgentBehavior, &doc.behavior_id, "behavior_id")?;
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
                optional(
                    Collection::AgentBehavior,
                    doc.behavior_id.as_deref(),
                    "behavior_id",
                )?;
            }
            Collection::Task => {
                let task: Task = decode(value)?;
                task.validate()?;
                require(Collection::AgentBehavior, &task.behavior_id, "behavior_id")?;
            }
            Collection::Trigger => {
                let doc: Trigger = decode(value)?;
                require(Collection::Task, &doc.task_id, "task_id")?;
                let task: Task = decode(
                    self.documents
                        .get(&(Collection::Task, doc.task_id.clone()))
                        .context("trigger task missing")?,
                )?;
                let forbidden: &[&str] = match &doc.source {
                    TriggerSource::Schedule { schedule_id } => {
                        require(Collection::Schedule, schedule_id, "source.schedule_id")?;
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
                                .context("trigger event source missing")?,
                        )?;
                        if source.group.is_some() {
                            &["args"]
                        } else {
                            &["args", "group"]
                        }
                    }
                };
                for (field, template) in
                    std::iter::once(("prompt_template", task.prompt_template.as_str())).chain(
                        task.goal_objective_template
                            .as_deref()
                            .map(|value| ("goal_objective_template", value)),
                    )
                {
                    for reference in crate::template::parse_template_for_validation(template)? {
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
            Collection::Callback => {
                if let CallbackHandler::Module { module_id } = decode::<Callback>(value)?.handler {
                    require(Collection::CallbackModule, &module_id, "handler.module_id")?;
                }
            }
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
                        .context("callback event source missing")?,
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
            // Skill tool_refs name tools, not config documents. Schema names,
            // ACP policy IDs, hook commands and tags keep their existing owners.
            Collection::Skill | Collection::CallbackModule | Collection::GraphDefinition => {}
        }
        Ok(())
    }
}

fn is_behavior_owned(collection: Collection) -> bool {
    matches!(
        collection,
        Collection::AgentContext
            | Collection::Tools
            | Collection::Compaction
            | Collection::InferenceProfile
            | Collection::InferenceSampling
            | Collection::InferenceExecution
            | Collection::InferenceRetryPolicy
    )
}

fn ensure_root_scope(
    behavior_id: &str,
    collection: Collection,
    id: &str,
    actual_scope: Option<&str>,
) -> Result<()> {
    ensure!(
        actual_scope.is_none() || actual_scope == Some(behavior_id),
        "AgentBehavior {behavior_id} references {} {id} scoped to {:?}; expected legacy null or {behavior_id:?}",
        collection.graphql_type(),
        actual_scope
    );
    Ok(())
}

fn ensure_owned_scope(
    collection: Collection,
    id: &str,
    actual_scope: Option<&str>,
    expected_scope: Option<&str>,
) -> Result<()> {
    ensure!(
        actual_scope == expected_scope,
        "{} {id} scope_behavior_id {:?} does not match its owned closure scope {:?}",
        collection.graphql_type(),
        actual_scope,
        expected_scope
    );
    Ok(())
}

fn decode<T: DeserializeOwned>(value: &Value) -> Result<T> {
    Ok(serde_json::from_value(value.clone())?)
}

#[cfg(test)]
mod tests;
