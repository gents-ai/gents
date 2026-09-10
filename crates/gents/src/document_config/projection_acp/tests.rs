use super::*;
use serde_json::json;

#[test]
fn canonical_projection_config_guards_apply_through_reference_owner() {
    let valid = json!({"binding_id":"projection", "agent_did":"owner", "policy_id":"active",
        "staged_policy_id":"staged", "previous_policy_id":"previous", "projection_id":"atif_trajectory",
        "resource_map_json": r#"{"AgentSession":"sessions","InferenceCall":"inference"}"#});
    let validate = |value: serde_json::Value| {
        gents_refs("owner", value)
    };
    validate(valid.clone()).unwrap();
    for (field, bad) in [
        ("policy_id", json!(" ")),
        ("staged_policy_id", json!("active")),
        ("staged_policy_id", json!("previous")),
        ("previous_policy_id", json!("active")),
        ("projection_id", json!("unknown")),
        ("resource_map_json", json!(r#"{"AgentConversation":"sessions"}"#)),
        ("resource_map_json", json!(r#"{"AgentSession":""}"#)),
        ("resource_map_json", json!("[]")),
    ] {
        let mut invalid = valid.clone(); invalid[field] = bad;
        assert!(validate(invalid).is_err(), "accepted invalid {field}");
    }
    let mut observed = valid;
    observed["publication_status"] = json!("published");
    assert!(serde_json::from_value::<ProjectionAcpBinding>(observed).is_err());
}

fn gents_refs(owner: &str, value: serde_json::Value) -> anyhow::Result<()> {
    super::super::ConfigReferences::from_documents(owner,
        [(crate::Collection::ProjectionAcpBinding, value)])?.validate()
}

#[test]
fn projection_resource_map_preserves_runtime_scope_and_rejects_bad_names() {
    let parsed = parse_projection_resource_map(Some(r#"{"AgentSession":" sessions ","RenderedRequest":"capture"}"#)).unwrap();
    assert_eq!(parsed["AgentSession"], "sessions");
    assert_eq!(parsed["RenderedRequest"], "capture");
    for raw in [r#"{"AgentConversation":"sessions"}"#, r#"{"AgentMesage":"messages"}"#, r#"{"AgentRequest":""}"#] {
        assert!(parse_projection_resource_map(Some(raw)).is_err());
    }
    assert!(parse_projection_resource_map(None).unwrap().is_empty());
}
