//! Fixtures shared by the xAI reasoning-effort warning tests.

use serde_json::Value;

/// An embedded node holding an xAI API-key backend whose catalog lists
/// `grok-4.20-0309-reasoning` with no reasoning effort, and a profile `grok`
/// selecting it with effort `low`. The probe is unhealthy, so no check
/// reaches the network.
pub(crate) async fn seed_unsent_xai_effort(owner: &str) -> gents::config_client::ConfigAccess {
    let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    gents::ensure_runtime_schemas(&node).await.unwrap();
    for (collection, value) in [
        ("InferenceBackend", xai_effort_backend(owner)),
        ("InferenceProfile", xai_effort_profile(owner)),
    ] {
        let input = gents_protocol::graphql::graphql_input_literal(&value).unwrap();
        let result = node
            .execute(&format!(
                "mutation {{ create_{collection}(input: {input}) {{_docID}} }}"
            ))
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);
    }
    gents::config_client::ConfigAccess::Local(node)
}

pub(crate) fn xai_effort_backend(owner: &str) -> Value {
    serde_json::json!({
        "agent_did": owner, "backend_id": "xai", "name": "xAI",
        "provider_kind": "OpenAiCompatible", "openai_wire_api": "responses",
        "endpoint": "https://api.x.ai/v1", "enabled": true,
        "auth": {"kind": "environment", "variable": "XAI_API_KEY"},
        "probe_status": "unhealthy",
        "catalogs": {"entries": [{"agent_did": null, "observed_at": "2026-01-01T00:00:00Z",
            "models": [{"model_name": "grok-4.20-0309-reasoning", "reasoning_efforts": []}]}]}
    })
}

pub(crate) fn xai_effort_profile(owner: &str) -> Value {
    serde_json::json!({
        "agent_did": owner, "profile_id": "grok", "backend_id": "xai",
        "model_name": "grok-4.20-0309-reasoning", "reasoning_effort": "low"
    })
}
