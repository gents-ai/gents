//! Transactional planning for behavior-owned configuration closures.
//!
//! Mutable context and inference documents are copied to deterministic target
//! IDs. Reusable resources remain references. The returned desired-state plan
//! is applied by the existing transaction owner, so staged replacements,
//! stale scoped removals, reference validation, and commit/discard share one
//! atomic boundary.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Context, Result};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use super::{
    apply_desired_state_plan, config_projection, ConfigApplyTxn, DesiredStateApplyCounts,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::behavior_scope::{
    behavior_component_id, valid_generated_qualified_key, BehaviorComponentPath,
};
use crate::document_config::{
    AgentBehavior, AgentContext, CompactionConfig, ConfigReferences, InferenceExecution,
    InferenceProfile, InferenceRetryPolicy, InferenceSampling, Tools,
};
use crate::Collection;

type DocumentKey = (Collection, String);

const OWNED_COLLECTIONS: [Collection; 7] = [
    Collection::AgentContext,
    Collection::Tools,
    Collection::Compaction,
    Collection::InferenceProfile,
    Collection::InferenceSampling,
    Collection::InferenceExecution,
    Collection::InferenceRetryPolicy,
];

#[derive(Clone, Copy)]
struct InferencePaths {
    profile: BehaviorComponentPath,
    sampling: BehaviorComponentPath,
    execution: BehaviorComponentPath,
    retry_policy: BehaviorComponentPath,
}

const PRIMARY_INFERENCE_PATHS: InferencePaths = InferencePaths {
    profile: BehaviorComponentPath::Inference,
    sampling: BehaviorComponentPath::Sampling,
    execution: BehaviorComponentPath::Execution,
    retry_policy: BehaviorComponentPath::RetryPolicy,
};

const COMPACTION_INFERENCE_PATHS: InferencePaths = InferencePaths {
    profile: BehaviorComponentPath::CompactionInference,
    sampling: BehaviorComponentPath::CompactionSampling,
    execution: BehaviorComponentPath::CompactionExecution,
    retry_policy: BehaviorComponentPath::CompactionRetryPolicy,
};

struct ClosurePlanner<'a> {
    snapshot: &'a ConfigReferences,
    overlays: &'a BTreeMap<DocumentKey, Value>,
    agent_did: &'a str,
    source_behavior_id: &'a str,
    target_behavior_id: &'a str,
    visited: BTreeMap<DocumentKey, String>,
    documents: Vec<DesiredStateApplyDocument>,
}

impl<'a> ClosurePlanner<'a> {
    fn new(
        snapshot: &'a ConfigReferences,
        overlays: &'a BTreeMap<DocumentKey, Value>,
        agent_did: &'a str,
        source_behavior_id: &'a str,
        target_behavior_id: &'a str,
    ) -> Self {
        Self {
            snapshot,
            overlays,
            agent_did,
            source_behavior_id,
            target_behavior_id,
            visited: BTreeMap::new(),
            documents: Vec::new(),
        }
    }

    fn source<T: DeserializeOwned>(&self, collection: Collection, id: &str) -> Result<T> {
        let value = self
            .overlays
            .get(&(collection, id.to_owned()))
            .or_else(|| snapshot_document(self.snapshot, collection, id))
            .with_context(|| {
                format!(
                    "source behavior {:?} references missing {} {:?}",
                    self.source_behavior_id,
                    collection.graphql_type(),
                    id
                )
            })?;
        ensure!(
            value.get("agent_did").and_then(Value::as_str) == Some(self.agent_did),
            "source {} {:?} belongs to a different principal",
            collection.graphql_type(),
            id
        );
        serde_json::from_value(value.clone())
            .with_context(|| format!("decode source {} {:?}", collection.graphql_type(), id))
    }

    fn begin_component(
        &mut self,
        collection: Collection,
        source_id: &str,
        path: BehaviorComponentPath,
    ) -> Option<String> {
        let key = (collection, source_id.to_owned());
        if let Some(target_id) = self.visited.get(&key) {
            return Some(target_id.clone());
        }
        let target_id = behavior_component_id(self.target_behavior_id, path);
        self.visited.insert(key, target_id.clone());
        None
    }

    fn push<T: Serialize>(&mut self, collection: Collection, document: &T) -> Result<()> {
        let value = serde_json::to_value(document)?;
        self.documents.push(DesiredStateApplyDocument {
            collection,
            add: value.clone(),
            update: value,
        });
        Ok(())
    }

    fn clone_tools(&mut self, source_id: &str) -> Result<String> {
        if let Some(target_id) =
            self.begin_component(Collection::Tools, source_id, BehaviorComponentPath::Tools)
        {
            return Ok(target_id);
        }
        let target_id =
            behavior_component_id(self.target_behavior_id, BehaviorComponentPath::Tools);
        let mut document: Tools = self.source(Collection::Tools, source_id)?;
        document.tools_id = target_id.clone();
        document.scope_behavior_id = Some(self.target_behavior_id.to_owned());
        self.push(Collection::Tools, &document)?;
        Ok(target_id)
    }

    fn clone_retry_policy(
        &mut self,
        source_id: &str,
        path: BehaviorComponentPath,
    ) -> Result<String> {
        if let Some(target_id) =
            self.begin_component(Collection::InferenceRetryPolicy, source_id, path)
        {
            return Ok(target_id);
        }
        let target_id = behavior_component_id(self.target_behavior_id, path);
        let mut document: InferenceRetryPolicy =
            self.source(Collection::InferenceRetryPolicy, source_id)?;
        document.retry_policy_id = target_id.clone();
        document.scope_behavior_id = Some(self.target_behavior_id.to_owned());
        self.push(Collection::InferenceRetryPolicy, &document)?;
        Ok(target_id)
    }

    fn clone_execution(&mut self, source_id: &str, paths: InferencePaths) -> Result<String> {
        if let Some(target_id) =
            self.begin_component(Collection::InferenceExecution, source_id, paths.execution)
        {
            return Ok(target_id);
        }
        let target_id = behavior_component_id(self.target_behavior_id, paths.execution);
        let mut document: InferenceExecution =
            self.source(Collection::InferenceExecution, source_id)?;
        document.execution_id = target_id.clone();
        document.scope_behavior_id = Some(self.target_behavior_id.to_owned());
        document.retry_policy_id = document
            .retry_policy_id
            .as_deref()
            .map(|id| self.clone_retry_policy(id, paths.retry_policy))
            .transpose()?;
        self.push(Collection::InferenceExecution, &document)?;
        Ok(target_id)
    }

    fn clone_sampling(&mut self, source_id: &str, path: BehaviorComponentPath) -> Result<String> {
        if let Some(target_id) =
            self.begin_component(Collection::InferenceSampling, source_id, path)
        {
            return Ok(target_id);
        }
        let target_id = behavior_component_id(self.target_behavior_id, path);
        let mut document: InferenceSampling =
            self.source(Collection::InferenceSampling, source_id)?;
        document.sampling_id = target_id.clone();
        document.scope_behavior_id = Some(self.target_behavior_id.to_owned());
        self.push(Collection::InferenceSampling, &document)?;
        Ok(target_id)
    }

    fn clone_profile(&mut self, source_id: &str, paths: InferencePaths) -> Result<String> {
        if let Some(target_id) =
            self.begin_component(Collection::InferenceProfile, source_id, paths.profile)
        {
            return Ok(target_id);
        }
        let target_id = behavior_component_id(self.target_behavior_id, paths.profile);
        let mut document: InferenceProfile =
            self.source(Collection::InferenceProfile, source_id)?;
        document.profile_id = target_id.clone();
        document.scope_behavior_id = Some(self.target_behavior_id.to_owned());
        document.sampling_id = document
            .sampling_id
            .as_deref()
            .map(|id| self.clone_sampling(id, paths.sampling))
            .transpose()?;
        document.execution_id = document
            .execution_id
            .as_deref()
            .map(|id| self.clone_execution(id, paths))
            .transpose()?;
        self.push(Collection::InferenceProfile, &document)?;
        Ok(target_id)
    }

    fn clone_compaction(&mut self, source_id: &str) -> Result<String> {
        if let Some(target_id) = self.begin_component(
            Collection::Compaction,
            source_id,
            BehaviorComponentPath::Compaction,
        ) {
            return Ok(target_id);
        }
        let target_id =
            behavior_component_id(self.target_behavior_id, BehaviorComponentPath::Compaction);
        let mut document: CompactionConfig = self.source(Collection::Compaction, source_id)?;
        document.compaction_id = target_id.clone();
        document.scope_behavior_id = Some(self.target_behavior_id.to_owned());
        document.inference_profile_id = document
            .inference_profile_id
            .as_deref()
            .map(|id| self.clone_profile(id, COMPACTION_INFERENCE_PATHS))
            .transpose()?;
        self.push(Collection::Compaction, &document)?;
        Ok(target_id)
    }

    fn clone_context(&mut self, source_id: &str) -> Result<String> {
        if let Some(target_id) = self.begin_component(
            Collection::AgentContext,
            source_id,
            BehaviorComponentPath::Context,
        ) {
            return Ok(target_id);
        }
        let target_id =
            behavior_component_id(self.target_behavior_id, BehaviorComponentPath::Context);
        let mut document: AgentContext = self.source(Collection::AgentContext, source_id)?;
        document.context_id = target_id.clone();
        document.scope_behavior_id = Some(self.target_behavior_id.to_owned());
        document.tools_id = document
            .tools_id
            .as_deref()
            .map(|id| self.clone_tools(id))
            .transpose()?;
        document.compaction_id = document
            .compaction_id
            .as_deref()
            .map(|id| self.clone_compaction(id))
            .transpose()?;
        self.push(Collection::AgentContext, &document)?;
        Ok(target_id)
    }
}

fn snapshot_document<'a>(
    snapshot: &'a ConfigReferences,
    collection: Collection,
    id: &str,
) -> Option<&'a Value> {
    snapshot
        .documents()
        .find(|((candidate, candidate_id), _)| *candidate == collection && candidate_id == id)
        .map(|(_, value)| value)
}

fn decode_snapshot<T: DeserializeOwned>(
    snapshot: &ConfigReferences,
    collection: Collection,
    id: &str,
) -> Result<Option<T>> {
    snapshot_document(snapshot, collection, id)
        .map(|value| serde_json::from_value(value.clone()).map_err(anyhow::Error::from))
        .transpose()
}

fn current_owned_closure(
    snapshot: &ConfigReferences,
    behavior: &AgentBehavior,
) -> Result<BTreeSet<DocumentKey>> {
    let mut visited = BTreeSet::new();
    let mut pending = Vec::new();
    if let Some(context_id) = &behavior.context_id {
        pending.push((Collection::AgentContext, context_id.clone()));
    }
    pending.push((
        Collection::InferenceProfile,
        behavior.inference_profile_id.clone(),
    ));

    while let Some((collection, id)) = pending.pop() {
        if !visited.insert((collection, id.clone())) {
            continue;
        }
        let Some(value) = snapshot_document(snapshot, collection, &id) else {
            continue;
        };
        match collection {
            Collection::AgentContext => {
                let document: AgentContext = serde_json::from_value(value.clone())?;
                if let Some(id) = document.tools_id {
                    pending.push((Collection::Tools, id));
                }
                if let Some(id) = document.compaction_id {
                    pending.push((Collection::Compaction, id));
                }
            }
            Collection::Compaction => {
                let document: CompactionConfig = serde_json::from_value(value.clone())?;
                if let Some(id) = document.inference_profile_id {
                    pending.push((Collection::InferenceProfile, id));
                }
            }
            Collection::InferenceProfile => {
                let document: InferenceProfile = serde_json::from_value(value.clone())?;
                if let Some(id) = document.sampling_id {
                    pending.push((Collection::InferenceSampling, id));
                }
                if let Some(id) = document.execution_id {
                    pending.push((Collection::InferenceExecution, id));
                }
            }
            Collection::InferenceExecution => {
                let document: InferenceExecution = serde_json::from_value(value.clone())?;
                if let Some(id) = document.retry_policy_id {
                    pending.push((Collection::InferenceRetryPolicy, id));
                }
            }
            Collection::Tools
            | Collection::InferenceSampling
            | Collection::InferenceRetryPolicy => {}
            _ => unreachable!("only behavior-owned collections are traversed"),
        }
    }
    Ok(visited)
}

fn owned_references(collection: Collection, value: &Value) -> Result<Vec<DocumentKey>> {
    let mut references = Vec::new();
    match collection {
        Collection::AgentBehavior => {
            let document: AgentBehavior = serde_json::from_value(value.clone())?;
            if let Some(id) = document.context_id {
                references.push((Collection::AgentContext, id));
            }
            references.push((Collection::InferenceProfile, document.inference_profile_id));
        }
        Collection::AgentContext => {
            let document: AgentContext = serde_json::from_value(value.clone())?;
            if let Some(id) = document.tools_id {
                references.push((Collection::Tools, id));
            }
            if let Some(id) = document.compaction_id {
                references.push((Collection::Compaction, id));
            }
        }
        Collection::Compaction => {
            let document: CompactionConfig = serde_json::from_value(value.clone())?;
            if let Some(id) = document.inference_profile_id {
                references.push((Collection::InferenceProfile, id));
            }
        }
        Collection::InferenceProfile => {
            let document: InferenceProfile = serde_json::from_value(value.clone())?;
            if let Some(id) = document.sampling_id {
                references.push((Collection::InferenceSampling, id));
            }
            if let Some(id) = document.execution_id {
                references.push((Collection::InferenceExecution, id));
            }
        }
        Collection::InferenceExecution => {
            let document: InferenceExecution = serde_json::from_value(value.clone())?;
            if let Some(id) = document.retry_policy_id {
                references.push((Collection::InferenceRetryPolicy, id));
            }
        }
        _ => {}
    }
    Ok(references)
}

fn has_external_consumer(
    snapshot: &ConfigReferences,
    target_behavior_id: &str,
    current_target_closure: &BTreeSet<DocumentKey>,
    target: &DocumentKey,
) -> Result<bool> {
    for ((collection, id), value) in snapshot.documents() {
        if !owned_references(*collection, value)?.contains(target) {
            continue;
        }
        let referrer = (*collection, id.clone());
        let belongs_to_target = (*collection == Collection::AgentBehavior
            && id == target_behavior_id)
            || current_target_closure.contains(&referrer);
        if !belongs_to_target {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Build the complete target replacement from one coherent owner snapshot.
///
/// The source behavior may be an in-memory candidate, but every referenced
/// mutable component must be present in `snapshot`. Existing target-owned
/// component slots may be replaced. An unscoped slot is replaceable only when
/// it is already reachable from the existing target behavior; other collisions
/// are rejected rather than silently taking ownership.
pub fn plan_behavior_closure(
    snapshot: &ConfigReferences,
    source_behavior: &AgentBehavior,
    target_behavior_id: &str,
    target_display_name: &str,
) -> Result<DesiredStateApplyPlan> {
    plan_behavior_closure_with_overlays(
        snapshot,
        source_behavior,
        std::iter::empty(),
        target_behavior_id,
        target_display_name,
    )
}

/// Plan from a coherent retained snapshot plus canonical in-memory source
/// documents. Overlays replace source documents for traversal only; the final
/// output is still a complete desired-state replacement validated by the same
/// plan owner. This supports fresh persona context/tools and pre-publication
/// clone edits without temporarily persisting partial source state.
pub fn plan_behavior_closure_with_overlays(
    snapshot: &ConfigReferences,
    source_behavior: &AgentBehavior,
    source_overlays: impl IntoIterator<Item = (Collection, Value)>,
    target_behavior_id: &str,
    target_display_name: &str,
) -> Result<DesiredStateApplyPlan> {
    ensure!(
        valid_generated_qualified_key(target_behavior_id),
        "target behavior ID {target_behavior_id:?} must use lowercase colon-separated kebab segments"
    );
    ensure!(
        !target_display_name.trim().is_empty(),
        "target behavior display name must not be blank"
    );

    let mut overlays = BTreeMap::new();
    for (collection, value) in source_overlays {
        ensure!(
            OWNED_COLLECTIONS.contains(&collection),
            "source overlay {} is not behavior-owned",
            collection.graphql_type()
        );
        let value = config_projection(collection, Some(&value))?
            .1
            .context("source overlay projection is missing")?;
        ensure!(
            value.get("agent_did").and_then(Value::as_str)
                == Some(source_behavior.agent_did.as_str()),
            "source overlay {} belongs to a different principal",
            collection.graphql_type()
        );
        let id = value
            .get(collection.unique_field())
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .context("source overlay requires a logical ID")?
            .to_owned();
        ensure!(
            overlays.insert((collection, id.clone()), value).is_none(),
            "duplicate source overlay {} {:?}",
            collection.graphql_type(),
            id
        );
    }

    let existing_target: Option<AgentBehavior> =
        decode_snapshot(snapshot, Collection::AgentBehavior, target_behavior_id)?;
    if let Some(existing) = &existing_target {
        ensure!(
            existing.agent_did == source_behavior.agent_did,
            "target behavior belongs to a different principal"
        );
    }
    let current_target_closure = existing_target
        .as_ref()
        .map(|behavior| current_owned_closure(snapshot, behavior))
        .transpose()?
        .unwrap_or_default();

    let mut planner = ClosurePlanner::new(
        snapshot,
        &overlays,
        &source_behavior.agent_did,
        &source_behavior.behavior_id,
        target_behavior_id,
    );
    let inference_profile_id = planner.clone_profile(
        &source_behavior.inference_profile_id,
        PRIMARY_INFERENCE_PATHS,
    )?;
    let context_id = source_behavior
        .context_id
        .as_deref()
        .map(|id| planner.clone_context(id))
        .transpose()?;

    let mut target_behavior = source_behavior.clone();
    target_behavior.behavior_id = target_behavior_id.to_owned();
    target_behavior.display_name = Some(target_display_name.to_owned());
    target_behavior.context_id = context_id;
    target_behavior.inference_profile_id = inference_profile_id;
    target_behavior.created_at = existing_target.and_then(|behavior| behavior.created_at);
    planner.push(Collection::AgentBehavior, &target_behavior)?;

    let planned_keys: BTreeSet<DocumentKey> = planner
        .documents
        .iter()
        .map(|document| {
            let id = document.add[document.collection.unique_field()]
                .as_str()
                .expect("typed materialized document has its logical ID");
            (document.collection, id.to_owned())
        })
        .collect();

    for (collection, id) in planned_keys
        .iter()
        .filter(|(collection, _)| *collection != Collection::AgentBehavior)
    {
        let Some(existing) = snapshot_document(snapshot, *collection, id) else {
            continue;
        };
        let scope = existing.get("scope_behavior_id").and_then(Value::as_str);
        let legacy_target_slot_is_private = scope.is_none()
            && current_target_closure.contains(&(*collection, id.clone()))
            && !has_external_consumer(
                snapshot,
                target_behavior_id,
                &current_target_closure,
                &(*collection, id.clone()),
            )?;
        ensure!(
            scope == Some(target_behavior_id) || legacy_target_slot_is_private,
            "target slot {} {:?} is occupied outside behavior scope {:?}",
            collection.graphql_type(),
            id,
            target_behavior_id
        );
    }

    let removals = snapshot
        .documents()
        .filter(|((collection, id), value)| {
            OWNED_COLLECTIONS.contains(collection)
                && value.get("scope_behavior_id").and_then(Value::as_str)
                    == Some(target_behavior_id)
                && !planned_keys.contains(&(*collection, id.clone()))
        })
        .map(|((collection, id), _)| (*collection, source_behavior.agent_did.clone(), id.clone()))
        .collect();

    DesiredStateApplyPlan::new(planner.documents)?.with_removals(removals)
}

/// Snapshot, plan, and stage one behavior closure inside the caller's
/// transaction. The caller remains the commit/discard and retry owner.
pub async fn materialize_behavior_closure_in_txn(
    txn: &ConfigApplyTxn<'_>,
    agent_did: &str,
    source_behavior_id: &str,
    target_behavior_id: &str,
    target_display_name: &str,
) -> Result<DesiredStateApplyCounts> {
    let snapshot = ConfigReferences::load_in_txn(txn, agent_did).await?;
    let source: AgentBehavior =
        decode_snapshot(&snapshot, Collection::AgentBehavior, source_behavior_id)?
            .with_context(|| format!("source behavior {source_behavior_id:?} does not exist"))?;
    let plan = plan_behavior_closure(&snapshot, &source, target_behavior_id, target_display_name)?;
    apply_desired_state_plan(txn, &plan).await
}

/// Stage an in-memory source behavior and edited component overlays using the
/// same coherent transaction snapshot as persisted cloning.
pub async fn materialize_behavior_closure_candidate_in_txn(
    txn: &ConfigApplyTxn<'_>,
    source_behavior: &AgentBehavior,
    source_overlays: impl IntoIterator<Item = (Collection, Value)>,
    target_behavior_id: &str,
    target_display_name: &str,
) -> Result<DesiredStateApplyCounts> {
    let source_overlays: Vec<_> = source_overlays.into_iter().collect();
    let snapshot = ConfigReferences::load_in_txn(txn, &source_behavior.agent_did).await?;
    let plan = plan_behavior_closure_with_overlays(
        &snapshot,
        source_behavior,
        source_overlays,
        target_behavior_id,
        target_display_name,
    )?;
    apply_desired_state_plan(txn, &plan).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(documents: Vec<(Collection, Value)>) -> ConfigReferences {
        ConfigReferences::from_documents("owner", documents).unwrap()
    }

    fn source_behavior() -> AgentBehavior {
        serde_json::from_value(json!({
            "agent_did": "owner",
            "behavior_id": "legacy-source",
            "display_name": "Source",
            "context_id": "source-context",
            "inference_profile_id": "primary-profile"
        }))
        .unwrap()
    }

    fn full_source_documents() -> Vec<(Collection, Value)> {
        vec![
            (
                Collection::AgentContext,
                json!({
                    "agent_did": "owner",
                    "context_id": "source-context",
                    "tools_id": "source-tools",
                    "compaction_id": "source-compaction",
                    "skill_ids": ["shared-skill"]
                }),
            ),
            (
                Collection::Tools,
                json!({
                    "agent_did": "owner",
                    "tools_id": "source-tools",
                    "remote": {"services": [{"mcp_service_id": "shared-mcp"}]},
                    "subagents": {"target_ids": ["shared-subagent"]},
                    "datastore": {"datastore_tool_surface_ids": ["shared-surface"]},
                    "integrations": {"eth_tool_ids": ["shared-eth"]}
                }),
            ),
            (
                Collection::Compaction,
                json!({
                    "agent_did": "owner",
                    "compaction_id": "source-compaction",
                    "inference_profile_id": "summary-profile"
                }),
            ),
            (
                Collection::InferenceProfile,
                json!({
                    "agent_did": "owner",
                    "profile_id": "primary-profile",
                    "backend_id": "shared-backend",
                    "model_name": "primary-model",
                    "sampling_id": "shared-sampling",
                    "execution_id": "primary-execution"
                }),
            ),
            (
                Collection::InferenceProfile,
                json!({
                    "agent_did": "owner",
                    "profile_id": "summary-profile",
                    "backend_id": "shared-backend",
                    "model_name": "summary-model",
                    "sampling_id": "shared-sampling",
                    "execution_id": "summary-execution"
                }),
            ),
            (
                Collection::InferenceSampling,
                json!({
                    "agent_did": "owner",
                    "sampling_id": "shared-sampling",
                    "temperature": 0.25
                }),
            ),
            (
                Collection::InferenceExecution,
                json!({
                    "agent_did": "owner",
                    "execution_id": "primary-execution",
                    "retry_policy_id": "shared-retry"
                }),
            ),
            (
                Collection::InferenceExecution,
                json!({
                    "agent_did": "owner",
                    "execution_id": "summary-execution",
                    "retry_policy_id": "shared-retry"
                }),
            ),
            (
                Collection::InferenceRetryPolicy,
                json!({
                    "agent_did": "owner",
                    "retry_policy_id": "shared-retry",
                    "max_transport_retries": 2
                }),
            ),
        ]
    }

    fn planned<'a>(plan: &'a DesiredStateApplyPlan, collection: Collection, id: &str) -> &'a Value {
        &plan
            .documents()
            .iter()
            .find(|document| {
                document.collection == collection
                    && document.add[collection.unique_field()].as_str() == Some(id)
            })
            .unwrap_or_else(|| panic!("missing {} {id}", collection.graphql_type()))
            .add
    }

    #[test]
    fn full_closure_uses_deterministic_ids_scope_and_shared_references() {
        let snapshot = snapshot(full_source_documents());
        let plan = plan_behavior_closure(
            &snapshot,
            &source_behavior(),
            "local:reviewer",
            "Jack's reviewer",
        )
        .unwrap();

        let behavior = planned(&plan, Collection::AgentBehavior, "local:reviewer");
        assert_eq!(behavior["display_name"], "Jack's reviewer");
        assert_eq!(behavior["context_id"], "local:reviewer:context");
        assert_eq!(behavior["inference_profile_id"], "local:reviewer:inference");

        let context = planned(&plan, Collection::AgentContext, "local:reviewer:context");
        assert_eq!(context["scope_behavior_id"], "local:reviewer");
        assert_eq!(context["skill_ids"], json!(["shared-skill"]));
        assert_eq!(context["tools_id"], "local:reviewer:tools");
        assert_eq!(context["compaction_id"], "local:reviewer:compaction");

        let tools = planned(&plan, Collection::Tools, "local:reviewer:tools");
        assert_eq!(tools["scope_behavior_id"], "local:reviewer");
        assert_eq!(
            tools["remote"]["services"][0]["mcp_service_id"],
            "shared-mcp"
        );
        assert_eq!(tools["subagents"]["target_ids"], json!(["shared-subagent"]));
        assert_eq!(
            tools["datastore"]["datastore_tool_surface_ids"],
            json!(["shared-surface"])
        );
        assert_eq!(tools["integrations"]["eth_tool_ids"], json!(["shared-eth"]));

        let primary = planned(
            &plan,
            Collection::InferenceProfile,
            "local:reviewer:inference",
        );
        assert_eq!(primary["backend_id"], "shared-backend");
        assert_eq!(primary["sampling_id"], "local:reviewer:sampling");
        assert_eq!(primary["execution_id"], "local:reviewer:execution");

        let summary = planned(
            &plan,
            Collection::InferenceProfile,
            "local:reviewer:compaction:inference",
        );
        assert_eq!(summary["backend_id"], "shared-backend");
        assert_eq!(summary["sampling_id"], "local:reviewer:sampling");
        assert_eq!(
            summary["execution_id"],
            "local:reviewer:compaction:execution"
        );

        let summary_execution = planned(
            &plan,
            Collection::InferenceExecution,
            "local:reviewer:compaction:execution",
        );
        assert_eq!(
            summary_execution["retry_policy_id"],
            "local:reviewer:retry-policy"
        );
        assert!(
            !plan.documents().iter().any(|document| {
                document.collection == Collection::InferenceSampling
                    && document.add["sampling_id"] == "local:reviewer:compaction:sampling"
            }),
            "a shared source sampling document must remain one target alias"
        );
    }

    #[test]
    fn same_summary_profile_preserves_the_primary_profile_alias() {
        let mut documents = full_source_documents();
        documents
            .iter_mut()
            .find(|(collection, value)| {
                *collection == Collection::Compaction
                    && value["compaction_id"] == "source-compaction"
            })
            .unwrap()
            .1["inference_profile_id"] = json!("primary-profile");
        let plan = plan_behavior_closure(
            &snapshot(documents),
            &source_behavior(),
            "local:alias",
            "Alias",
        )
        .unwrap();

        let compaction = planned(&plan, Collection::Compaction, "local:alias:compaction");
        assert_eq!(compaction["inference_profile_id"], "local:alias:inference");
        assert_eq!(
            plan.documents()
                .iter()
                .filter(|document| document.collection == Collection::InferenceProfile)
                .count(),
            1
        );
    }

    #[test]
    fn optional_documents_remain_absent() {
        let behavior: AgentBehavior = serde_json::from_value(json!({
            "agent_did": "owner",
            "behavior_id": "source",
            "inference_profile_id": "minimal-profile"
        }))
        .unwrap();
        let snapshot = snapshot(vec![(
            Collection::InferenceProfile,
            json!({
                "agent_did": "owner",
                "profile_id": "minimal-profile",
                "backend_id": "shared-backend",
                "model_name": "model"
            }),
        )]);
        let plan = plan_behavior_closure(&snapshot, &behavior, "local:minimal", "Minimal").unwrap();

        assert_eq!(plan.documents().len(), 2);
        let target = planned(&plan, Collection::AgentBehavior, "local:minimal");
        assert!(target["context_id"].is_null());
        let profile = planned(
            &plan,
            Collection::InferenceProfile,
            "local:minimal:inference",
        );
        assert!(profile["sampling_id"].is_null());
        assert!(profile["execution_id"].is_null());
    }

    #[test]
    fn stale_target_scoped_descendants_are_removed() {
        let mut documents = full_source_documents();
        documents.push((
            Collection::InferenceSampling,
            json!({
                "agent_did": "owner",
                "sampling_id": "local:reviewer:compaction:sampling",
                "scope_behavior_id": "local:reviewer"
            }),
        ));
        let plan = plan_behavior_closure(
            &snapshot(documents),
            &source_behavior(),
            "local:reviewer",
            "Reviewer",
        )
        .unwrap();

        assert!(plan.removals().contains(&(
            Collection::InferenceSampling,
            "owner".to_owned(),
            "local:reviewer:compaction:sampling".to_owned()
        )));
    }

    #[test]
    fn unrelated_target_slot_collision_is_rejected() {
        let mut documents = full_source_documents();
        documents.push((
            Collection::InferenceSampling,
            json!({
                "agent_did": "owner",
                "sampling_id": "local:reviewer:sampling"
            }),
        ));
        let error = plan_behavior_closure(
            &snapshot(documents),
            &source_behavior(),
            "local:reviewer",
            "Reviewer",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("occupied outside behavior scope"), "{error}");
    }

    #[test]
    fn legacy_target_slot_shared_by_a_sibling_is_not_claimed() {
        let mut documents = full_source_documents();
        documents.extend([
            (
                Collection::AgentBehavior,
                json!({
                    "agent_did": "owner",
                    "behavior_id": "local:reviewer",
                    "inference_profile_id": "local:reviewer:inference"
                }),
            ),
            (
                Collection::AgentBehavior,
                json!({
                    "agent_did": "owner",
                    "behavior_id": "legacy-sibling",
                    "inference_profile_id": "local:reviewer:inference"
                }),
            ),
            (
                Collection::InferenceProfile,
                json!({
                    "agent_did": "owner",
                    "profile_id": "local:reviewer:inference",
                    "backend_id": "shared-backend",
                    "model_name": "legacy-model"
                }),
            ),
        ]);
        let error = plan_behavior_closure(
            &snapshot(documents),
            &source_behavior(),
            "local:reviewer",
            "Reviewer",
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("occupied outside behavior scope"), "{error}");
    }

    #[test]
    fn overlay_supports_fresh_context_edits_with_a_snapshot_profile() {
        let behavior: AgentBehavior = serde_json::from_value(json!({
            "agent_did": "owner",
            "behavior_id": "draft",
            "context_id": "draft-context",
            "inference_profile_id": "existing-profile"
        }))
        .unwrap();
        let snapshot = snapshot(vec![(
            Collection::InferenceProfile,
            json!({
                "agent_did": "owner",
                "profile_id": "existing-profile",
                "backend_id": "shared-backend",
                "model_name": "model"
            }),
        )]);
        let overlays = vec![
            (
                Collection::AgentContext,
                json!({
                    "agent_did": "owner",
                    "context_id": "draft-context",
                    "system_prompt": "edited before publication",
                    "tools_id": "draft-tools"
                }),
            ),
            (
                Collection::Tools,
                json!({
                    "agent_did": "owner",
                    "tools_id": "draft-tools",
                    "host": {"root": "/edited/root"}
                }),
            ),
        ];
        let plan = plan_behavior_closure_with_overlays(
            &snapshot,
            &behavior,
            overlays,
            "local:edited",
            "Edited",
        )
        .unwrap();

        let context = planned(&plan, Collection::AgentContext, "local:edited:context");
        assert_eq!(context["system_prompt"], "edited before publication");
        assert_eq!(context["tools_id"], "local:edited:tools");
        let tools = planned(&plan, Collection::Tools, "local:edited:tools");
        assert_eq!(tools["host"]["root"], "/edited/root");
        let profile = planned(
            &plan,
            Collection::InferenceProfile,
            "local:edited:inference",
        );
        assert_eq!(profile["backend_id"], "shared-backend");
    }
}
