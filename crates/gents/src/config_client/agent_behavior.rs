use crate::graphql::escape_graphql_string;
use crate::AgentBehaviorDocument as AgentBehavior;
use anyhow::{Context, Result};

use super::{ConfigAccess, ConfigApplyTxn};
use gents_protocol::graphql::{
    graphql_bool_literal, graphql_rows_from_response, optional_f64_field, optional_string_field,
};

/// Upsert one `AgentBehavior` after validating its references against the
/// same access path used for the mutation. For an update, validation projects
/// the sparse input onto the stored document first, exactly matching the
/// writer's `None`-means-preserve semantics.
pub async fn write_agent_behavior_document(
    access: &ConfigAccess,
    behavior: &AgentBehavior,
) -> Result<String> {
    let txn = access.begin_apply_txn().await?;
    let result = write_agent_behavior_in_txn(&txn, behavior).await;
    match result {
        Ok(doc_id) => {
            txn.commit().await?;
            Ok(doc_id)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

async fn write_agent_behavior_in_txn(
    txn: &ConfigApplyTxn<'_>,
    behavior: &AgentBehavior,
) -> Result<String> {
    let effective = effective_agent_behavior(txn, behavior).await?;
    let refs = crate::ConfigReferences::load_in_txn(txn, &effective.agent_did).await?;
    effective.validate_references(&refs)?;
    let created_at = behavior
        .created_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let add_fields = vec![
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(&behavior.behavior_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&behavior.agent_did)
        )),
        optional_string_field("display_name", behavior.display_name.as_deref()),
        optional_string_field("description", behavior.description.as_deref()),
        optional_string_field("summary", behavior.summary.as_deref()),
        optional_string_field("system_prompt", behavior.system_prompt.as_deref()),
        optional_string_field(
            "request_context_template",
            behavior.request_context_template.as_deref(),
        ),
        optional_string_field("backend_id", behavior.backend_id.as_deref()),
        optional_string_field("model_name", behavior.model_name.as_deref()),
        optional_string_field("tool_selection_id", behavior.tool_selection_id.as_deref()),
        optional_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        optional_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        optional_f64_field("compaction_threshold", behavior.compaction_threshold),
        Some(format!(
            "enabled: {}",
            graphql_bool_literal(behavior.enabled)
        )),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let update_fields = vec![
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(&behavior.agent_did)
        )),
        optional_string_field("display_name", behavior.display_name.as_deref()),
        optional_string_field("description", behavior.description.as_deref()),
        optional_string_field("summary", behavior.summary.as_deref()),
        optional_string_field("system_prompt", behavior.system_prompt.as_deref()),
        optional_string_field(
            "request_context_template",
            behavior.request_context_template.as_deref(),
        ),
        optional_string_field("backend_id", behavior.backend_id.as_deref()),
        optional_string_field("model_name", behavior.model_name.as_deref()),
        optional_string_field("tool_selection_id", behavior.tool_selection_id.as_deref()),
        optional_string_field(
            "inference_profile_id",
            behavior.inference_profile_id.as_deref(),
        ),
        optional_string_field(
            "compaction_strategy",
            behavior.compaction_strategy.as_deref(),
        ),
        optional_f64_field("compaction_threshold", behavior.compaction_threshold),
        Some(format!(
            "enabled: {}",
            graphql_bool_literal(behavior.enabled)
        )),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(",\n                    ");
    let mutation = format!(
        r#"mutation {{
            upsert_AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                add: {{
                    {add_fields}
                }},
                update: {{
                    {update_fields}
                }}
            ) {{ _docID }}
        }}"#,
        behavior_id = escape_graphql_string(&behavior.behavior_id),
        add_fields = add_fields,
        update_fields = update_fields,
    );
    let response = txn.execute(&mutation).await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentBehavior")
}

async fn effective_agent_behavior(
    txn: &ConfigApplyTxn<'_>,
    patch: &AgentBehavior,
) -> Result<AgentBehavior> {
    let existing = load_agent_behavior_in_txn(txn, &patch.behavior_id).await?;
    let mut effective = merge_agent_behavior_patch(existing.clone(), patch.clone());
    if existing.is_none() {
        // This imperative writer does not own the apply-managed skill arrays.
        // On create DefraDB therefore persists their empty defaults.
        effective.skill_refs.clear();
        effective.skill_excludes.clear();
    }
    Ok(effective)
}

pub(crate) async fn load_agent_behavior_in_txn(
    txn: &ConfigApplyTxn<'_>,
    behavior_id: &str,
) -> Result<Option<AgentBehavior>> {
    let behavior_id = escape_graphql_string(behavior_id);
    let query = format!(
        r#"{{
            AgentBehavior(
                filter: {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                limit: 1
            ) {{
                behavior_id agent_did display_name description summary system_prompt
                request_context_template backend_id model_name tool_selection_id
                inference_profile_id compaction_strategy compaction_threshold enabled
                skill_refs skill_excludes created_at
            }}
        }}"#
    );
    let response = txn.execute(&query).await?;
    graphql_rows_from_response(&response, "AgentBehavior")
        .into_iter()
        .next()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding existing AgentBehavior")
}

fn merge_agent_behavior_patch(
    existing: Option<AgentBehavior>,
    patch: AgentBehavior,
) -> AgentBehavior {
    let Some(mut effective) = existing else {
        return patch;
    };

    effective.agent_did = patch.agent_did;
    effective.enabled = patch.enabled;
    macro_rules! apply_optional {
        ($field:ident) => {
            if patch.$field.is_some() {
                effective.$field = patch.$field;
            }
        };
    }
    apply_optional!(display_name);
    apply_optional!(description);
    apply_optional!(summary);
    apply_optional!(system_prompt);
    apply_optional!(request_context_template);
    apply_optional!(backend_id);
    apply_optional!(model_name);
    apply_optional!(tool_selection_id);
    apply_optional!(inference_profile_id);
    apply_optional!(compaction_strategy);
    apply_optional!(compaction_threshold);
    effective
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ensure_runtime_schemas, load_agent_behavior};
    use defra_node::{EmbeddedNode, StorageBackend};
    use std::sync::Arc;

    #[tokio::test]
    async fn create_validates_the_skill_arrays_the_writer_actually_persists() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Local(node.clone());
        let behavior = AgentBehavior {
            behavior_id: "behavior-create".to_string(),
            agent_did: "did:test:owner".to_string(),
            display_name: None,
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: vec!["not-written".to_string()],
            skill_excludes: vec!["also-not-written".to_string()],
            created_at: None,
        };

        write_agent_behavior_document(&access, &behavior).await?;
        let stored = load_agent_behavior(&node, &behavior.behavior_id)
            .await?
            .expect("created behavior");
        assert!(stored.skill_refs.is_empty());
        assert!(stored.skill_excludes.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn sparse_backend_change_validates_the_preserved_model() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("data"))
                .with_storage_backend(StorageBackend::Regolith)
                .build()
                .await?,
        );
        ensure_runtime_schemas(&node).await?;
        let response = node
            .execute(
                r#"mutation {
                    a: create_InferenceBackend(input: {
                        backend_id: "backend-a", name: "A", provider_kind: "OpenAiCompatible",
                        endpoint: "http://a.test/v1", max_concurrent: 1, max_queue_depth: 1,
                        enabled: true, models: ["model-a"], probe_status: "healthy"
                    }) { _docID }
                    b: create_InferenceBackend(input: {
                        backend_id: "backend-b", name: "B", provider_kind: "OpenAiCompatible",
                        endpoint: "http://b.test/v1", max_concurrent: 1, max_queue_depth: 1,
                        enabled: true, models: ["model-b"], probe_status: "healthy"
                    }) { _docID }
                }"#,
            )
            .await;
        assert!(
            !response.has_errors(),
            "seed backends: {:?}",
            response.errors
        );

        let access = ConfigAccess::Local(node.clone());
        let behavior = AgentBehavior {
            behavior_id: "behavior".to_string(),
            agent_did: "did:test:owner".to_string(),
            display_name: None,
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: Some("backend-a".to_string()),
            model_name: Some("model-a".to_string()),
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            created_at: None,
        };
        write_agent_behavior_document(&access, &behavior).await?;

        let mut patch = behavior.clone();
        patch.backend_id = Some("backend-b".to_string());
        patch.model_name = None;
        let error = write_agent_behavior_document(&access, &patch)
            .await
            .expect_err("preserved model must be checked against the new backend")
            .to_string();
        assert!(error.contains("does not advertise"), "{error}");

        let stored = load_agent_behavior(&node, "behavior")
            .await?
            .expect("stored behavior");
        assert_eq!(stored.backend_id.as_deref(), Some("backend-a"));
        assert_eq!(stored.model_name.as_deref(), Some("model-a"));
        Ok(())
    }

    #[tokio::test]
    async fn document_upsert_validates_preserved_skill_references() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(dir.path().join("data"))
                .with_storage_backend(StorageBackend::Regolith)
                .build()
                .await?,
        );
        ensure_runtime_schemas(&node).await?;
        let response = node
            .execute(
                r#"mutation { create_AgentBehavior(input: {
                    behavior_id: "direct-behavior", agent_did: "did:test:direct",
                    display_name: "Original", enabled: true, skill_refs: ["missing-skill"]
                }) { _docID } }"#,
            )
            .await;
        assert!(!response.has_errors(), "seed: {:?}", response.errors);

        let behavior = AgentBehavior {
            behavior_id: "direct-behavior".to_string(),
            agent_did: "did:test:direct".to_string(),
            display_name: Some("Updated".to_string()),
            description: None,
            summary: None,
            system_prompt: None,
            request_context_template: None,
            backend_id: None,
            model_name: None,
            tool_selection_id: None,
            inference_profile_id: None,
            compaction_strategy: None,
            compaction_threshold: None,
            enabled: true,
            skill_refs: Vec::new(),
            skill_excludes: Vec::new(),
            created_at: None,
        };
        let error = crate::upsert_agent_behavior(&node, &behavior)
            .await
            .expect_err("preserved missing skill reference must reject the upsert")
            .to_string();
        assert!(error.contains("missing skill_ref missing-skill"), "{error}");
        let stored = load_agent_behavior(&node, &behavior.behavior_id)
            .await?
            .expect("stored behavior");
        assert_eq!(stored.display_name.as_deref(), Some("Original"));
        Ok(())
    }
}
