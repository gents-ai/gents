use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, watch};

use crate::admission::BackendAdmissionConfig;
use crate::config::BehaviorConfig;
use crate::tool_surface::ToolSurface;
use crate::watcher::AgentRequest;

pub(crate) type DispatcherMap = HashMap<String, mpsc::Sender<AgentRequest>>;

#[derive(Clone, Debug)]
pub(crate) struct ResolvedRuntimeSnapshot {
    pub(crate) default_behavior_id: String,
    pub(crate) behaviors: HashMap<String, Arc<BehaviorConfig>>,
    pub(crate) tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    pub(crate) backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
    pub(crate) unavailable_behaviors: HashMap<String, String>,
}

impl ResolvedRuntimeSnapshot {
    #[allow(dead_code)]
    pub(crate) fn from_parts(
        default_behavior_id: String,
        behaviors: Vec<Arc<BehaviorConfig>>,
        tool_surfaces: HashMap<String, Arc<ToolSurface>>,
        unavailable_behaviors: HashMap<String, String>,
    ) -> Self {
        Self::from_parts_with_admission_configs(
            default_behavior_id,
            behaviors,
            tool_surfaces,
            HashMap::new(),
            unavailable_behaviors,
        )
    }

    pub(crate) fn from_parts_with_admission_configs(
        default_behavior_id: String,
        behaviors: Vec<Arc<BehaviorConfig>>,
        tool_surfaces: HashMap<String, Arc<ToolSurface>>,
        backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
        unavailable_behaviors: HashMap<String, String>,
    ) -> Self {
        Self {
            default_behavior_id,
            behaviors: behaviors
                .into_iter()
                .map(|behavior| (behavior.name.clone(), behavior))
                .collect(),
            tool_surfaces,
            backend_admission_configs,
            unavailable_behaviors,
        }
    }

    pub(crate) fn activate(
        self,
        generation: u64,
        dispatchers: DispatcherMap,
    ) -> ActiveRuntimeSnapshot {
        ActiveRuntimeSnapshot {
            generation,
            default_behavior_id: self.default_behavior_id,
            behaviors: self.behaviors,
            tool_surfaces: self.tool_surfaces,
            backend_admission_configs: self.backend_admission_configs,
            unavailable_behaviors: self.unavailable_behaviors,
            dispatchers,
        }
    }

    pub(crate) fn configuration_fingerprint(&self) -> String {
        configuration_fingerprint(
            &self.default_behavior_id,
            &self.behaviors,
            &self.tool_surfaces,
            &self.backend_admission_configs,
            &self.unavailable_behaviors,
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ActiveRuntimeSnapshot {
    pub(crate) generation: u64,
    pub(crate) default_behavior_id: String,
    pub(crate) behaviors: HashMap<String, Arc<BehaviorConfig>>,
    pub(crate) tool_surfaces: HashMap<String, Arc<ToolSurface>>,
    pub(crate) backend_admission_configs: HashMap<String, BackendAdmissionConfig>,
    pub(crate) unavailable_behaviors: HashMap<String, String>,
    pub(crate) dispatchers: DispatcherMap,
}

impl ActiveRuntimeSnapshot {
    pub(crate) fn behavior(&self, behavior_id: &str) -> Option<&Arc<BehaviorConfig>> {
        self.behaviors.get(behavior_id)
    }

    pub(crate) fn tool_surface(&self, behavior_id: &str) -> Option<&Arc<ToolSurface>> {
        self.tool_surfaces.get(behavior_id)
    }

    pub(crate) fn unavailable_reason(&self, behavior_id: &str) -> Option<&str> {
        self.unavailable_behaviors
            .get(behavior_id)
            .map(String::as_str)
    }

    pub(crate) fn configuration_fingerprint(&self) -> String {
        configuration_fingerprint(
            &self.default_behavior_id,
            &self.behaviors,
            &self.tool_surfaces,
            &self.backend_admission_configs,
            &self.unavailable_behaviors,
        )
    }
}

pub(crate) fn refresh_active_snapshot(
    active_snapshot: &mut Arc<ActiveRuntimeSnapshot>,
    active_snapshot_rx: &mut watch::Receiver<Arc<ActiveRuntimeSnapshot>>,
) -> bool {
    match active_snapshot_rx.has_changed() {
        Ok(true) => {
            *active_snapshot = active_snapshot_rx.borrow_and_update().clone();
            true
        }
        Ok(false) | Err(_) => false,
    }
}

fn configuration_fingerprint(
    default_behavior_id: &str,
    behaviors: &HashMap<String, Arc<BehaviorConfig>>,
    tool_surfaces: &HashMap<String, Arc<ToolSurface>>,
    backend_admission_configs: &HashMap<String, BackendAdmissionConfig>,
    unavailable_behaviors: &HashMap<String, String>,
) -> String {
    let mut fingerprint = String::new();
    fingerprint.push_str("default:");
    fingerprint.push_str(default_behavior_id);
    fingerprint.push('\n');

    let mut behavior_ids = behaviors.keys().cloned().collect::<Vec<_>>();
    behavior_ids.sort();
    for behavior_id in behavior_ids {
        let behavior = behaviors
            .get(&behavior_id)
            .expect("behavior id came from behaviors map");
        fingerprint.push_str("behavior:");
        fingerprint.push_str(&behavior_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{behavior:?}"));
        fingerprint.push('\n');
    }

    let mut tool_ids = tool_surfaces.keys().cloned().collect::<Vec<_>>();
    tool_ids.sort();
    for behavior_id in tool_ids {
        let tool_surface = tool_surfaces
            .get(&behavior_id)
            .expect("behavior id came from tool surface map");
        fingerprint.push_str("tools:");
        fingerprint.push_str(&behavior_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{tool_surface:?}"));
        fingerprint.push('\n');
    }

    let mut backend_ids = backend_admission_configs
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    backend_ids.sort();
    for backend_id in backend_ids {
        let config = backend_admission_configs
            .get(&backend_id)
            .expect("backend id came from backend admission config map");
        fingerprint.push_str("backend_admission:");
        fingerprint.push_str(&backend_id);
        fingerprint.push('=');
        fingerprint.push_str(&format!("{config:?}"));
        fingerprint.push('\n');
    }

    let mut unavailable_ids = unavailable_behaviors.keys().cloned().collect::<Vec<_>>();
    unavailable_ids.sort();
    for behavior_id in unavailable_ids {
        let reason = unavailable_behaviors
            .get(&behavior_id)
            .expect("behavior id came from unavailable behavior map");
        fingerprint.push_str("unavailable:");
        fingerprint.push_str(&behavior_id);
        fingerprint.push('=');
        fingerprint.push_str(reason);
        fingerprint.push('\n');
    }

    fingerprint
}

#[cfg(test)]
mod tests;
