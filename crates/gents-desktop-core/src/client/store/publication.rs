use super::{ClientStore, HashSet};

impl ClientStore {
    /// Known owners of a patch's retained facts. An ownerless fact may affect
    /// any scoped reload, so it must invalidate the global fence instead.
    pub(crate) fn publication_owners(&self) -> Option<HashSet<String>> {
        let mut owners = self.operator_config_authoritative_node_dids.clone();
        if owners.contains("") {
            return None;
        }
        macro_rules! owned {
            ($($field:ident),+ $(,)?) => {$(
                for row in &self.$field {
                    if row.node_did.is_empty() { return None; }
                    owners.insert(row.node_did.clone());
                }
            )+};
        }
        owned!(
            nodes,
            agents,
            runtimes,
            node_readiness,
            mailbox_items,
            sessions,
            goals,
            tasks,
            schedules,
            triggers,
            skills,
            tools,
            contexts,
            compactions,
            inference_backends,
            inference_profiles,
            inference_sampling,
            inference_execution,
            tool_service_registries,
            event_sources,
            agent_targets,
            datastore_tool_surfaces,
            chain_key_bindings
        );
        for row in &self.requests {
            let owner = row.node_did.as_deref().filter(|owner| !owner.is_empty())?;
            owners.insert(owner.to_owned());
        }
        macro_rules! observations {
            ($($rows:ident => $sources:ident),+ $(,)?) => {$(
                for index in 0..self.$rows.len() {
                    let owner = self.$sources.get(index)?.as_deref()
                        .filter(|owner| !owner.is_empty())?;
                    owners.insert(owner.to_owned());
                }
            )+};
        }
        observations!(
            schedule_observations => schedule_observation_source_node_dids,
            trigger_observations => trigger_observation_source_node_dids,
            backend_observations => backend_observation_source_node_dids,
        );
        // Source ownership also participates in merge identity, even where a
        // row supplies a canonical node DID of its own.
        macro_rules! sources {
            ($($field:ident),+ $(,)?) => {$(
                for owner in self.$field.iter().flatten() {
                    if owner.is_empty() { return None; }
                    owners.insert(owner.clone());
                }
            )+};
        }
        sources!(
            session_source_node_dids,
            task_source_node_dids,
            schedule_source_node_dids,
            trigger_source_node_dids,
            skill_source_node_dids,
            tools_source_node_dids,
            context_source_node_dids,
            compaction_source_node_dids,
            inference_backend_source_node_dids,
            inference_profile_source_node_dids,
            inference_sampling_source_node_dids,
            inference_execution_source_node_dids,
            tool_service_registry_source_node_dids,
            event_source_source_node_dids,
            agent_target_source_node_dids,
            datastore_tool_surface_source_node_dids,
            chain_key_binding_source_node_dids
        );
        Some(owners)
    }
}
