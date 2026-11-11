use super::*;
use anyhow::Context;
use gents_protocol::session::{decode_session_row, AGENT_SESSION_FIELDS};

pub async fn session_stopped_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    use gents_protocol::document_query::{
        fields::agent_session as session, session_scope, Query, Selection,
    };
    let rendered = Query::select(Selection::new().field(session::STOPPED_AT))
        .filter(session_scope(node_did, session_id, requester_did))
        .limit(2)
        .render();
    let response = txn.run(&rendered).await?;
    let rows = response
        .pointer("/data/AgentSession")
        .and_then(serde_json::Value::as_array)
        .context("session stop-latch query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "session stop-latch scope is ambiguous");
    let Some(row) = rows.first() else {
        return Ok(false);
    };
    match row
        .get("stopped_at")
        .context("session stop-latch query omitted stopped_at")?
    {
        serde_json::Value::Null => Ok(false),
        serde_json::Value::String(value) if !value.trim().is_empty() => Ok(true),
        _ => anyhow::bail!("session stopped_at is malformed"),
    }
}

pub async fn session_is_stopped(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: &::identity::Did,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        Some(actor.clone()),
        "session.stop_latch",
        |txn| {
            Box::pin(session_stopped_in_txn(
                txn,
                node_did,
                session_id,
                requester_did,
            ))
        },
    )
    .await
}

pub fn public_request_filter(scope: &str) -> String {
    let purpose = format!(
        r#"purpose: {{ _eq: "{}" }}"#,
        gents_protocol::request_admission::RequestPurpose::PUBLIC.as_str()
    );
    if scope.is_empty() {
        purpose
    } else {
        format!("{scope}, {purpose}")
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(super) async fn load_agent_session_row(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<gents_protocol::session::SessionOwnerRow>> {
    load_agent_session_rows(node, node_did, session_id, requester_did)
        .await
        .and_then(|mut rows| {
            anyhow::ensure!(rows.len() <= 1, "duplicate AgentSession rows for one label");
            Ok(rows.pop())
        })
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(super) async fn load_agent_session_rows(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Vec<gents_protocol::session::SessionOwnerRow>> {
    use gents_protocol::document_query::{
        fields::agent_session as fields, session_scope, Query, Selection,
    };
    let rendered = Query::<fields::Doc>::select(Selection::owned(AGENT_SESSION_FIELDS))
        .filter(session_scope(node_did, session_id, requester_did))
        .render();

    let resp =
        gents_store::graphql::read_rendered(node, &rendered, "load_agent_session_rows").await?;

    let rows = gents_store::graphql::rows::<serde_json::Value>(&resp, "AgentSession")?;
    rows.iter()
        .map(|row| decode_session_row(row).map_err(anyhow::Error::from))
        .collect()
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn load_agent_session(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<gents_protocol::session::AgentSession>> {
    Ok(
        load_agent_session_row(node, node_did, session_id, requester_did)
            .await?
            .map(|row| row.session),
    )
}

/// The requester that owns the existing session `(node_did, session_id)`, or
/// `None` when no session exists yet. Runtime deliveries into an existing
/// session adopt it (Lean `Enrollment.runtimeRequesterScope`).
pub async fn load_session_requester_scope(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    agent_did: &str,
) -> Result<Option<String>> {
    use gents_protocol::document_query::{
        fields::agent_session as session, Filter, Query, Selection,
    };
    let rendered = Query::select(Selection::owned(AGENT_SESSION_FIELDS))
        .filter(
            Filter::eq(session::NODE_DID, node_did)
                .and(Filter::eq(session::SESSION_ID, session_id))
                .and(Filter::eq(session::AGENT_DID, agent_did)),
        )
        .render();
    let response = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(agent_did)?,
    )
    .read(&rendered)
    .await?;
    let rows = response
        .pointer("/data/AgentSession")
        .and_then(serde_json::Value::as_array)
        .context("session requester scope query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "duplicate AgentSession rows for one label");
    Ok(match rows.first() {
        Some(row) => decode_session_row(row)?.session.requester_did,
        None => None,
    })
}

pub async fn require_session(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: Option<::identity::Did>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let row = gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        actor,
        "session.require_for_completion",
        |txn| {
            Box::pin(
                gents_store::config_client::agent_session::load_agent_session_row_in_txn(
                    txn,
                    node_did,
                    session_id,
                    requester_did,
                ),
            )
        },
    )
    .await?;
    anyhow::ensure!(
        row.is_some(),
        "loading session for completion: no AgentSession for session_id={session_id}"
    );
    Ok(())
}

pub async fn load_session_agent_selection(
    node: &std::sync::Arc<EmbeddedNode>,
    actor: &::identity::Did,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    Ok(
        gents_store::config_client::ConfigAccess::transact_local_readonly(
            node,
            Some(actor.clone()),
            "session.agent_selection",
            |txn| {
                Box::pin(
                    gents_store::config_client::agent_session::load_agent_session_row_in_txn(
                        txn,
                        node_did,
                        session_id,
                        requester_did,
                    ),
                )
            },
        )
        .await?
        .map(|row| row.session.agent_did),
    )
}

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn load_session_agent_did(
    node: &std::sync::Arc<EmbeddedNode>,
    node_did: &str,
    session_id: &str,
) -> Result<::identity::Did> {
    let row = gents_store::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "session.resolve_agent_actor",
        |txn| {
            Box::pin(
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn, node_did, session_id,
                ),
            )
        },
    )
    .await?
    .context("cannot select an agent for a missing session")?;
    Ok(::identity::Did::new(row.session.agent_did)?)
}

pub async fn client_requester_scope(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    session_id: &str,
    actor_did: &str,
) -> Result<String> {
    access
        .transact_readonly("session.client_requester_scope", |txn| {
            Box::pin(async move {
                match gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn, node_did, session_id,
                )
                .await?
                {
                    Some(row) => row
                        .session
                        .requester_did
                        .context("existing session has no requester provenance"),
                    None => Ok(actor_did.to_owned()),
                }
            })
        })
        .await
}

#[cfg(test)]
#[tokio::test]
async fn stop_latch_reads_as_the_selected_actor() {
    use gents_store::identity::NodeIdentity as _;
    let home = tempfile::tempdir().unwrap();
    let runtime =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("runtime.key"), None)
            .unwrap();
    let user =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("user.key"), None)
            .unwrap();
    let agent =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("agent.key"), None)
            .unwrap();
    let other =
        gents_store::identity::KeyIdentity::load_or_create(home.path().join("other.key"), None)
            .unwrap();
    let node = std::sync::Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(runtime.did())
            .build()
            .await
            .unwrap(),
    );
    gents_store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    // Embedded writes need the node identity or a managed Agent; the requester
    // user is recorded as data on the session, not used as the writer.
    let access = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        ::identity::Did::new(runtime.did()).unwrap(),
    );
    let doc = access.transact_write("test.session.stop_latch", |txn| {
        Box::pin(async {
            let response = txn.execute_with_variables(
                "mutation($input: AgentSessionMutationInputArg!) { create_AgentSession(input: $input) { _docID } }",
                &serde_json::json!({"input": {"node_did": runtime.did(), "session_id": "stopped", "agent_did": agent.did(), "requester_did":user.did(), "stopped_at":"2026-11-04T00:00:00Z"}}),
            ).await?;
            let doc = gents_store::graphql::created_doc_id(&response, "AgentSession")?;
            Ok::<_, anyhow::Error>(doc)
        })
    }).await.unwrap();
    access
        .add_actor_relationship(
            "AgentSession",
            &doc,
            "working_agent",
            &::identity::Did::new(agent.did()).unwrap(),
        )
        .await
        .unwrap();
    assert!(session_is_stopped(
        &node,
        &::identity::Did::new(agent.did()).unwrap(),
        runtime.did(),
        "stopped",
        Some(user.did())
    )
    .await
    .unwrap());
    assert!(!session_is_stopped(
        &node,
        &::identity::Did::new(other.did()).unwrap(),
        runtime.did(),
        "stopped",
        Some(user.did())
    )
    .await
    .unwrap());
    node.shutdown().await.expect("shut down test node");
}
