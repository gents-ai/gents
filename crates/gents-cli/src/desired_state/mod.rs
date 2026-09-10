pub(crate) mod apply_bundle;
pub(crate) mod convert;
pub(crate) mod diff;
pub(crate) mod interpolate;
pub(crate) mod load;
pub(crate) mod normalize;
pub(crate) mod provision;
pub(crate) mod prune;
#[cfg(test)]
mod tests;
pub(crate) mod validate;
pub(crate) mod write;

pub(crate) use apply_bundle::DesiredApplyBundle;
pub(crate) use convert::{
    export_bundle_from_manifest, manifest_from_export_bundle,
    normalize_tool_service_registry_storage_fields,
};
pub(crate) use diff::diff_manifests;
pub(crate) use load::load_manifest_root;
pub(crate) use provision::apply_workspace_provisioning;
pub(crate) use write::write_manifest_root;

mod document_handle;
pub(crate) use document_handle::document_handle;

use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};

use gents::{BackendProviderKind, Collection};

fn deserialize_present_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

pub(crate) const TOOL_SERVICE_ADDRESS_FIELDS: &[&str] = &["hostname", "tailscale_ip", "lan_ip"];
// One canonical configuration model for CLI, packs, graph installation, and
// runtime resolution. Legacy readers/writers below migrate in the implementation
// phase; these aliases do not preserve old fields or old pack formats.
pub(crate) use gents::document_config::{
    AgentBehavior as DesiredAgentBehavior, AgentPrincipal as DesiredAgentPrincipal,
    CallbackBinding as DesiredCallbackBinding, ChainKeyBindingDocument as DesiredChainKeyBinding,
    DatastoreToolSurfaceDocument as DesiredDatastoreToolSurface, EthToolDocument as DesiredEthTool,
    EventSource as DesiredEventTrigger, InferenceBackend as DesiredInferenceBackend,
    InferenceProfile as DesiredInferenceProfile, PackConfig as DesiredStateManifest,
    ProjectionAcpBinding as DesiredProjectionAcpBinding,
    RepositoryPlacement as DesiredRepositoryPlacement, Schedule as DesiredSchedule,
    SkillDocument as DesiredSkill, Task as DesiredTask,
    ToolServiceRegistry as DesiredToolServiceRegistry, Tools as DesiredToolSelection,
};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateCollectionDiff {
    pub(crate) create: Vec<String>,
    pub(crate) update: Vec<String>,
    pub(crate) delete: Vec<String>,
    pub(crate) unchanged: Vec<String>,
    pub(crate) live_only: Vec<String>,
}

impl DesiredStateCollectionDiff {
    pub(super) fn counts(&self) -> DesiredStateDiffCounts {
        DesiredStateDiffCounts {
            create: self.create.len(),
            update: self.update.len(),
            delete: self.delete.len(),
            unchanged: self.unchanged.len(),
            live_only: self.live_only.len(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffCounts {
    pub(crate) create: usize,
    pub(crate) update: usize,
    pub(crate) delete: usize,
    pub(crate) unchanged: usize,
    pub(crate) live_only: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffCollections {
    pub(crate) agent_principal: DesiredStateCollectionDiff,
    pub(crate) agent_behaviors: DesiredStateCollectionDiff,
    pub(crate) skills: DesiredStateCollectionDiff,
    pub(crate) datastore_tool_surfaces: DesiredStateCollectionDiff,
    pub(crate) chain_key_bindings: DesiredStateCollectionDiff,
    pub(crate) eth_tools: DesiredStateCollectionDiff,
    // WorkspaceRoot has no desired-state file/GraphQL wiring yet (not part
    // of Collection::ALL) — always empty until that CRUD surface lands.
    pub(crate) workspace_roots: DesiredStateCollectionDiff,
    pub(crate) tool_selections: DesiredStateCollectionDiff,
    pub(crate) inference_backends: DesiredStateCollectionDiff,
    pub(crate) inference_profiles: DesiredStateCollectionDiff,
    pub(crate) tool_service_registries: DesiredStateCollectionDiff,
    pub(crate) projection_acp_bindings: DesiredStateCollectionDiff,
    pub(crate) tasks: DesiredStateCollectionDiff,
    pub(crate) schedules: DesiredStateCollectionDiff,
    pub(crate) event_triggers: DesiredStateCollectionDiff,
}

impl DesiredStateDiffCollections {
    pub(crate) fn get(&self, collection: Collection) -> &DesiredStateCollectionDiff {
        match collection {
            Collection::AgentPrincipal => &self.agent_principal,
            Collection::AgentBehavior => &self.agent_behaviors,
            Collection::Skill => &self.skills,
            Collection::DatastoreToolSurface => &self.datastore_tool_surfaces,
            Collection::ChainKeyBinding => &self.chain_key_bindings,
            Collection::EthTool => &self.eth_tools,
            Collection::WorkspaceRoot => &self.workspace_roots,
            Collection::ToolSelection => &self.tool_selections,
            Collection::InferenceBackend => &self.inference_backends,
            Collection::InferenceProfile => &self.inference_profiles,
            Collection::ToolServiceRegistry => &self.tool_service_registries,
            Collection::ProjectionAcpBinding => &self.projection_acp_bindings,
            Collection::Task => &self.tasks,
            Collection::Schedule => &self.schedules,
            Collection::EventTrigger => &self.event_triggers,
        }
    }

    fn get_mut(&mut self, collection: Collection) -> &mut DesiredStateCollectionDiff {
        match collection {
            Collection::AgentPrincipal => &mut self.agent_principal,
            Collection::AgentBehavior => &mut self.agent_behaviors,
            Collection::Skill => &mut self.skills,
            Collection::DatastoreToolSurface => &mut self.datastore_tool_surfaces,
            Collection::ChainKeyBinding => &mut self.chain_key_bindings,
            Collection::EthTool => &mut self.eth_tools,
            Collection::WorkspaceRoot => &mut self.workspace_roots,
            Collection::ToolSelection => &mut self.tool_selections,
            Collection::InferenceBackend => &mut self.inference_backends,
            Collection::InferenceProfile => &mut self.inference_profiles,
            Collection::ToolServiceRegistry => &mut self.tool_service_registries,
            Collection::ProjectionAcpBinding => &mut self.projection_acp_bindings,
            Collection::Task => &mut self.tasks,
            Collection::Schedule => &mut self.schedules,
            Collection::EventTrigger => &mut self.event_triggers,
        }
    }

    pub(crate) fn record_prune_deletes(&mut self, deletes: &[gents::apply_model::DocRef]) {
        for doc in deletes {
            let diff = self.get_mut(doc.collection);
            diff.live_only.retain(|id| id != &doc.id);
            if !diff.delete.contains(&doc.id) {
                diff.delete.push(doc.id.clone());
            }
        }
    }

    pub(crate) fn counts(&self) -> DesiredStateDiffCollectionsCounts {
        DesiredStateDiffCollectionsCounts {
            agent_principal: self.agent_principal.counts(),
            agent_behaviors: self.agent_behaviors.counts(),
            skills: self.skills.counts(),
            datastore_tool_surfaces: self.datastore_tool_surfaces.counts(),
            chain_key_bindings: self.chain_key_bindings.counts(),
            eth_tools: self.eth_tools.counts(),
            workspace_roots: self.workspace_roots.counts(),
            tool_selections: self.tool_selections.counts(),
            inference_backends: self.inference_backends.counts(),
            inference_profiles: self.inference_profiles.counts(),
            tool_service_registries: self.tool_service_registries.counts(),
            projection_acp_bindings: self.projection_acp_bindings.counts(),
            tasks: self.tasks.counts(),
            schedules: self.schedules.counts(),
            event_triggers: self.event_triggers.counts(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffCollectionsCounts {
    pub(crate) agent_principal: DesiredStateDiffCounts,
    pub(crate) agent_behaviors: DesiredStateDiffCounts,
    pub(crate) skills: DesiredStateDiffCounts,
    pub(crate) datastore_tool_surfaces: DesiredStateDiffCounts,
    pub(crate) chain_key_bindings: DesiredStateDiffCounts,
    pub(crate) eth_tools: DesiredStateDiffCounts,
    pub(crate) workspace_roots: DesiredStateDiffCounts,
    pub(crate) tool_selections: DesiredStateDiffCounts,
    pub(crate) inference_backends: DesiredStateDiffCounts,
    pub(crate) inference_profiles: DesiredStateDiffCounts,
    pub(crate) tool_service_registries: DesiredStateDiffCounts,
    pub(crate) projection_acp_bindings: DesiredStateDiffCounts,
    pub(crate) tasks: DesiredStateDiffCounts,
    pub(crate) schedules: DesiredStateDiffCounts,
    pub(crate) event_triggers: DesiredStateDiffCounts,
}

impl DesiredStateDiffCollectionsCounts {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &DesiredStateDiffCounts> {
        Collection::ALL
            .iter()
            .copied()
            .map(|collection| self.get(collection))
    }

    pub(crate) fn get(&self, collection: Collection) -> &DesiredStateDiffCounts {
        match collection {
            Collection::AgentPrincipal => &self.agent_principal,
            Collection::AgentBehavior => &self.agent_behaviors,
            Collection::Skill => &self.skills,
            Collection::DatastoreToolSurface => &self.datastore_tool_surfaces,
            Collection::ChainKeyBinding => &self.chain_key_bindings,
            Collection::EthTool => &self.eth_tools,
            Collection::WorkspaceRoot => &self.workspace_roots,
            Collection::ToolSelection => &self.tool_selections,
            Collection::InferenceBackend => &self.inference_backends,
            Collection::InferenceProfile => &self.inference_profiles,
            Collection::ToolServiceRegistry => &self.tool_service_registries,
            Collection::ProjectionAcpBinding => &self.projection_acp_bindings,
            Collection::Task => &self.tasks,
            Collection::Schedule => &self.schedules,
            Collection::EventTrigger => &self.event_triggers,
        }
    }

    pub(crate) fn is_exact_match(&self) -> bool {
        self.iter().all(|count| {
            count.create == 0 && count.update == 0 && count.delete == 0 && count.live_only == 0
        })
    }

    pub(crate) fn has_pending_apply(&self) -> bool {
        self.iter()
            .any(|count| count.create > 0 || count.update > 0 || count.delete > 0)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateDiffReport {
    pub(crate) status: &'static str,
    pub(crate) ok: bool,
    pub(crate) root: String,
    pub(crate) access_mode: String,
    pub(crate) agent_did: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) live_validation_errors: Vec<String>,
    pub(crate) counts: DesiredStateDiffCollectionsCounts,
    pub(crate) collections: DesiredStateDiffCollections,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateCounts {
    pub(crate) agent_principal: usize,
    pub(crate) agent_behaviors: usize,
    pub(crate) skills: usize,
    pub(crate) datastore_tool_surfaces: usize,
    pub(crate) chain_key_bindings: usize,
    pub(crate) eth_tools: usize,
    pub(crate) tool_selections: usize,
    pub(crate) inference_backends: usize,
    pub(crate) inference_profiles: usize,
    pub(crate) tool_service_registries: usize,
    pub(crate) projection_acp_bindings: usize,
    pub(crate) tasks: usize,
    pub(crate) schedules: usize,
    pub(crate) event_triggers: usize,
    pub(crate) callback_bindings: usize,
    pub(crate) repository_placements: usize,
}

impl DesiredStateCounts {
    pub(crate) fn empty() -> Self {
        Self {
            agent_principal: 0,
            agent_behaviors: 0,
            skills: 0,
            datastore_tool_surfaces: 0,
            chain_key_bindings: 0,
            eth_tools: 0,
            tool_selections: 0,
            inference_backends: 0,
            inference_profiles: 0,
            tool_service_registries: 0,
            projection_acp_bindings: 0,
            tasks: 0,
            schedules: 0,
            event_triggers: 0,
            callback_bindings: 0,
            repository_placements: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DesiredStateValidationReport {
    pub(crate) status: &'static str,
    pub(crate) ok: bool,
    pub(crate) root: String,
    pub(crate) agent_did: Option<String>,
    pub(crate) counts: DesiredStateCounts,
    pub(crate) errors: Vec<String>,
}

impl DesiredStateValidationReport {
    pub(crate) fn is_ok(&self) -> bool {
        self.ok
    }
}

use gents::DesiredFields;

impl DesiredFields for DesiredAgentPrincipal {
    fn collection_tag(&self) -> &'static str {
        "agent_principal"
    }
}
impl DesiredFields for DesiredAgentBehavior {
    fn collection_tag(&self) -> &'static str {
        "agent_behaviors"
    }
}
impl DesiredFields for DesiredToolSelection {
    fn collection_tag(&self) -> &'static str {
        "tool_selections"
    }
}
impl DesiredFields for DesiredSkill {
    fn collection_tag(&self) -> &'static str {
        "skills"
    }
}
impl DesiredFields for DesiredDatastoreToolSurface {
    fn collection_tag(&self) -> &'static str {
        "datastore_tool_surfaces"
    }
}
impl DesiredFields for DesiredInferenceBackend {
    fn collection_tag(&self) -> &'static str {
        "inference_backends"
    }
}
impl DesiredFields for DesiredInferenceProfile {
    fn collection_tag(&self) -> &'static str {
        "inference_profiles"
    }
}
impl DesiredFields for DesiredToolServiceRegistry {
    fn collection_tag(&self) -> &'static str {
        "tool_service_registries"
    }
}
impl DesiredFields for DesiredProjectionAcpBinding {
    fn collection_tag(&self) -> &'static str {
        "projection_acp_bindings"
    }
}
impl DesiredFields for DesiredTask {
    fn collection_tag(&self) -> &'static str {
        "tasks"
    }
}
impl DesiredFields for DesiredSchedule {
    fn collection_tag(&self) -> &'static str {
        "schedules"
    }
}
impl DesiredFields for DesiredEventTrigger {
    fn collection_tag(&self) -> &'static str {
        "event_triggers"
    }
}

#[allow(dead_code)]
pub(crate) trait HasUniqueId {
    fn unique_id(&self) -> &str;
}

impl HasUniqueId for DesiredAgentBehavior {
    fn unique_id(&self) -> &str {
        &self.behavior_id
    }
}
impl HasUniqueId for DesiredToolSelection {
    fn unique_id(&self) -> &str {
        &self.selection_id
    }
}
impl HasUniqueId for DesiredSkill {
    fn unique_id(&self) -> &str {
        &self.skill_id
    }
}
impl HasUniqueId for DesiredDatastoreToolSurface {
    fn unique_id(&self) -> &str {
        &self.surface_id
    }
}
impl HasUniqueId for DesiredChainKeyBinding {
    fn unique_id(&self) -> &str {
        &self.binding_id
    }
}
impl HasUniqueId for DesiredEthTool {
    fn unique_id(&self) -> &str {
        &self.tool_id
    }
}
impl HasUniqueId for DesiredInferenceBackend {
    fn unique_id(&self) -> &str {
        &self.backend_id
    }
}
impl HasUniqueId for DesiredInferenceProfile {
    fn unique_id(&self) -> &str {
        &self.profile_id
    }
}
impl HasUniqueId for DesiredToolServiceRegistry {
    fn unique_id(&self) -> &str {
        &self.service_id
    }
}
impl HasUniqueId for DesiredProjectionAcpBinding {
    fn unique_id(&self) -> &str {
        &self.binding_id
    }
}
impl HasUniqueId for DesiredTask {
    fn unique_id(&self) -> &str {
        &self.task_id
    }
}
impl HasUniqueId for DesiredSchedule {
    fn unique_id(&self) -> &str {
        &self.schedule_id
    }
}
impl HasUniqueId for DesiredEventTrigger {
    fn unique_id(&self) -> &str {
        &self.trigger_id
    }
}
impl HasUniqueId for DesiredCallbackBinding {
    fn unique_id(&self) -> &str {
        &self.binding_id
    }
}
impl HasUniqueId for DesiredRepositoryPlacement {
    fn unique_id(&self) -> &str {
        &self.repository_id
    }
}

#[cfg(test)]
mod desired_fields_tests {
    use super::*;
    use gents::DesiredFields;

    #[test]
    fn desired_structs_report_their_collection_tags() {
        let p = DesiredAgentPrincipal {
            agent_did: "did:x".into(),
            display_name: None,
            default_behavior_id: None,
            enabled: true,
        };
        assert_eq!(p.collection_tag(), "agent_principal");
    }
}
