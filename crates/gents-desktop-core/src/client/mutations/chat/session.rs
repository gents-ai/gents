use crate::client::store::ClientStore;
use anyhow::Result;
use defra_node::EmbeddedNode;

use super::super::graphql::{escape_graphql_string, execute_mutation, normalize_required};

pub async fn rename_session(
    node: &EmbeddedNode,
    store: &ClientStore,
    agent_did: &str,
    requester_did: &str,
    session_id: &str,
    title: &str,
) -> Result<()> {
    let mutation =
        build_rename_session_mutation(store, agent_did, requester_did, session_id, title)?;
    execute_mutation(node, &mutation, "rename_session").await
}

fn build_rename_session_mutation(
    store: &ClientStore,
    agent_did: &str,
    requester_did: &str,
    session_id: &str,
    title: &str,
) -> Result<String> {
    let agent_did = normalize_required("agent_did", agent_did)?;
    let requester_did = normalize_required("requester_did", requester_did)?;
    let session_id = normalize_required("session_id", session_id)?;
    let title = normalize_required("title", title)?;
    store
        .sessions
        .iter()
        .find(|row| {
            row.session_id == session_id
                && row.agent_did == agent_did
                && row.requester_did.as_deref() == Some(requester_did)
        })
        .ok_or_else(|| anyhow::anyhow!("session {} not found", session_id))?;

    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_requester_did = escape_graphql_string(requester_did);
    let escaped_title = escape_graphql_string(title);
    let mutation = format!(
        r#"mutation {{
            update_AgentSession(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    agent_did: {{ _eq: "{escaped_agent_did}" }},
                    requester_did: {{ _eq: "{escaped_requester_did}" }}
                }},
                input: {{
                    title: {{ text: "{escaped_title}", source: "user" }}
                }}
            ) {{ _docID }}
        }}"#
    );
    Ok(mutation)
}
