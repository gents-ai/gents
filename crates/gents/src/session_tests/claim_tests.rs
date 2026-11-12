use gents_store::graphql::escape_graphql_string;

#[tokio::test]
async fn pending_interrupt_and_expiry_release_workspace_bindings_atomically() {
    use crate::session_tests::admission_workspace_cleanup_tests::Fixture;

    for interrupted in [true, false] {
        let fixture = Fixture::new().await;
        let (_, mutations) =
            gents_store::config_client::ConfigApplyTxn::assert_every_mutation_rolls_back(
                || async {
                    let mut lifecycle = fixture.lifecycle();
                    if interrupted {
                        lifecycle
                            .transition_pending_to_interrupted("2026-09-01T00:00:01Z")
                            .await
                    } else {
                        lifecycle.transition_pending_to_dead_stale().await
                    }
                },
                || fixture.observe(),
            )
            .await;
        assert!(
            mutations >= 2,
            "terminal and binding must share the transaction"
        );
        let observed = fixture.observe().await;
        assert_eq!(
            observed["AgentRequest"][0]["lifecycle_state"],
            if interrupted { "interrupted" } else { "dead" }
        );
        assert_eq!(
            observed["WorkspaceBinding"][0]["lifecycle_state"],
            "released"
        );

        fixture.write_binding(&fixture.binding).await;
        let before_replay = fixture.observe().await;
        let mut replay = fixture.lifecycle();
        if interrupted {
            replay
                .transition_pending_to_interrupted("2026-09-01T00:00:01Z")
                .await
                .unwrap();
        } else {
            replay.transition_pending_to_dead_stale().await.unwrap();
        }
        assert_eq!(
            fixture.observe().await,
            before_replay,
            "replayed terminal cannot release again"
        );
    }
}

#[tokio::test]
async fn pending_interrupt_and_expiry_cannot_release_a_claimed_workspace_binding() {
    use crate::session_tests::admission_workspace_cleanup_tests::Fixture;

    for interrupted in [true, false] {
        let fixture = Fixture::new().await;
        let doc_id = escape_graphql_string(&fixture.request.doc_id);
        fixture
            .access
            .write(
                "test.pending_workspace_terminal_lost_cas",
                &format!(
                    r#"mutation {{ update_AgentRequest(
                    filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                    input: {{ lifecycle_state: "claimed" }}
                ) {{ _docID }} }}"#
                ),
            )
            .await
            .unwrap();
        let before = fixture.observe().await;
        let mut lifecycle = fixture.lifecycle();
        let result = if interrupted {
            lifecycle
                .transition_pending_to_interrupted("2026-09-01T00:00:01Z")
                .await
        } else {
            lifecycle.transition_pending_to_dead_stale().await
        };
        assert!(
            result.is_err(),
            "a claimed request lost the pending terminal CAS"
        );
        assert_eq!(fixture.observe().await, before);
    }
}
