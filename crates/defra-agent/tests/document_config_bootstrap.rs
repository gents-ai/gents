use defra_agent::graphql::escape_graphql_string;
use defra_agent::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, list_agent_behaviors, load_agent_behavior, load_inference_profile,
    upsert_agent_behavior, upsert_inference_profile, AgentBehavior, InferenceProfile,
};

mod support;

use support::test_db;

#[tokio::test]
async fn ensure_agent_principal_creates_and_reuses_default_behavior() {
    let db = test_db("principal-bootstrap-create").await;
    let agent_did = "did:defra-agent:amy";

    let created = ensure_agent_principal(db.node.as_ref(), agent_did)
        .await
        .expect("bootstrap succeeds");
    assert!(created.created_principal);
    assert!(created.created_default_behavior);
    assert!(created.created_default_inference_profile);
    assert_eq!(created.principal.agent_did, agent_did);
    assert_eq!(created.principal.display_name.as_deref(), Some("amy"));
    assert_eq!(
        created.principal.default_behavior_id.as_deref(),
        Some(default_behavior_id_for_agent(agent_did).as_str())
    );
    assert_eq!(
        created.default_behavior.behavior_id,
        default_behavior_id_for_agent(agent_did)
    );
    assert_eq!(
        created.default_behavior.display_name.as_deref(),
        Some("Default")
    );
    assert_eq!(
        created.default_behavior.inference_profile_id.as_deref(),
        Some(
            default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(agent_did))
                .as_str()
        )
    );
    assert_eq!(
        created.default_inference_profile.profile_id,
        default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(agent_did))
    );
    assert!(created.default_behavior.enabled);

    let reused = ensure_agent_principal(db.node.as_ref(), agent_did)
        .await
        .expect("second bootstrap succeeds");
    assert!(!reused.created_principal);
    assert!(!reused.created_default_behavior);
    assert!(!reused.created_default_inference_profile);

    let behaviors = list_agent_behaviors(db.node.as_ref(), agent_did)
        .await
        .expect("list behaviors");
    assert_eq!(behaviors.len(), 1);
    assert_eq!(
        behaviors[0].behavior_id,
        default_behavior_id_for_agent(agent_did)
    );
}

#[tokio::test]
async fn ensure_agent_principal_backfills_missing_default_behavior() {
    let db = test_db("principal-bootstrap-backfill").await;
    let agent_did = "did:defra-agent:backfill";
    insert_principal(db.node.as_ref(), agent_did, "").await;

    let bootstrap = ensure_agent_principal(db.node.as_ref(), agent_did)
        .await
        .expect("bootstrap succeeds");
    assert!(!bootstrap.created_principal);
    assert!(bootstrap.created_default_behavior);
    assert!(bootstrap.created_default_inference_profile);
    assert_eq!(
        bootstrap.principal.default_behavior_id.as_deref(),
        Some(default_behavior_id_for_agent(agent_did).as_str())
    );
    assert_eq!(
        bootstrap.default_behavior.inference_profile_id.as_deref(),
        Some(
            default_inference_profile_id_for_behavior(&default_behavior_id_for_agent(agent_did))
                .as_str()
        )
    );
}

#[tokio::test]
async fn ensure_agent_principal_rejects_missing_referenced_default_behavior() {
    let db = test_db("principal-bootstrap-missing-default").await;
    let agent_did = "did:defra-agent:broken";
    insert_principal(db.node.as_ref(), agent_did, "custom-behavior").await;

    let error = ensure_agent_principal(db.node.as_ref(), agent_did)
        .await
        .expect_err("bootstrap should fail");
    assert!(error
        .to_string()
        .contains("references missing default behavior custom-behavior"));
}

#[tokio::test]
async fn load_inference_profile_reads_document_fields() {
    let db = test_db("inference-profile-load").await;
    let profile_id = "balanced";
    insert_inference_profile(db.node.as_ref(), profile_id).await;

    let profile = load_inference_profile(db.node.as_ref(), profile_id)
        .await
        .expect("load succeeds")
        .expect("profile exists");
    assert_eq!(profile.profile_id, profile_id);
    assert_eq!(profile.display_name.as_deref(), Some("Balanced"));
    assert_eq!(profile.context_window, Some(32768));
    assert_eq!(profile.max_output_tokens, Some(4096));
    assert_eq!(profile.temperature, Some(0.2));
    assert_eq!(profile.deadline_duration_secs, Some(120));
}

#[tokio::test]
async fn upsert_helpers_roundtrip_behavior_and_profile() {
    let db = test_db("document-config-upsert-roundtrip").await;
    let agent_did = "did:defra-agent:roundtrip";
    let behavior_id = default_behavior_id_for_agent(agent_did);

    upsert_inference_profile(
        db.node.as_ref(),
        &InferenceProfile {
            profile_id: "balanced".to_string(),
            display_name: Some("Balanced".to_string()),
            context_window: Some(32768),
            max_output_tokens: Some(4096),
            max_turns: Some(8),
            temperature: Some(0.2),
            stream_batch_ms: Some(500),
            deadline_duration_secs: Some(120),
        },
    )
    .await
    .expect("upsert inference profile");

    upsert_agent_behavior(
        db.node.as_ref(),
        &AgentBehavior {
            behavior_id: behavior_id.clone(),
            agent_did: agent_did.to_string(),
            display_name: Some("Default".to_string()),
            system_prompt: Some("Be precise".to_string()),
            backend_id: Some("backend-local".to_string()),
            model_name: Some("gpt-local".to_string()),
            tool_selection_id: None,
            inference_profile_id: Some("balanced".to_string()),
            compaction_strategy: Some("Summarize".to_string()),
            compaction_threshold: Some(0.6),
            enabled: true,
            created_at: None,
        },
    )
    .await
    .expect("upsert behavior");

    let behavior = load_agent_behavior(db.node.as_ref(), &behavior_id)
        .await
        .expect("load behavior")
        .expect("behavior exists");
    assert_eq!(behavior.agent_did, agent_did);
    assert_eq!(behavior.system_prompt.as_deref(), Some("Be precise"));
    assert_eq!(behavior.backend_id.as_deref(), Some("backend-local"));
    assert_eq!(behavior.inference_profile_id.as_deref(), Some("balanced"));

    let profile = load_inference_profile(db.node.as_ref(), "balanced")
        .await
        .expect("load profile")
        .expect("profile exists");
    assert_eq!(profile.context_window, Some(32768));
    assert_eq!(profile.deadline_duration_secs, Some(120));
}

#[tokio::test]
async fn tool_service_registry_schema_does_not_expose_broken_tools_relation() {
    let db = test_db("tool-service-registry-tools-relation").await;
    let response = db
        .node
        .execute(
            r#"{
                ToolServiceRegistry {
                    service_id
                    tools { name }
                }
            }"#,
        )
        .await;

    assert!(
        response.has_errors(),
        "querying the removed tools relation should fail validation"
    );
    let errors = format!("{:?}", response.errors);
    assert!(
        errors.contains("tools"),
        "expected validation error to mention tools field, got {errors}"
    );
    assert!(
        !errors.contains("TypeJoinMany"),
        "schema should not expose a tools relation that fails during join planning: {errors}"
    );
}

async fn insert_principal(
    node: &defra_agent::defra_node::EmbeddedNode,
    agent_did: &str,
    default_behavior_id: &str,
) {
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_default_behavior_id = escape_graphql_string(default_behavior_id);
    let mutation = format!(
        r#"mutation {{
            create_AgentPrincipal(input: {{
                agent_did: "{escaped_agent_did}",
                display_name: "Preset",
                default_behavior_id: "{escaped_default_behavior_id}",
                enabled: true,
                created_by: "{escaped_agent_did}"
            }}) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
}

async fn insert_inference_profile(node: &defra_agent::defra_node::EmbeddedNode, profile_id: &str) {
    let escaped_profile_id = escape_graphql_string(profile_id);
    let mutation = format!(
        r#"mutation {{
            create_InferenceProfile(input: {{
                profile_id: "{escaped_profile_id}",
                display_name: "Balanced",
                context_window: 32768,
                max_output_tokens: 4096,
                max_turns: 8,
                temperature: 0.2,
                stream_batch_ms: 500,
                deadline_duration_secs: 120
            }}) {{ _docID }}
        }}"#
    );

    let resp = node.execute(&mutation).await;
    assert!(!resp.has_errors(), "{:?}", resp.errors);
}
