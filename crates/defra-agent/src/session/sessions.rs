use super::query::{load_session_document, load_session_document_optional};
use super::retry::{execute_query_timed, log_mutation_timing, retry_operation};
use super::*;

pub async fn create_session(node: &EmbeddedNode, agent_name: &str) -> Result<String> {
    let session_id = uuid::Uuid::new_v4().to_string();
    create_session_with_id(node, &session_id, agent_name).await?;
    Ok(session_id)
}

pub(crate) async fn create_session_with_id(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
) -> Result<()> {
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_name = escape_graphql_string(agent_name);

    retry_operation("create_session", || async {
        let now = chrono::Utc::now().to_rfc3339();
        let started = match load_session_document_optional(node, session_id).await? {
            Some(session) => session.started,
            None => now.clone(),
        };
        let escaped_started = escape_graphql_string(&started);

        let mutation = format!(
            r#"mutation {{
                upsert_AgentSession(
                    filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                    add: {{
                        session_id: "{escaped_session_id}",
                        agent_name: "{escaped_agent_name}",
                        started: "{escaped_started}",
                        status: "active"
                    }},
                    update: {{
                        agent_name: "{escaped_agent_name}",
                        started: "{escaped_started}",
                        status: "active"
                    }}
                ) {{ _docID }}
            }}"#
        );

        let started_at = std::time::Instant::now();
        let resp = node.execute(&mutation).await;
        log_mutation_timing("create_session", started_at.elapsed());

        if !resp.has_errors() {
            return Ok(());
        }

        anyhow::bail!("create_session mutation failed: {:?}", resp.errors)
    })
    .await?;

    tracing::info!(session_id = %session_id, agent = %agent_name, "session created");
    Ok(())
}

pub(crate) async fn ensure_session(
    node: &EmbeddedNode,
    session_id: &str,
    agent_name: &str,
) -> Result<()> {
    if session_exists(node, session_id).await? {
        return Ok(());
    }

    create_session_with_id(node, session_id, agent_name).await
}

pub(crate) async fn session_exists(node: &EmbeddedNode, session_id: &str) -> Result<bool> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentSession(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                limit: 1
            ) {{ _docID }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "session_exists").await;
    if resp.has_errors() {
        anyhow::bail!(
            "checking session existence for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentSession"))
        .and_then(|value| value.as_array())
        .is_some_and(|sessions| !sessions.is_empty()))
}

pub(crate) async fn max_sequence(node: &EmbeddedNode, session_id: &str) -> Result<u32> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                order: {{ sequence: DESC }},
                limit: 1
            ) {{ sequence }}
        }}"#
    );

    let resp = execute_query_timed(node, &query, "max_sequence").await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading max sequence for session_id={}: {:?}",
            session_id,
            resp.errors
        );
    }

    Ok(resp
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|message| message.get("sequence"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32)
}

pub async fn close_session(node: &EmbeddedNode, session_id: &str) -> Result<()> {
    retry_operation("close_session", || async {
        let session = load_session_document(node, session_id).await?;

        let now = chrono::Utc::now().to_rfc3339();
        let escaped_started = escape_graphql_string(&session.started);
        let mutation = format!(
            r#"mutation {{
                update_AgentSession(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{
                        started: "{escaped_started}",
                        status: "completed",
                        ended: "{now}"
                    }}
                ) {{ _docID }}
            }}"#,
            doc_id = session.doc_id,
        );

        let started_at = std::time::Instant::now();
        let resp = node.execute(&mutation).await;
        log_mutation_timing("close_session", started_at.elapsed());

        if !resp.has_errors() {
            return Ok(());
        }

        anyhow::bail!("close_session mutation failed: {:?}", resp.errors)
    })
    .await?;

    tracing::info!(session_id = %session_id, "session closed");
    Ok(())
}
