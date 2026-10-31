use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use std::collections::BTreeSet;

use super::{indexing::clean_string, ClientStore, ClientStoreRows};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StoreScope {
    All,
    Nodes(BTreeSet<String>),
}

impl Default for StoreScope {
    fn default() -> Self {
        Self::Nodes(BTreeSet::new())
    }
}

impl StoreScope {
    pub(crate) fn node(node_did: impl Into<String>) -> Self {
        let mut nodes = BTreeSet::new();
        nodes.insert(node_did.into());
        Self::Nodes(nodes)
    }

    pub(crate) fn insert_node(&mut self, node_did: &str) {
        if matches!(self, Self::All) {
            return;
        }
        let Some(node_did) = nonblank(Some(node_did)) else {
            *self = Self::All;
            return;
        };
        if let Self::Nodes(nodes) = self {
            if !nodes.contains(node_did) {
                nodes.insert(node_did.to_owned());
            }
        }
    }

    fn insert_optional(&mut self, node_did: Option<&str>) {
        if matches!(self, Self::All) {
            return;
        }
        match nonblank(node_did) {
            Some(node_did) => self.insert_node(node_did),
            None => *self = Self::All,
        }
    }

    pub(super) fn extend_rows(&mut self, rows: &ClientStore) {
        if matches!(self, Self::All) {
            return;
        }

        for row in &rows.nodes {
            self.insert_optional(Some(&row.node_did));
        }
        for row in &rows.agents {
            self.insert_optional(Some(&row.node_did));
        }
        for row in &rows.runtimes {
            self.insert_optional(Some(&row.node_did));
        }
        for row in &rows.node_readiness {
            self.insert_optional(Some(&row.node_did));
        }
        for row in &rows.requests {
            self.insert_optional(row.node_did.as_deref());
        }
        for row in &rows.mailbox_items {
            self.insert_optional(Some(&row.node_did));
        }
        for row in &rows.transcript_messages {
            self.insert_optional(Some(&row.message.node_did));
        }
        for row in &rows.output_segments {
            self.insert_optional(Some(&row.segment.node_did));
        }
        for row in &rows.sessions {
            self.insert_optional(Some(&row.node_did));
        }
        for row in &rows.goals {
            self.insert_optional(Some(&row.node_did));
        }
        for (index, row) in rows.tool_calls.iter().enumerate() {
            self.insert_optional(owner_or_source(
                row.node_did.as_deref(),
                source_at(&rows.tool_call_source_node_dids, index),
            ));
        }
        for (index, _) in rows.compaction_entries.iter().enumerate() {
            self.insert_optional(source_at(&rows.compaction_entry_source_node_dids, index));
        }
        for (index, row) in rows.tasks.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.task_source_node_dids, index),
            ));
        }
        for (index, row) in rows.schedules.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.schedule_source_node_dids, index),
            ));
        }
        for (index, _) in rows.schedule_observations.iter().enumerate() {
            self.insert_optional(source_at(
                &rows.schedule_observation_source_node_dids,
                index,
            ));
        }
        for (index, row) in rows.triggers.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.trigger_source_node_dids, index),
            ));
        }
        for (index, _) in rows.trigger_observations.iter().enumerate() {
            self.insert_optional(source_at(&rows.trigger_observation_source_node_dids, index));
        }
        for (index, row) in rows.skills.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.skill_source_node_dids, index),
            ));
        }
        for (index, row) in rows.tools.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.tools_source_node_dids, index),
            ));
        }
        for (index, row) in rows.contexts.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.context_source_node_dids, index),
            ));
        }
        for (index, row) in rows.compactions.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.compaction_source_node_dids, index),
            ));
        }
        for (index, row) in rows.inference_backends.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.inference_backend_source_node_dids, index),
            ));
        }
        for (index, _) in rows.backend_observations.iter().enumerate() {
            self.insert_optional(source_at(&rows.backend_observation_source_node_dids, index));
        }
        for (index, row) in rows.inference_profiles.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.inference_profile_source_node_dids, index),
            ));
        }
        for (index, row) in rows.inference_sampling.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.inference_sampling_source_node_dids, index),
            ));
        }
        for (index, row) in rows.inference_execution.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.inference_execution_source_node_dids, index),
            ));
        }
        for (index, row) in rows.tool_service_registries.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.tool_service_registry_source_node_dids, index),
            ));
        }
        for (index, row) in rows.event_sources.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.event_source_source_node_dids, index),
            ));
        }
        for (index, row) in rows.agent_targets.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.agent_target_source_node_dids, index),
            ));
        }
        for (index, row) in rows.datastore_tool_surfaces.iter().enumerate() {
            self.insert_optional(owner_or_source(
                Some(&row.node_did),
                source_at(&rows.datastore_tool_surface_source_node_dids, index),
            ));
        }
        for node_did in &rows.operator_config_authoritative_node_dids {
            self.insert_optional(Some(node_did));
        }
    }
}

fn source_at(sources: &[Option<String>], index: usize) -> Option<&str> {
    sources.get(index).and_then(Option::as_deref)
}

pub(super) fn nonblank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn owner_or_source<'a>(owner: Option<&'a str>, source: Option<&'a str>) -> Option<&'a str> {
    owner
        .filter(|value| !value.trim().is_empty())
        .or_else(|| source.filter(|value| !value.trim().is_empty()))
}

fn insert_doc_owner<T>(
    index: &mut RapidHashMap<String, Option<usize>>,
    rows: &[T],
    row_index: usize,
    doc_id: &str,
    owner: Option<&str>,
    owner_at: impl for<'a> Fn(&'a T) -> Option<&'a str>,
) {
    let Some(doc_id) = clean_string(Some(doc_id)) else {
        return;
    };
    let owner = nonblank(owner);
    match index.entry(doc_id) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(owner.map(|_| row_index));
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if let Some(previous_index) = *entry.get() {
                let previous_owner = rows.get(previous_index).and_then(owner_at);
                if nonblank(previous_owner) != owner {
                    entry.insert(None);
                }
            }
        }
    }
}

/// Row index by doc id; `None` marks an ownerless doc id or one claimed by two nodes.
type DocOwnerIndex = RapidHashMap<String, Option<usize>>;

impl ClientStore {
    pub(super) fn build_scope_indexes(
        rows: &ClientStoreRows,
    ) -> (DocOwnerIndex, RapidHashSet<String>, DocOwnerIndex) {
        let mut request_index_by_doc_id = RapidHashMap::new();
        let mut unscoped_placeholder_request_ids = RapidHashSet::new();
        for (index, row) in rows.requests.iter().enumerate() {
            if row.doc_id.is_none() && nonblank(row.node_did.as_deref()).is_none() {
                unscoped_placeholder_request_ids.insert(row.request_id.clone());
            }
            if let Some(doc_id) = row.doc_id.as_deref() {
                insert_doc_owner(
                    &mut request_index_by_doc_id,
                    &rows.requests,
                    index,
                    doc_id,
                    row.node_did.as_deref(),
                    |row| row.node_did.as_deref(),
                );
            }
        }

        let mut mailbox_index_by_doc_id = RapidHashMap::new();
        for (index, row) in rows.mailbox_items.iter().enumerate() {
            insert_doc_owner(
                &mut mailbox_index_by_doc_id,
                &rows.mailbox_items,
                index,
                &row.doc_id,
                Some(&row.node_did),
                |row| Some(row.node_did.as_str()),
            );
        }

        (
            request_index_by_doc_id,
            unscoped_placeholder_request_ids,
            mailbox_index_by_doc_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        doc_id: &str,
        request_id: &str,
        node_did: Option<&str>,
    ) -> gents_protocol::row::AgentRequestRow {
        serde_json::from_value(serde_json::json!({
            "_docID": doc_id,
            "request_id": request_id,
            "node_did": node_did,
            "purpose": "normal"
        }))
        .expect("request row")
    }

    fn mailbox(doc_id: &str, node_did: &str) -> gents_protocol::row::MailboxItemRow {
        serde_json::from_value(serde_json::json!({
            "_docID": doc_id,
            "item_key": "item",
            "requester_did": "did:key:requester",
            "node_did": node_did,
            "status": "pending",
            "kind": "approval",
            "action": "approve",
            "title": "Review",
            "source_kind": "request",
            "source_id": "source",
            "target_node_did": node_did,
            "target_agent_id": "agent",
            "created_at": "2026-01-01T00:00:00Z"
        }))
        .expect("mailbox row")
    }

    fn scoped(nodes: &[&str]) -> StoreScope {
        StoreScope::Nodes(nodes.iter().map(|node| (*node).to_owned()).collect())
    }

    #[test]
    fn canonical_owner_takes_precedence_over_source_and_observation_uses_source() {
        let incoming = ClientStore::from_rows(ClientStoreRows {
            contexts: vec![serde_json::from_value(serde_json::json!({
                "context_id": "ctx",
                "node_did": "did:node:canonical",
                "skill_ids": [],
                "tags": []
            }))
            .expect("context")],
            context_source_node_dids: vec![Some("did:node:source".to_owned())],
            schedule_observations: vec![serde_json::from_value(serde_json::json!({
                "trigger_id": "trigger",
                "next_run_at": null
            }))
            .expect("schedule observation")],
            schedule_observation_source_node_dids: vec![Some("did:node:observed".to_owned())],
            ..ClientStoreRows::default()
        });
        assert_eq!(
            ClientStore::default().merge_scope(&incoming),
            scoped(&["did:node:canonical", "did:node:observed"])
        );
    }

    #[test]
    fn physical_request_owner_is_o1_and_ambiguous_or_unknown_is_global() {
        let store = ClientStore::from_rows(ClientStoreRows {
            requests: vec![
                request("request-doc", "one", Some("did:node:one")),
                request("shared-doc", "two", Some("did:node:two")),
                request("shared-doc", "three", Some("did:node:three")),
            ],
            ..ClientStoreRows::default()
        });
        assert_eq!(
            store.request_event_scope(Some("request-doc")),
            scoped(&["did:node:one"])
        );
        assert_eq!(
            store.request_event_scope(Some("shared-doc")),
            StoreScope::All
        );
        assert_eq!(
            store.request_event_scope(Some("unknown-doc")),
            StoreScope::All
        );
        assert_eq!(store.request_event_scope(None), StoreScope::All);
    }

    #[test]
    fn merge_footprint_includes_mailbox_collision_and_unscoped_request_removal() {
        let store = ClientStore::from_rows(ClientStoreRows {
            mailbox_items: vec![mailbox("mailbox-doc", "did:node:old")],
            requests: vec![{
                let mut placeholder = request("", "request-key", None);
                placeholder.doc_id = None;
                placeholder
            }],
            ..ClientStoreRows::default()
        });
        let incoming = ClientStore::from_rows(ClientStoreRows {
            mailbox_items: vec![mailbox("mailbox-doc", "did:node:new")],
            requests: vec![request("new-doc", "other-key", Some("did:node:new"))],
            ..ClientStoreRows::default()
        });
        assert_eq!(
            store.merge_scope(&incoming),
            scoped(&["did:node:old", "did:node:new"])
        );

        let incoming = ClientStore::from_rows(ClientStoreRows {
            requests: vec![request(
                "replacement-doc",
                "request-key",
                Some("did:node:new"),
            )],
            ..ClientStoreRows::default()
        });
        assert_eq!(store.merge_scope(&incoming), StoreScope::All);
    }

    #[test]
    fn physical_request_replacement_includes_previous_owner() {
        let rows = |owner| {
            ClientStore::from_rows(ClientStoreRows {
                requests: vec![request("physical", "request", owner)],
                ..Default::default()
            })
        };
        let incoming = rows(Some("did:node:new"));
        assert_eq!(
            rows(Some("did:node:old")).merge_scope(&incoming),
            scoped(&["did:node:old", "did:node:new"])
        );
        assert_eq!(rows(None).merge_scope(&incoming), StoreScope::All);
    }

    #[test]
    fn replacement_footprint_includes_target_and_actual_foreign_owners() {
        let store = ClientStore::default();
        let incoming = ClientStore::from_rows(ClientStoreRows {
            requests: vec![request("request-doc", "one", Some("did:node:foreign"))],
            ..ClientStoreRows::default()
        });
        assert_eq!(
            store.replacement_scope("did:node:target", &incoming),
            scoped(&["did:node:foreign", "did:node:target"])
        );
        assert_eq!(store.replacement_scope("", &incoming), StoreScope::All);
    }
}
