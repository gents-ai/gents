use crate::graphql::escape_graphql_string;
use crate::AgentBehaviorDocument as AgentBehavior;
use anyhow::Result;

use super::ConfigAccess;
use gents_protocol::graphql::{graphql_bool_literal, optional_f64_field, optional_string_field};

pub async fn write_agent_behavior_document(
    access: &ConfigAccess,
    behavior: &AgentBehavior,
) -> Result<String> {
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
    let response = access
        .execute_mutation(&mutation, "upsert AgentBehavior")
        .await?;
    gents_protocol::graphql::extract_mutation_doc_id(&response, "AgentBehavior")
}
