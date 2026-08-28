mod apply;
mod load;
mod snapshot;

pub(crate) use apply::apply_control_update;
pub(crate) use load::load_document_runtime_view;
pub(crate) use snapshot::resolve_document_runtime_snapshot_from_view;

use std::collections::{BTreeSet, HashMap};

use crate::backend_registry::InferenceBackend;
use crate::chatgpt_codex::OAuthCredential;
use crate::document_config::{
    AgentBehavior, AgentPrincipal, DatastoreToolSurfaceDocument, EthToolDocument, EventTrigger,
    GraphDefinition, GraphRunPin, InferenceProfile, Schedule, SkillDocument, Task,
    ToolSelectionDocument,
};

#[derive(Debug, Clone)]
pub(crate) struct DocumentRecord<T> {
    pub(crate) doc_id: String,
    pub(crate) value: T,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentRuntimeView {
    pub(crate) principal: DocumentRecord<AgentPrincipal>,
    pub(crate) behaviors: HashMap<String, DocumentRecord<AgentBehavior>>,
    pub(crate) skills: HashMap<String, DocumentRecord<SkillDocument>>,
    pub(crate) datastore_tool_surfaces:
        HashMap<String, DocumentRecord<DatastoreToolSurfaceDocument>>,
    pub(crate) eth_tools: HashMap<String, DocumentRecord<EthToolDocument>>,
    pub(crate) tool_selections: HashMap<String, DocumentRecord<ToolSelectionDocument>>,
    pub(crate) inference_profiles: HashMap<String, DocumentRecord<InferenceProfile>>,
    pub(crate) backends: HashMap<String, DocumentRecord<InferenceBackend>>,
    pub(crate) oauth_credentials: HashMap<String, DocumentRecord<OAuthCredential>>,
    pub(crate) tasks: HashMap<String, DocumentRecord<Task>>,
    pub(crate) schedules: HashMap<String, DocumentRecord<Schedule>>,
    pub(crate) event_triggers: HashMap<String, DocumentRecord<EventTrigger>>,
    pub(crate) graph_definitions: HashMap<String, DocumentRecord<GraphDefinition>>,
    pub(crate) graph_run_pins: HashMap<String, DocumentRecord<GraphRunPin>>,
    /// Package-owned behavior/selection/surface ids admitted by an active or
    /// nonterminal-run-pinned immutable revision for this principal.
    pub(crate) visible_graph_package_artifact_ids: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlUpdateOutcome {
    Irrelevant,
    Applied,
    PendingVisibility,
    FullReload,
}

impl DocumentRuntimeView {
    fn has_package_config_artifact_doc_id(&self, doc_id: &str) -> bool {
        self.behaviors
            .iter()
            .any(|(id, record)| id.starts_with("pkg-") && record.doc_id == doc_id)
            || self
                .tool_selections
                .iter()
                .any(|(id, record)| id.starts_with("pkg-") && record.doc_id == doc_id)
            || self
                .datastore_tool_surfaces
                .iter()
                .any(|(id, record)| id.starts_with("pkg-") && record.doc_id == doc_id)
            || self
                .tasks
                .iter()
                .any(|(id, record)| id.starts_with("pkg-") && record.doc_id == doc_id)
    }

    fn has_behavior_doc_id(&self, doc_id: &str) -> bool {
        self.behaviors
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_tool_selection_doc_id(&self, doc_id: &str) -> bool {
        self.tool_selections
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_skill_doc_id(&self, doc_id: &str) -> bool {
        self.skills.values().any(|record| record.doc_id == doc_id)
    }

    fn has_datastore_tool_surface_doc_id(&self, doc_id: &str) -> bool {
        self.datastore_tool_surfaces
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_eth_tool_doc_id(&self, doc_id: &str) -> bool {
        self.eth_tools
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_inference_profile_doc_id(&self, doc_id: &str) -> bool {
        self.inference_profiles
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_backend_doc_id(&self, doc_id: &str) -> bool {
        self.backends.values().any(|record| record.doc_id == doc_id)
    }

    fn has_task_doc_id(&self, doc_id: &str) -> bool {
        self.tasks.values().any(|record| record.doc_id == doc_id)
    }

    fn has_schedule_doc_id(&self, doc_id: &str) -> bool {
        self.schedules
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_event_trigger_doc_id(&self, doc_id: &str) -> bool {
        self.event_triggers
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_graph_definition_doc_id(&self, doc_id: &str) -> bool {
        self.graph_definitions
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn has_graph_run_doc_id(&self, doc_id: &str) -> bool {
        self.graph_run_pins
            .values()
            .any(|record| record.doc_id == doc_id)
    }
    fn remove_behavior_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self.behaviors.iter().find_map(|(behavior_id, record)| {
            (record.doc_id == doc_id).then_some(behavior_id.clone())
        });
        key.is_some_and(|behavior_id| self.behaviors.remove(&behavior_id).is_some())
    }

    fn remove_tool_selection_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .tool_selections
            .iter()
            .find_map(|(selection_id, record)| {
                (record.doc_id == doc_id).then_some(selection_id.clone())
            });
        key.is_some_and(|selection_id| self.tool_selections.remove(&selection_id).is_some())
    }

    fn remove_skill_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .skills
            .iter()
            .find_map(|(skill_id, record)| (record.doc_id == doc_id).then_some(skill_id.clone()));
        key.is_some_and(|skill_id| self.skills.remove(&skill_id).is_some())
    }

    fn remove_datastore_tool_surface_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .datastore_tool_surfaces
            .iter()
            .find_map(|(surface_id, record)| {
                (record.doc_id == doc_id).then_some(surface_id.clone())
            });
        key.is_some_and(|surface_id| self.datastore_tool_surfaces.remove(&surface_id).is_some())
    }

    fn remove_eth_tool_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .eth_tools
            .iter()
            .find_map(|(tool_id, record)| (record.doc_id == doc_id).then_some(tool_id.clone()));
        key.is_some_and(|tool_id| self.eth_tools.remove(&tool_id).is_some())
    }

    fn remove_inference_profile_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .inference_profiles
            .iter()
            .find_map(|(profile_id, record)| {
                (record.doc_id == doc_id).then_some(profile_id.clone())
            });
        key.is_some_and(|profile_id| self.inference_profiles.remove(&profile_id).is_some())
    }

    fn remove_backend_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self.backends.iter().find_map(|(backend_id, record)| {
            (record.doc_id == doc_id).then_some(backend_id.clone())
        });
        key.is_some_and(|backend_id| self.backends.remove(&backend_id).is_some())
    }

    fn has_oauth_credential_doc_id(&self, doc_id: &str) -> bool {
        self.oauth_credentials
            .values()
            .any(|record| record.doc_id == doc_id)
    }

    fn remove_oauth_credential_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .oauth_credentials
            .iter()
            .find_map(|(credential_id, record)| {
                (record.doc_id == doc_id).then_some(credential_id.clone())
            });
        key.is_some_and(|credential_id| self.oauth_credentials.remove(&credential_id).is_some())
    }

    pub(super) fn has_enabled_oauth_credential(&self, provider: &str) -> bool {
        self.oauth_credentials
            .values()
            .any(|record| record.value.provider == provider && record.value.enabled)
    }

    fn remove_task_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self
            .tasks
            .iter()
            .find_map(|(task_id, record)| (record.doc_id == doc_id).then_some(task_id.clone()));
        key.is_some_and(|task_id| self.tasks.remove(&task_id).is_some())
    }

    fn remove_schedule_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self.schedules.iter().find_map(|(schedule_id, record)| {
            (record.doc_id == doc_id).then_some(schedule_id.clone())
        });
        key.is_some_and(|schedule_id| self.schedules.remove(&schedule_id).is_some())
    }

    fn remove_event_trigger_by_doc_id(&mut self, doc_id: &str) -> bool {
        let key = self.event_triggers.iter().find_map(|(trigger_id, record)| {
            (record.doc_id == doc_id).then_some(trigger_id.clone())
        });
        key.is_some_and(|trigger_id| self.event_triggers.remove(&trigger_id).is_some())
    }

    fn references_profile(&self, profile_id: &str) -> bool {
        self.behaviors.values().any(|record| {
            record
                .value
                .inference_profile_id
                .as_deref()
                .is_some_and(|id| id == profile_id)
        })
    }

    fn references_backend(&self, backend_id: &str) -> bool {
        self.behaviors.values().any(|record| {
            record
                .value
                .backend_id
                .as_deref()
                .is_some_and(|id| id == backend_id)
        })
    }

    pub(crate) fn has_unresolved_behavior_references(&self) -> bool {
        !self.pending_visibility_details().is_empty()
    }

    pub(crate) fn pending_visibility_details(&self) -> Vec<String> {
        let mut details = Vec::new();

        if let Some(default_behavior_id) = self
            .principal
            .value
            .default_behavior_id
            .as_deref()
            .and_then(snapshot::non_empty)
        {
            if !self.behaviors.contains_key(default_behavior_id) {
                details.push(format!(
                    "principal {} references missing default behavior {}",
                    self.principal.value.agent_did, default_behavior_id
                ));
            }
        }

        for record in self.behaviors.values() {
            snapshot::collect_unresolved_behavior_references(self, &record.value, &mut details);
        }

        details.sort();
        details
    }
}

fn active_graph_revision_pins(view: &DocumentRuntimeView) -> (BTreeSet<String>, BTreeSet<String>) {
    let agent_did = view.principal.value.agent_did.as_str();
    let active = view
        .graph_definitions
        .values()
        .filter(|record| record.value.owner_did == agent_did && record.value.enabled)
        .filter_map(|record| record.value.active_revision_digest.clone())
        .collect();
    let pinned = view
        .graph_run_pins
        .values()
        .filter(|record| record.value.owner_did == agent_did && !record.value.is_terminal())
        .map(|record| record.value.revision_digest.clone())
        .collect();
    (active, pinned)
}

fn package_config_artifact_is_visible(view: &DocumentRuntimeView, id: &str) -> bool {
    !id.starts_with("pkg-") || view.visible_graph_package_artifact_ids.contains(id)
}
fn validate_subagent_targets_resolve(
    selection: &ToolSelectionDocument,
    view: &DocumentRuntimeView,
) -> anyhow::Result<()> {
    let own_agent_did = view.principal.value.agent_did.as_str();
    for entry in selection.subagent_targets.iter().flatten() {
        let target = crate::document_config::SubagentTarget::parse(entry).map_err(|error| {
            anyhow::anyhow!(
                "ToolSelection {} subagent_targets entry {entry:?} is not a valid SubagentTarget JSON: {error}",
                selection.selection_id,
            )
        })?;
        if !target.is_structurally_valid() {
            anyhow::bail!(
                "ToolSelection {} subagent_targets entry {entry:?} has empty name/agent_did/behavior_id",
                selection.selection_id,
            );
        }
        if target.agent_did == own_agent_did && !view.behaviors.contains_key(&target.behavior_id) {
            anyhow::bail!(
                "ToolSelection {} subagent_targets entry {entry:?} names a local behavior that does not resolve to an AgentBehavior",
                selection.selection_id,
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// Merge inline `write_tools` with entries from linked `DatastoreToolSurface`
/// docs. Fail-closed on missing/disabled/foreign surfaces and name collisions.
pub(crate) fn merge_surface_tools(
    selection: &ToolSelectionDocument,
    view: &DocumentRuntimeView,
) -> anyhow::Result<crate::document_config::MergedSurfaceTools> {
    crate::document_config::merge_datastore_tool_surfaces(
        selection,
        view.datastore_tool_surfaces
            .values()
            .map(|record| &record.value),
    )
}

/// Expand `eth_tool_ids` into query tools. Missing / foreign ids fail closed.
/// Disabled EthTools and empty `query_methods` are skipped (no advertise).
pub(crate) fn expand_eth_tools(
    selection: &ToolSelectionDocument,
    view: &DocumentRuntimeView,
) -> anyhow::Result<Vec<crate::eth::ResolvedEthQuery>> {
    use anyhow::{anyhow, bail};
    use std::collections::HashSet;

    let mut out = Vec::new();
    let mut linked_ids: HashSet<&str> = HashSet::new();
    let mut tool_names: HashSet<String> = HashSet::new();
    for tool_id in selection.eth_tool_ids.as_deref().unwrap_or(&[]) {
        let tool_id = tool_id.trim();
        if tool_id.is_empty() {
            bail!(
                "ToolSelection {} has an empty eth_tool_ids entry",
                selection.selection_id
            );
        }
        if !linked_ids.insert(tool_id) {
            bail!(
                "ToolSelection {} lists EthTool {} more than once",
                selection.selection_id,
                tool_id
            );
        }
        let record = view.eth_tools.get(tool_id).ok_or_else(|| {
            anyhow!(
                "ToolSelection {} references missing EthTool {}",
                selection.selection_id,
                tool_id
            )
        })?;
        let doc = &record.value;
        if doc.agent_did.trim() != selection.agent_did.trim() {
            bail!(
                "ToolSelection {} references EthTool {} owned by a different agent",
                selection.selection_id,
                tool_id
            );
        }
        let Some(resolved) = crate::eth::ResolvedEthQuery::from_document(doc)? else {
            continue;
        };
        if !tool_names.insert(resolved.tool_name()) {
            bail!(
                "duplicate eth tool name {:?} after expanding EthTool {} for ToolSelection {}",
                resolved.tool_name(),
                tool_id,
                selection.selection_id
            );
        }
        out.push(resolved);
    }
    Ok(out)
}
