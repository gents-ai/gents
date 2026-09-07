use std::collections::BTreeSet;

use super::super::{DesiredProjectionAcpBinding, DesiredStateManifest};
use super::storage::non_empty;

const PROJECTION_ACP_BINDING_PROJECTION_IDS: &[&str] = &[
    "openai_codex_run_trace",
    "langgraph_state_history",
    "multi_agent_task",
];

const PROJECTION_ACP_RUNTIME_COLLECTIONS: &[&str] = &[
    "AgentRequest",
    "AgentMessage",
    "AgentToolCall",
    "AgentResponse",
    "AgentSession",
    "AgentConversation",
];

pub(super) fn validate_projection_bindings(
    manifest: &DesiredStateManifest,
    principal_agent_did: &str,
    behavior_ids: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    let mut projection_binding_ids = BTreeSet::new();
    for binding in &manifest.projection_acp_bindings {
        let binding_id = binding.binding_id.trim();
        if binding_id.is_empty() {
            errors.push(
                "projection_acp_bindings manifest contains a binding with an empty binding_id"
                    .to_string(),
            );
        } else if !projection_binding_ids.insert(binding_id.to_string()) {
            errors.push(format!(
                "duplicate binding_id in projection_acp_bindings manifest: {binding_id}"
            ));
        }

        if non_empty(&binding.agent_did).is_none() {
            errors.push(format!(
                "projection ACP binding {} must contain a non-empty agent_did",
                binding.binding_id
            ));
        } else if !principal_agent_did.is_empty()
            && non_empty(&binding.agent_did) != Some(principal_agent_did)
        {
            errors.push(format!(
                "projection ACP binding {} belongs to {} not {}",
                binding.binding_id,
                binding.agent_did.as_deref().unwrap_or_default(),
                manifest.agent_principal.agent_did
            ));
        }

        if binding.policy_id.trim().is_empty() {
            errors.push(format!(
                "projection ACP binding {} must contain a non-empty policy_id",
                binding.binding_id
            ));
        }

        if let Some(behavior_id) = non_empty(&binding.behavior_id) {
            if !behavior_ids.contains(behavior_id) {
                errors.push(format!(
                    "projection ACP binding {} references missing behavior_id {}",
                    binding.binding_id, behavior_id
                ));
            }
        }

        validate_projection_id(binding, errors);
        validate_projection_policy_lifecycle(binding, errors);
        validate_projection_resource_map_json(binding, errors);
    }
}

fn validate_projection_resource_map_json(
    binding: &DesiredProjectionAcpBinding,
    errors: &mut Vec<String>,
) {
    let Some(raw) = non_empty(&binding.resource_map_json) else {
        return;
    };
    let parsed = serde_json::from_str::<std::collections::BTreeMap<String, String>>(raw);
    let resource_map = match parsed {
        Ok(resource_map) => resource_map,
        Err(error) => {
            errors.push(format!(
                "projection ACP binding {} has invalid resource_map_json: {}",
                binding.binding_id, error
            ));
            return;
        }
    };
    for (collection, resource_name) in resource_map {
        let collection = collection.trim();
        let resource_name = resource_name.trim();
        if collection.is_empty() || resource_name.is_empty() {
            errors.push(format!(
            "projection ACP binding {} resource_map_json must map non-empty collection names to non-empty ACP resource names",
            binding.binding_id
        ));
            break;
        }
        if !PROJECTION_ACP_RUNTIME_COLLECTIONS.contains(&collection) {
            errors.push(format!(
            "projection ACP binding {} resource_map_json contains unknown runtime collection {}; expected one of {}",
            binding.binding_id,
            collection,
            PROJECTION_ACP_RUNTIME_COLLECTIONS.join(", ")
        ));
        }
    }
}

fn validate_projection_id(binding: &DesiredProjectionAcpBinding, errors: &mut Vec<String>) {
    let Some(projection_id) = non_empty(&binding.projection_id) else {
        return;
    };
    if !PROJECTION_ACP_BINDING_PROJECTION_IDS.contains(&projection_id) {
        errors.push(format!(
            "projection ACP binding {} has invalid projection_id {}; expected one of {}",
            binding.binding_id,
            projection_id,
            PROJECTION_ACP_BINDING_PROJECTION_IDS.join(", ")
        ));
    }
}

fn validate_projection_policy_lifecycle(
    binding: &DesiredProjectionAcpBinding,
    errors: &mut Vec<String>,
) {
    let policy_id = binding.policy_id.trim();
    let staged_policy_id = non_empty(&binding.staged_policy_id);
    let previous_policy_id = non_empty(&binding.previous_policy_id);
    if let Some(staged_policy_id) = staged_policy_id {
        if staged_policy_id == policy_id {
            errors.push(format!(
                "projection ACP binding {} staged_policy_id must differ from active policy_id",
                binding.binding_id
            ));
        }
        if previous_policy_id == Some(staged_policy_id) {
            errors.push(format!(
                "projection ACP binding {} staged_policy_id must differ from previous_policy_id",
                binding.binding_id
            ));
        }
    }
    if previous_policy_id == Some(policy_id) {
        errors.push(format!(
            "projection ACP binding {} previous_policy_id must differ from active policy_id",
            binding.binding_id
        ));
    }

    match non_empty(&binding.publication_status) {
    None => {
        if staged_policy_id.is_some() {
            errors.push(format!(
                "projection ACP binding {} staged_policy_id requires publication_status staged or rotating",
                binding.binding_id
            ));
        }
    }
    Some("draft") => {
        if binding.enabled {
            errors.push(format!(
                "projection ACP binding {} publication_status draft must not be enabled",
                binding.binding_id
            ));
        }
        if staged_policy_id.is_some() {
            errors.push(format!(
                "projection ACP binding {} publication_status draft must not keep staged_policy_id",
                binding.binding_id
            ));
        }
    }
    Some("staged") => {
        if binding.enabled {
            errors.push(format!(
                "projection ACP binding {} publication_status staged must not be enabled",
                binding.binding_id
            ));
        }
        if staged_policy_id.is_none() {
            errors.push(format!(
                "projection ACP binding {} publication_status staged requires staged_policy_id",
                binding.binding_id
            ));
        }
    }
    Some("published") => {
        if staged_policy_id.is_some() {
            errors.push(format!(
                "projection ACP binding {} publication_status published must not keep staged_policy_id; promote it to policy_id",
                binding.binding_id
            ));
        }
    }
    Some("rotating") => {
        if staged_policy_id.is_none() {
            errors.push(format!(
                "projection ACP binding {} publication_status rotating requires staged_policy_id",
                binding.binding_id
            ));
        }
    }
    Some("retired") => {
        if binding.enabled {
            errors.push(format!(
                "projection ACP binding {} publication_status retired must not be enabled",
                binding.binding_id
            ));
        }
    }
    Some(status) => errors.push(format!(
        "projection ACP binding {} has invalid publication_status {}; expected draft, staged, published, rotating, or retired",
        binding.binding_id, status
    )),
}
}
