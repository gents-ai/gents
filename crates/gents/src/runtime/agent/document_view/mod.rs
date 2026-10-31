mod apply;
mod automation;
mod load;
mod snapshot;

pub(crate) use apply::{apply_control_update, control_collection_names};
pub(crate) use load::load_document_runtime_view;
pub(crate) use snapshot::resolve_document_runtime_snapshot_from_view;

use crate::documents::*;
use crate::inference::auth::credential::OAuthCredential;
use rapidhash::RapidHashMap;

/// Ids in these namespaces were minted only by the retired graph publisher and
/// are never authored; a stored document carrying one stays inert forever.
pub(crate) fn is_retired_reserved_id(id: &str) -> bool {
    id.starts_with("graph-trigger-") || id.starts_with("pkg-")
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentRecord<T> {
    pub(crate) doc_id: String,
    pub(crate) value: T,
}

#[derive(Debug, Clone)]
pub(crate) struct DocumentRuntimeView {
    pub(crate) node: DocumentRecord<Node>,
    pub(crate) agents: RapidHashMap<String, DocumentRecord<Agent>>,
    pub(crate) contexts: RapidHashMap<String, DocumentRecord<AgentContext>>,
    pub(crate) compactions: RapidHashMap<String, DocumentRecord<CompactionConfig>>,
    pub(crate) tools: RapidHashMap<String, DocumentRecord<Tools>>,
    pub(crate) agent_targets: RapidHashMap<String, DocumentRecord<AgentTargetDocument>>,
    pub(crate) skills: RapidHashMap<String, DocumentRecord<SkillDocument>>,
    pub(crate) datastore_tool_surfaces:
        RapidHashMap<String, DocumentRecord<DatastoreToolSurfaceDocument>>,
    pub(crate) inference_profiles: RapidHashMap<String, DocumentRecord<InferenceProfile>>,
    pub(crate) inference_sampling: RapidHashMap<String, DocumentRecord<InferenceSampling>>,
    pub(crate) inference_execution: RapidHashMap<String, DocumentRecord<InferenceExecution>>,
    pub(crate) inference_retry_policies: RapidHashMap<String, DocumentRecord<InferenceRetryPolicy>>,
    pub(crate) backends: RapidHashMap<String, DocumentRecord<InferenceBackend>>,
    pub(crate) tasks: RapidHashMap<String, DocumentRecord<Task>>,
    pub(crate) schedules: RapidHashMap<String, DocumentRecord<Schedule>>,
    pub(crate) triggers: RapidHashMap<String, DocumentRecord<Trigger>>,
    pub(crate) event_sources: RapidHashMap<String, DocumentRecord<EventSource>>,
    pub(crate) tool_services: RapidHashMap<String, DocumentRecord<ToolServiceRegistry>>,
    pub(crate) backend_observations: RapidHashMap<String, InferenceBackendObservation>,
    pub(crate) unknown_kind_backends: RapidHashMap<String, String>,
    pub(crate) oauth_credentials: RapidHashMap<String, DocumentRecord<OAuthCredential>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlUpdateOutcome {
    Irrelevant,
    FullReload,
}

impl DocumentRuntimeView {
    pub(super) fn resolves_oauth_credential(
        &self,
        provider: &str,
        pick: crate::inference::auth::credential::AccountPick<'_>,
    ) -> bool {
        crate::inference::auth::credential::pick_oauth_credential(
            self.oauth_credentials.values().map(|record| &record.value),
            &self.node.value.node_did,
            provider,
            pick,
        )
        .is_some()
    }

    pub(crate) fn has_unresolved_agent_references(&self) -> bool {
        !self.pending_visibility_details().is_empty()
    }

    pub(crate) fn pending_visibility_details(&self) -> Vec<String> {
        let mut details = Vec::new();
        if let Some(id) = self.node.value.default_agent_id.as_deref() {
            if !self.agents.contains_key(id) {
                details.push(format!(
                    "principal {} references missing default agent {id}",
                    self.node.value.node_did
                ));
            }
        }
        for record in self.agents.values() {
            snapshot::collect_unresolved_behavior_references(self, &record.value, &mut details);
        }
        details.sort();
        details
    }
}

#[cfg(test)]
mod tests;

pub(crate) fn merge_surface_tools(
    tools: &Tools,
    view: &DocumentRuntimeView,
) -> anyhow::Result<crate::documents::MergedSurfaceTools> {
    crate::documents::merge_datastore_tool_surfaces(
        tools,
        view.datastore_tool_surfaces
            .values()
            .map(|record| &record.value),
    )
}
