use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::{
    default_behavior_id_for_agent, load_agent_behavior, load_agent_principal,
    load_inference_profile,
};

/// Normalize an explicit `--codex-shim-behavior-id` override: trim whitespace and
/// treat empty as unset.
fn explicit_override(override_behavior_id: Option<&str>) -> Option<String> {
    override_behavior_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Resolve the behavior the Codex shim binds to.
///
/// An explicit override always wins. Otherwise we use the agent principal's
/// configured `default_behavior_id` — that is the id behaviors are actually
/// stored under (e.g. `default` from an applied manifest). We only fall back to
/// the synthesized `<did>:default` form when the principal is missing or has no
/// default set, which preserves the historical behavior for legacy homes.
///
/// Previously this always synthesized `<did>:default`, which never matched a
/// manifest-applied behavior keyed `default`, so the shim failed to start with
/// "no AgentBehavior document with that behavior_id exists".
pub(super) async fn resolve_bound_behavior_id(
    node: &EmbeddedNode,
    override_behavior_id: Option<&str>,
    agent_did: &str,
) -> String {
    if let Some(value) = explicit_override(override_behavior_id) {
        return value;
    }
    match load_agent_principal(node, agent_did).await {
        Ok(Some(principal)) => principal
            .default_behavior_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| default_behavior_id_for_agent(agent_did)),
        _ => default_behavior_id_for_agent(agent_did),
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
    fn explicit_override_uses_value() {
        assert_eq!(
            explicit_override(Some("custom-behavior")),
            Some("custom-behavior".to_string())
        );
    }

    #[test]
    fn explicit_override_trims_whitespace() {
        assert_eq!(
            explicit_override(Some("  spaced  ")),
            Some("spaced".to_string())
        );
    }

    #[test]
    fn explicit_override_treats_empty_as_unset() {
        // Empty / whitespace-only override is unset, so resolution falls through
        // to the principal's default_behavior_id (or the synthesized fallback).
        assert_eq!(explicit_override(Some("")), None);
        assert_eq!(explicit_override(Some("   ")), None);
        assert_eq!(explicit_override(None), None);
    }

    #[test]
    fn synthesized_fallback_matches_did_default() {
        // When no override and no loadable principal default, resolution falls
        // back to this synthesized form.
        assert_eq!(default_behavior_id_for_agent("did:key:zABC"), "did:key:zABC:default");
    }
}
