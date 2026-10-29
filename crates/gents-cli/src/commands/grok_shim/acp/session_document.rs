use anyhow::Result;
use gents::config_client::ConfigAccess;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use serde_json::Value;

use super::AcpServiceConfig;

pub(super) async fn ensure_session_document(
    config: &AcpServiceConfig,
    session_id: &str,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let lookup = format!(
        r#"{{
            AgentSession(filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }}) {{
                session_id
                node_did
                requester_did
                agent_id
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(&config.node, &lookup, "grok shim AgentSession lookup")
            .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if let Some(existing) = rows
        .iter()
        .filter_map(|row| row.get("agent_id").and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
    {
        if existing != config.agent_id.as_ref() {
            anyhow::bail!(
                "session {session_id:?} already exists with agent {existing:?}; the \
                 Grok shim is bound to {:?} and will not rewrite session identity",
                config.agent_id
            );
        }
    }
    // The serving node DID is `@immutable` on `AgentSession`: an existing
    // row stamped for a different node is a hard identity mismatch and
    // must never be reactivated under this shim's identity.
    if let Some(existing) = rows
        .iter()
        .filter_map(|row| row.get("node_did").and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
    {
        if existing != config.node_did.as_ref() {
            anyhow::bail!(
                "session {session_id:?} already exists with agent {existing:?}; the \
                 Grok shim serves as {:?} and will not reactivate a session bound to \
                 a different immutable node_did",
                config.node_did
            );
        }
    }

    anyhow::ensure!(
        rows.iter()
            .all(|row| row.get("requester_did").and_then(Value::as_str)
                == Some(config.node_did.as_ref())),
        "session belongs to a different immutable requester_did"
    );
    let escaped_node_did = escape_graphql_string(&config.node_did);
    let escaped_agent_id = escape_graphql_string(&config.agent_id);
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let escaped_created_at = escape_graphql_string(&created_at);
    if rows.is_empty() {
        let create = format!(
            r#"mutation {{
                create_AgentSession(input: {{
                    session_id: "{escaped_session_id}",
                    node_did: "{escaped_node_did}",
                    requester_did: "{escaped_node_did}",
                    agent_id: "{escaped_agent_id}",
                    created_at: "{escaped_created_at}"
                }}) {{ _docID }}
            }}"#
        );
        ConfigAccess::local(config.node.clone())
            .write("grok.session.create", &create)
            .await?;
    } else {
        let update = format!(
            r#"mutation {{
                update_AgentSession(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    input: {{
                        closed_at: null
                    }}
                ) {{ _docID }}
            }}"#
        );
        ConfigAccess::local(config.node.clone())
            .write("grok.session.reactivate", &update)
            .await?;
    }
    Ok(())
}
