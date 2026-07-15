use std::sync::Arc;

use anyhow::{anyhow, Result};
use defra_agent::defra_node::EmbeddedNode;
use defra_agent::{
    default_behavior_id_for_agent, load_agent_behavior, load_agent_principal,
    load_inference_profile, AgentBehaviorDocument, DEFAULT_CONTEXT_WINDOW,
};

pub(super) const MODEL_SELECTION_SEPARATOR: &str = "::";

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

/// Resolve the exact context window the runtime derives for a behavior. This
/// mirrors `defra_agent::agent::load_document_agent`: invalid or absent profile
/// values fall back to the runtime default.
pub(super) async fn load_bound_context_window(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<i64> {
    let profile_id = load_bound_inference_profile_id(node, behavior_id).await?;
    let profile = load_inference_profile(node, &profile_id)
        .await?
        .ok_or_else(|| anyhow!("InferenceProfile {profile_id:?} disappeared while loading"))?;
    let context_window = effective_context_window(profile.context_window);
    i64::try_from(context_window)
        .map_err(|_| anyhow!("context window {context_window} does not fit the Codex protocol"))
}

fn effective_context_window(configured: Option<i64>) -> usize {
    configured
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

pub(super) async fn load_bound_model_selection_id(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<String> {
    let behavior = load_agent_behavior(node, behavior_id)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "Codex shim is bound to behavior {behavior_id:?}, but no AgentBehavior \
                 document with that behavior_id exists."
            )
        })?;
    model_selection_id_for_behavior(behavior_id, &behavior)
}

pub(super) async fn load_bound_model_selection_id_for_state(
    node: &EmbeddedNode,
    behavior_id: &Arc<str>,
) -> Result<String> {
    load_bound_model_selection_id(node, behavior_id.as_ref()).await
}

pub(super) fn model_selection_id(backend_id: &str, model_name: &str) -> String {
    format!("{backend_id}{MODEL_SELECTION_SEPARATOR}{model_name}")
}

pub(super) fn parse_model_selection_id(value: &str) -> Option<(&str, &str)> {
    let (backend_id, model_name) = value.split_once(MODEL_SELECTION_SEPARATOR)?;
    let backend_id = backend_id.trim();
    let model_name = model_name.trim();
    (!backend_id.is_empty() && !model_name.is_empty()).then_some((backend_id, model_name))
}

fn model_selection_id_for_behavior(
    behavior_id: &str,
    behavior: &AgentBehaviorDocument,
) -> Result<String> {
    let model_name = behavior
        .model_name
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "Codex shim is bound to behavior {behavior_id:?}, but that behavior has no \
                 model_name set."
            )
        })?;
    Ok(behavior
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|backend| !backend.is_empty())
        .map(|backend_id| model_selection_id(backend_id, model_name))
        .unwrap_or_else(|| model_name.to_string()))
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
        assert_eq!(
            default_behavior_id_for_agent("did:key:zABC"),
            "did:key:zABC:default"
        );
    }

    #[test]
    fn context_window_fallback_matches_runtime_loading() {
        assert_eq!(effective_context_window(Some(32_768)), 32_768);
        assert_eq!(effective_context_window(Some(0)), 0);
        assert_eq!(effective_context_window(Some(-1)), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(effective_context_window(None), DEFAULT_CONTEXT_WINDOW);
    }
}
