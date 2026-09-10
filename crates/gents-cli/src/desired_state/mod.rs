pub(crate) mod apply_bundle;
pub(crate) mod convert;
pub(crate) mod diff;
pub(crate) mod interpolate;
pub(crate) mod load;
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
pub(crate) use write::write_manifest_root;

mod document_handle;
pub(crate) use document_handle::document_handle;

use serde::Serialize;
use gents::Collection;

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

#[derive(Debug, Clone, Default, Serialize)]
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
#[serde(transparent)]
pub(crate) struct DesiredStateDiffCollections(
    std::collections::BTreeMap<String, DesiredStateCollectionDiff>,
);

impl Default for DesiredStateDiffCollections {
    fn default() -> Self {
        Self(
            Collection::ALL
                .into_iter()
                .map(|collection| {
                    (
                        collection
                            .dir_name()
                            .unwrap_or("agent_principal")
                            .to_owned(),
                        DesiredStateCollectionDiff::default(),
                    )
                })
                .collect(),
        )
    }
}

impl DesiredStateDiffCollections {
    pub(crate) fn get(&self, collection: Collection) -> &DesiredStateCollectionDiff {
        &self.0[collection.dir_name().unwrap_or("agent_principal")]
    }
    fn get_mut(&mut self, collection: Collection) -> &mut DesiredStateCollectionDiff {
        self.0
            .get_mut(collection.dir_name().unwrap_or("agent_principal"))
            .expect("all canonical collection reports are initialized")
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
        DesiredStateDiffCollectionsCounts(
            self.0
                .iter()
                .map(|(key, diff)| (key.clone(), diff.counts()))
                .collect(),
        )
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub(crate) struct DesiredStateDiffCollectionsCounts(
    std::collections::BTreeMap<String, DesiredStateDiffCounts>,
);

impl DesiredStateDiffCollectionsCounts {
    pub(crate) fn iter(&self) -> impl Iterator<Item = &DesiredStateDiffCounts> {
        self.0.values()
    }
    pub(crate) fn get(&self, collection: Collection) -> &DesiredStateDiffCounts {
        &self.0[collection.dir_name().unwrap_or("agent_principal")]
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
pub(crate) struct DesiredStateValidationReport {
    pub(crate) status: &'static str,
    /// Offline parsing cannot establish the retained installation's reference closure.
    pub(crate) validation_scope: &'static str,
    pub(crate) ok: bool,
    pub(crate) root: String,
    pub(crate) agent_did: Option<String>,
    pub(crate) counts: crate::shared::ConfigApplyCounts,
    pub(crate) errors: Vec<String>,
}

impl DesiredStateValidationReport {
    pub(crate) fn is_ok(&self) -> bool {
        self.ok
    }
}
