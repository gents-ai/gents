use anyhow::{bail, Context, Result};
use chrono::Utc;
use defra_node::EmbeddedNode;
use gents::config_client::ConfigApplyTxn;
use gents::{AgentBehaviorDocument, ConfigReferences};
use gents_protocol::row::AgentBehaviorRow;
use serde_json::Value;

use super::super::graphql::{
    escape_graphql_string, graphql_optional_bool_field, graphql_optional_float_field,
    graphql_string_field, graphql_string_list_field, join_fields, normalize_required,
};

pub async fn upsert_agent_behavior(node: &EmbeddedNode, row: &AgentBehaviorRow) -> Result<()> {
    let behavior = agent_behavior_document(row)?;
    let mutation = build_upsert_agent_behavior_mutation(row)?;
    let txn = ConfigApplyTxn::begin_local(node, None).await?;
    let result = async {
        let references = ConfigReferences::load_in_txn(&txn, &behavior.agent_did).await?;
        behavior.validate_references(&references)?;
        // Keep the desktop encoder: represented row options are authoritative
        // clears, while columns absent from AgentBehaviorRow (description,
        // summary, request_context_template) remain stored state.
        txn.execute(&mutation).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => txn.commit().await,
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

fn agent_behavior_document(row: &AgentBehaviorRow) -> Result<AgentBehaviorDocument> {
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for AgentBehavior")?,
    )?;
    let mut value = serde_json::to_value(row)?;
    value["agent_did"] = Value::String(agent_did.to_string());
    value["enabled"] = Value::Bool(row.enabled.unwrap_or(false));
    Ok(serde_json::from_value(value)?)
}

fn build_upsert_agent_behavior_mutation(row: &AgentBehaviorRow) -> Result<String> {
    let behavior_id = normalize_required("behavior_id", &row.behavior_id)?;
    let agent_did = normalize_required(
        "agent_did",
        row.agent_did
            .as_deref()
            .context("agent_did is required for AgentBehavior")?,
    )?;
    let created_at = row
        .created_at
        .as_deref()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Utc::now().to_rfc3339());

    let add_fields = [
        Some(format!(
            r#"behavior_id: "{}""#,
            escape_graphql_string(behavior_id)
        )),
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "system_prompt",
            row.system_prompt.as_deref(),
        )),
        Some(graphql_string_field(
            "backend_id",
            row.backend_id.as_deref(),
        )),
        Some(graphql_string_field(
            "model_name",
            row.model_name.as_deref(),
        )),
        Some(graphql_string_field(
            "tool_selection_id",
            row.tool_selection_id.as_deref(),
        )),
        Some(graphql_string_field(
            "inference_profile_id",
            row.inference_profile_id.as_deref(),
        )),
        Some(graphql_string_field(
            "compaction_strategy",
            row.compaction_strategy.as_deref(),
        )),
        Some(graphql_optional_float_field(
            "compaction_threshold",
            row.compaction_threshold,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("skill_refs", &row.skill_refs)),
        Some(graphql_string_list_field(
            "skill_excludes",
            &row.skill_excludes,
        )),
        Some(format!(
            r#"created_at: "{}""#,
            escape_graphql_string(&created_at)
        )),
    ];
    let update_fields = [
        Some(format!(
            r#"agent_did: "{}""#,
            escape_graphql_string(agent_did)
        )),
        Some(graphql_string_field(
            "display_name",
            row.display_name.as_deref(),
        )),
        Some(graphql_string_field(
            "system_prompt",
            row.system_prompt.as_deref(),
        )),
        Some(graphql_string_field(
            "backend_id",
            row.backend_id.as_deref(),
        )),
        Some(graphql_string_field(
            "model_name",
            row.model_name.as_deref(),
        )),
        Some(graphql_string_field(
            "tool_selection_id",
            row.tool_selection_id.as_deref(),
        )),
        Some(graphql_string_field(
            "inference_profile_id",
            row.inference_profile_id.as_deref(),
        )),
        Some(graphql_string_field(
            "compaction_strategy",
            row.compaction_strategy.as_deref(),
        )),
        Some(graphql_optional_float_field(
            "compaction_threshold",
            row.compaction_threshold,
        )),
        Some(graphql_optional_bool_field("enabled", row.enabled)),
        Some(graphql_string_list_field("skill_refs", &row.skill_refs)),
        Some(graphql_string_list_field(
            "skill_excludes",
            &row.skill_excludes,
        )),
    ];

    Ok(format!(
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
        behavior_id = escape_graphql_string(behavior_id),
        add_fields = join_fields(&add_fields),
        update_fields = join_fields(&update_fields),
    ))
}

pub async fn delete_agent_behavior(
    node: &EmbeddedNode,
    agent_did: &str,
    behavior_id: &str,
) -> Result<usize> {
    let mutation = build_delete_agent_behavior_mutation(agent_did, behavior_id)?;
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "delete_agent_behavior failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("delete_AgentBehavior"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0))
}

fn build_delete_agent_behavior_mutation(agent_did: &str, behavior_id: &str) -> Result<String> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let behavior_id = normalize_required("behavior_id", behavior_id)?;
    let agent_did = escape_graphql_string(agent_did);
    let behavior_id = escape_graphql_string(behavior_id);
    Ok(format!(
        r#"mutation {{
            delete_AgentBehavior(
                filter: {{
                    _and: [
                        {{ behavior_id: {{ _eq: "{behavior_id}" }} }},
                        {{ agent_did: {{ _eq: "{agent_did}" }} }}
                    ]
                }}
            ) {{ _docID }}
        }}"#
    ))
}

#[cfg(test)]
mod tests {
    use super::{agent_behavior_document, build_delete_agent_behavior_mutation};
    use gents::ConfigReferences;
    use gents_protocol::row::AgentBehaviorRow;

    #[test]
    fn delete_is_scoped_to_agent_and_escapes_values() {
        let mutation = build_delete_agent_behavior_mutation("did:test:remote", "say-\"hi\"")
            .expect("delete mutation");

        assert!(mutation.contains(r#"agent_did: { _eq: "did:test:remote" }"#));
        assert!(mutation.contains(r#"behavior_id: { _eq: "say-\"hi\"" }"#));
    }

    #[test]
    fn behavior_row_reports_all_missing_references() {
        let row: AgentBehaviorRow = serde_json::from_value(serde_json::json!({
            "behavior_id": "amy",
            "agent_did": "did:test:amy",
            "backend_id": "missing-backend",
            "tool_selection_id": "missing-tools",
            "inference_profile_id": "missing-profile"
        }))
        .expect("behavior row");
        let behavior = agent_behavior_document(&row).expect("behavior document");

        let error = behavior
            .validate_references(&ConfigReferences::default())
            .expect_err("invalid behavior")
            .to_string();
        assert!(error.contains("missing backend_id missing-backend"));
        assert!(error.contains("missing tool_selection_id missing-tools"));
        assert!(error.contains("missing inference_profile_id missing-profile"));
    }
}
