use super::*;

pub(super) async fn load_timeline_session(
    access: &ConfigAccess,
    agent_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<TimelineSessionRow>> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let fields = crate::session::AGENT_SESSION_FIELDS;
    let query = format!("{{ AgentSession(filter: {{ {scope} }}, limit: 2) {{ {fields} }} }}");
    let rows = load_rows::<serde_json::Value>(access, "AgentSession", &query).await?;
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous owner/requester-scoped timeline session"
    );
    rows.first()
        .map(|row| {
            let row = crate::session::decode_session_row(row)?;
            anyhow::ensure!(
                row.session.agent_did == agent_did
                    && row.session.session_id == session_id
                    && row.session.requester_did.as_deref() == requester_did,
                "timeline session is outside authoritative request scope"
            );
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
                session_id: "shared-label", agent_did: "{owner}", requester_did: {requester},
                behavior_id: "{behavior}", created_at: "2026-01-01T00:00:00Z",
                title: {{text: "A title", source: "user"}},
                provenance: {{task_id: "task"}},
                observation: {{last_activity_at: "2026-01-02T00:00:00Z", preview: "A preview"}}
            }}) {{_docID}} }}"#
            );
            let response = node.execute(&query).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
        }
        let access = ConfigAccess::Local(node.clone());
        let owner_a = load_timeline_session(&access, "owner-a", "shared-label", None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(owner_a.session.behavior_id, "behavior-a");
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
        assert_eq!(owner_b.session.behavior_id, "behavior-b");
        assert_ne!(owner_a.doc_id, owner_b.doc_id);
        assert!(
            load_timeline_session(&access, "absent", "shared-label", None)
                .await
                .unwrap()
                .is_none()
        );
        node.shutdown().await;
    }
}
