use anyhow::Result;
use serde_json::{json, Value};

use crate::config_writes::ConfigAccess;
use crate::{CONFIG_SCHEMA_COLLECTIONS, SCHEMA_COLLECTION_CHECKS};

pub(super) async fn diagnose_schema_presence(access: &ConfigAccess) -> Vec<Value> {
    let mut results = Vec::new();
    for (collection, field) in SCHEMA_COLLECTION_CHECKS {
        let required_for_config = CONFIG_SCHEMA_COLLECTIONS.contains(collection);
        let query = format!(
            r#"{{ {collection}(limit: 1) {{ {field} }} }}"#,
            collection = collection,
            field = field
        );
        match access.execute(&query).await {
            Ok(_) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": true,
            })),
            Err(error) => results.push(json!({
                "collection": collection,
                "required_for_config": required_for_config,
                "ok": false,
                "error": error.to_string(),
            })),
        }
    }
    results
}

pub(super) async fn load_runtime_row(access: &ConfigAccess, agent_did: &str) -> Result<Option<Value>> {
    use defra_agent::graphql::escape_graphql_string;
    let query = format!(
        r#"{{
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                agent_did
                process_state
                reconcile_phase
                active_generation
                router_generation
                default_behavior_id
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
                last_reconcile_error
                last_reconcile_completed_at
                updated_at
            }}
        }}"#,
        agent_did = escape_graphql_string(agent_did),
    );
    Ok(crate::graphql_rows(access, "AgentRuntime", &query)
        .await?
        .into_iter()
        .next())
}
