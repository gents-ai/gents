use super::error::{Result, RunTimelineError};
use super::*;

pub(super) async fn load_timeline_session(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> anyhow::Result<Option<TimelineSessionRow>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let fields = crate::session::AGENT_SESSION_FIELDS;
    let query = format!("{{ AgentSession(filter: {{ {scope} }}, limit: 2) {{ {fields} }} }}");
    let rows = load_rows::<serde_json::Value>(access, "AgentSession", &query).await?;
    Ok(decode_timeline_session_rows(
        rows,
        node_did,
        session_id,
        requester_did,
    )?)
}

pub(super) async fn load_timeline_sessions(
    access: &ConfigAccess,
    scopes: &[(String, String, Option<String>)],
) -> anyhow::Result<Vec<TimelineSessionRow>> {
    const BATCH_SIZE: usize = 64;
    let fields = crate::session::AGENT_SESSION_FIELDS;
    let mut sessions = Vec::new();
    for batch in scopes.chunks(BATCH_SIZE) {
        let selections = batch
            .iter()
            .enumerate()
            .map(|(index, (node_did, session_id, requester_did))| {
                let scope = crate::session::session_scope_filter(
                    node_did,
                    session_id,
                    requester_did.as_deref(),
                );
                format!("s{index}: AgentSession(filter: {{ {scope} }}, limit: 2) {{ {fields} }}")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let response = access.execute(&format!("{{ {selections} }}")).await?;
        anyhow::ensure!(
            response.get("errors").is_none_or(
                |errors| errors.is_null() || errors.as_array().is_some_and(Vec::is_empty)
            ),
            "timeline AgentSession query failed: {}",
            response["errors"]
        );
        let data = response
            .get("data")
            .context("timeline AgentSession query omitted data")?;
        for (index, (node_did, session_id, requester_did)) in batch.iter().enumerate() {
            let alias = format!("s{index}");
            let rows = data
                .get(alias.as_str())
                .and_then(serde_json::Value::as_array)
                .with_context(|| format!("timeline query omitted AgentSession scope s{index}"))?
                .clone();
            if let Some(session) =
                decode_timeline_session_rows(rows, node_did, session_id, requester_did.as_deref())?
            {
                sessions.push(session);
            }
        }
    }
    Ok(sessions)
}

fn decode_timeline_session_rows(
    rows: Vec<serde_json::Value>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<TimelineSessionRow>> {
    if rows.len() > 1 {
        return Err(RunTimelineError::AmbiguousSessionScope);
    }
    rows.first()
        .map(|row| {
            let row = crate::session::decode_session_row(row)?;
            if row.session.node_did != node_did
                || row.session.session_id != session_id
                || row.session.requester_did.as_deref() != requester_did
            {
                return Err(RunTimelineError::SessionOutsideScope);
            }
            Ok(TimelineSessionRow {
                doc_id: Some(row.doc_id),
                session: row.session,
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canonical_session_reads_require_exact_owner_and_requester_scope() {
        let node = std::sync::Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        node.add_schema(gents_protocol::schemas::AGENT_SESSION)
            .await
            .unwrap();
        for (owner, requester, behavior) in [
            ("owner-a", "null", "behavior-a"),
            ("owner-b", "\"requester-b\"", "behavior-b"),
        ] {
            let query = format!(
                r#"mutation {{ create_AgentSession(input: {{
                session_id: "shared-label", node_did: "{owner}", requester_did: {requester},
                agent_id: "{behavior}", created_at: "2026-01-01T00:00:00Z",
                title: {{text: "A title", source: "user"}},
                provenance: {{task_id: "task"}},
                observation: {{last_activity_at: "2026-01-02T00:00:00Z", preview: "A preview"}}
            }}) {{_docID}} }}"#
            );
            let response = node.execute(&query).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let access = ConfigAccess::local(node.clone());
        let owner_a = load_timeline_session(&access, "owner-a", "shared-label", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner_a.session.agent_id, "behavior-a");
        assert!(owner_a.doc_id.is_some());
        assert_eq!(owner_a.session.title.as_ref().unwrap().text, "A title");
        assert_eq!(
            owner_a
                .session
                .provenance
                .as_ref()
                .unwrap()
                .task_id
                .as_deref(),
            Some("task")
        );
        assert!(
            load_timeline_session(&access, "owner-b", "shared-label", None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            load_timeline_session(&access, "owner-a", "shared-label", Some("requester-b"))
                .await
                .unwrap()
                .is_none()
        );
        let owner_b =
            load_timeline_session(&access, "owner-b", "shared-label", Some("requester-b"))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(owner_b.session.agent_id, "behavior-b");
        assert_ne!(owner_a.doc_id, owner_b.doc_id);
        let batched = load_timeline_sessions(
            &access,
            &[
                ("owner-a".to_owned(), "shared-label".to_owned(), None),
                (
                    "owner-b".to_owned(),
                    "shared-label".to_owned(),
                    Some("requester-b".to_owned()),
                ),
            ],
        )
        .await
        .unwrap();
        assert_eq!(batched.len(), 2);
        assert_eq!(batched[0].session.agent_id, "behavior-a");
        assert_eq!(batched[1].session.agent_id, "behavior-b");
        assert!(
            load_timeline_session(&access, "absent", "shared-label", None)
                .await
                .unwrap()
                .is_none()
        );
        node.shutdown().await;
    }
}
