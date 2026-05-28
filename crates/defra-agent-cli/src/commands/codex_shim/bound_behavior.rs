use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::{
    default_behavior_id_for_agent, load_agent_behavior, load_inference_profile,
};

pub(super) fn resolve_bound_behavior_id(
    override_behavior_id: Option<&str>,
    agent_did: &str,
) -> String {
    match override_behavior_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(value) => value.to_string(),
        None => default_behavior_id_for_agent(agent_did),
    }
}

pub(super) async fn load_bound_inference_profile_id(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<String> {
    let behavior = load_agent_behavior(node, behavior_id)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Codex shim is bound to behavior {behavior_id:?}, but no AgentBehavior \
                 document with that behavior_id exists. Create or fix the behavior with \
                 `defra-agent config behavior set --behavior-id {behavior_id} ...`."
            )
        })?;
    let profile_id = behavior.inference_profile_id.ok_or_else(|| {
        anyhow!(
            "Codex shim is bound to behavior {behavior_id:?}, but that behavior has no \
             inference_profile_id set. Run \
             `defra-agent config behavior set --behavior-id {behavior_id} \
             --inference-profile-id <profile>` to attach one."
        )
    })?;
    if load_inference_profile(node, &profile_id).await?.is_none() {
        return Err(anyhow!(
            "Bound behavior {behavior_id:?} references inference_profile_id \
             {profile_id:?}, but no InferenceProfile document with that id exists."
        ));
    }
    Ok(profile_id)
}

pub(super) async fn load_bound_inference_profile_id_for_state(
    node: &EmbeddedNode,
    behavior_id: &Arc<str>,
) -> Result<String> {
    load_bound_inference_profile_id(node, behavior_id.as_ref()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_bound_behavior_id_uses_explicit_override() {
        let resolved = resolve_bound_behavior_id(Some("custom-behavior"), "did:key:zABC");
        assert_eq!(resolved, "custom-behavior");
    }

    #[test]
    fn resolve_bound_behavior_id_falls_back_to_default_for_did() {
        let resolved = resolve_bound_behavior_id(None, "did:key:zABC");
        assert_eq!(resolved, "did:key:zABC:default");
    }

    #[test]
    fn resolve_bound_behavior_id_trims_whitespace() {
        let resolved = resolve_bound_behavior_id(Some("  spaced  "), "did:key:zABC");
        assert_eq!(resolved, "spaced");
    }

    #[test]
    fn resolve_bound_behavior_id_treats_empty_override_as_unset() {
        let resolved = resolve_bound_behavior_id(Some(""), "did:key:zABC");
        assert_eq!(resolved, "did:key:zABC:default");
    }
}
