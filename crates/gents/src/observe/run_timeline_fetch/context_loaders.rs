use super::*;

pub(super) async fn load_timeline_session(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<TimelineSessionRow>> {
    let scope = gents_protocol::session::session_scope_filter(node_did, session_id, requester_did);
    let fields = gents_protocol::session::AGENT_SESSION_FIELDS;
    let query = format!("{{ AgentSession(filter: {{ {scope} }}, limit: 2) {{ {fields} }} }}");
    let rows = load_rows::<serde_json::Value>(access, "AgentSession", &query).await?;
    decode_timeline_session_rows(rows, node_did, session_id, requester_did)
}

fn decode_timeline_session_rows(
    rows: Vec<serde_json::Value>,
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
) -> Result<Option<TimelineSessionRow>> {
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous owner/requester-scoped timeline session"
    );
    rows.first()
        .map(|row| {
            let row = gents_protocol::session::decode_session_row(row)?;
            anyhow::ensure!(
                row.session.node_did == node_did
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
        use crate::store::identity::NodeIdentity;
        use gents_protocol::session::{SessionProvenance, SessionTitle, SessionTitleSource};

        let (first, user_a, access_a, binding_a) =
            crate::test_support::signed_test_node("timeline-scope-a").await;
        let (second, user_b, access_b, binding_b) =
            crate::test_support::signed_test_node("timeline-scope-b").await;
        let owner_a = first.node_identity.did();
        let owner_b = second.node_identity.did();
        assert_ne!(owner_a, owner_b);
        assert_ne!(user_a.did(), user_b.did());
        for (access, owner, user, binding) in [
            (&access_a, owner_a, &user_a, &binding_a),
            (&access_b, owner_b, &user_b, &binding_b),
        ] {
            access
                .transact_write("test.timeline_scope.session", |txn| {
                    Box::pin(async move {
                        crate::session::ensure_session_in_txn(
                            txn,
                            "shared-label",
                            owner,
                            &binding.agent_did,
                            Some(user.did()),
                            Some(SessionTitle {
                                text: "A title".into(),
                                source: SessionTitleSource::User,
                            }),
                            Some(SessionProvenance {
                                task_id: Some("task".into()),
                                ..Default::default()
                            }),
                            "2026-01-01T00:00:00Z",
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap();
            let (session_doc, agent_doc) = access
                .transact_readonly("test.timeline_scope.targets", |txn| {
                    Box::pin(async move {
                        let session = crate::store::config_client::agent_session::load_agent_session_owner_in_txn(
                            txn,
                            owner,
                            "shared-label",
                        )
                        .await?
                        .expect("session");
                        let (agent, _) =
                            crate::store::config_client::read_desired_state_record_in_txn(
                                txn,
                                crate::Collection::Agent,
                                owner,
                                &binding.agent_did,
                            )
                            .await?
                            .expect("agent");
                        Ok::<_, anyhow::Error>((session.doc_id, agent))
                    })
                })
                .await
                .unwrap();
            crate::session::grant_created_session_relationships(
                access,
                &session_doc,
                &agent_doc,
                &binding.agent_did,
            )
            .await
            .unwrap();
        }
        let first_row =
            load_timeline_session(&access_a, owner_a, "shared-label", Some(user_a.did()))
                .await
                .unwrap()
                .unwrap();
        let second_row =
            load_timeline_session(&access_b, owner_b, "shared-label", Some(user_b.did()))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(first_row.session.agent_did, binding_a.agent_did);
        assert_eq!(second_row.session.agent_did, binding_b.agent_did);
        assert!(first_row.doc_id.is_some());
        assert!(second_row.doc_id.is_some());
        assert_ne!(first_row.doc_id, second_row.doc_id);
        assert_eq!(first_row.session.title.as_ref().unwrap().text, "A title");
        assert_eq!(
            first_row
                .session
                .provenance
                .as_ref()
                .unwrap()
                .task_id
                .as_deref(),
            Some("task")
        );
        assert!(
            load_timeline_session(&access_a, owner_a, "shared-label", None)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            load_timeline_session(&access_a, owner_a, "shared-label", Some(user_b.did()))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            load_timeline_session(&access_a, owner_b, "shared-label", Some(user_b.did()))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            load_timeline_session(&access_b, owner_a, "shared-label", Some(user_a.did()))
                .await
                .unwrap()
                .is_none()
        );
        first.node.shutdown().await.expect("first node shutdown");
        second.node.shutdown().await.expect("second node shutdown");
    }
}
