use rapidhash::RapidHashMap;
use std::sync::{Arc, RwLock};

use super::BackgroundToolRegistry;
use crate::background::job::JobBinding;

/// What a request's toolset binds in the hook beside the tools themselves:
/// which tools may run in the background, the remote tools' identities, the
/// Goal tool authority and the plugin job tools. A rebound toolset replaces
/// them together at a turn boundary.
#[derive(Clone, Default)]
pub struct HookToolBindings {
    pub background_tool_registry: BackgroundToolRegistry,
    pub remote_tools: Option<gents_documents::RemoteTools>,
    pub goal_tools_enabled: bool,
    pub goal_creation_enabled: bool,
    pub job_tools: RapidHashMap<String, JobBinding>,
}

impl HookToolBindings {
    pub fn with_goal_tool_authority(mut self, goal_tools: bool, goal_creation: bool) -> Self {
        self.goal_tools_enabled = goal_tools;
        self.goal_creation_enabled = goal_tools && goal_creation;
        self
    }
}

/// The hook's current bindings, shared by its clones.
#[derive(Clone, Default)]
pub(super) struct SharedToolBindings(Arc<RwLock<Arc<HookToolBindings>>>);

impl SharedToolBindings {
    pub(super) fn current(&self) -> Arc<HookToolBindings> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn replace(&self, bindings: HookToolBindings) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::new(bindings);
    }

    pub(super) fn edit(&self, change: impl FnOnce(&mut HookToolBindings)) {
        let mut bindings = (*self.current()).clone();
        change(&mut bindings);
        self.replace(bindings);
    }
}
