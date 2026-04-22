mod apply;
mod load;
mod snapshot;

pub(crate) use apply::apply_control_update;
pub(crate) use load::load_document_runtime_view;
pub(crate) use snapshot::resolve_document_runtime_snapshot_from_view;

use std::collections::HashMap;

use crate::backend_registry::InferenceBackend;
use crate::document_config::{
    AgentBehavior, AgentPrincipal, EventTrigger, InferenceProfile, Schedule, Task,
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
    pub(crate) tool_selections: HashMap<String, DocumentRecord<ToolSelectionDocument>>,
    pub(crate) inference_profiles: HashMap<String, DocumentRecord<InferenceProfile>>,
    pub(crate) backends: HashMap<String, DocumentRecord<InferenceBackend>>,
    pub(crate) tasks: HashMap<String, DocumentRecord<Task>>,
    pub(crate) schedules: HashMap<String, DocumentRecord<Schedule>>,
    /// Stub populated in PR 2 of the event-driven-tasks series. Declared here
    /// so PR 2 can fill it without a breaking-change diff on this struct.
    #[allow(dead_code)]
    pub(crate) event_triggers: HashMap<String, DocumentRecord<EventTrigger>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ControlUpdateOutcome {
    Irrelevant,
    Applied,
    PendingVisibility,
}

impl DocumentRuntimeView {
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

#[cfg(test)]
mod tests;
