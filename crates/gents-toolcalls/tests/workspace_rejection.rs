#![cfg(target_os = "macos")]

use gents_store::config_client::ConfigApplyTxn;
use gents_store::graphql::escape_graphql_string;
use serde_json::Value;

use gents_toolcalls::test_support::workspace_rejection::{Fixture, RejectionOwner};
#[tokio::test]
async fn malformed_pending_request_still_rejects_without_cleanup_provenance() {
    for field in ["request_id", "workspace_id", "workspace_owner_node_did"] {
        let mut fixture = Fixture::new().await;
        fixture.request.request_id = "malformed-workspace-request".to_string();
        let mut input = serde_json::json!({
            "request_id": fixture.request.request_id,
            "node_did": fixture.request.node_did,
            "agent_did": fixture.request.agent_did,
            "purpose": "normal",
            "lifecycle_state": "pending",
            "workspace_id": fixture.binding.workspace_id,
            "workspace_owner_node_did": fixture.binding.owner_node_did,
        });
        input[field] = Value::Null;
        let input = gents_protocol::graphql::graphql_input_literal(&input).unwrap();
        let created = fixture
            .access
            .write(
                "test.malformed_workspace_rejection",
                &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
            )
            .await
            .unwrap();
        fixture.request.doc_id =
            gents_store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
        fixture.binding.binding_id = "malformed-binding".to_string();
        fixture.binding.request_id = fixture.request.request_id.clone();
        fixture.binding.request_doc_id = fixture.request.doc_id.clone();
        fixture.write_binding(&fixture.binding).await;
        fixture
            .reject(RejectionOwner::FreshAdmission)
            .await
            .unwrap();
        let observed = fixture.observe().await;
        assert_eq!(
            observed["AgentRequest"][0]["lifecycle_state"], "failed",
            "missing {field}"
        );
        assert!(
            observed["WorkspaceBinding"]
                .as_array()
                .unwrap()
                .iter()
                .all(|binding| binding["lifecycle_state"] == "active"),
            "missing {field}"
        );
    }
}

#[tokio::test]
async fn pending_rejection_releases_only_its_exact_binding_and_only_once() {
    for owner in [
        RejectionOwner::FreshAdmission,
        RejectionOwner::ClaimAdmission,
    ] {
        let fixture = Fixture::new().await;
        for field in ["workspace", "node", "logical_request", "physical_request"] {
            let mut foreign = fixture.binding.clone();
            foreign.binding_id = format!("foreign-{field}");
            match field {
                "workspace" => foreign.workspace_id.push_str("-other"),
                "node" => foreign.owner_node_did.push_str("-other"),
                "logical_request" => foreign.request_id.push_str("-other"),
                "physical_request" => foreign.request_doc_id.push_str("-other"),
                _ => unreachable!(),
            }
            fixture.write_binding(&foreign).await;
        }
        fixture.reject(owner).await.unwrap();
        let observed = fixture.observe().await;
        assert_eq!(
            observed["AgentRequest"][0]["lifecycle_state"], "failed",
            "{owner:?}"
        );
        for binding in observed["WorkspaceBinding"].as_array().unwrap() {
            assert_eq!(
                binding["lifecycle_state"],
                if binding["binding_id"] == "exact-binding" {
                    "released"
                } else {
                    "active"
                },
                "{owner:?}: {binding}",
            );
        }

        fixture.write_binding(&fixture.binding).await;
        let before_replay = fixture.observe().await;
        fixture.reject(owner).await.unwrap();
        assert_eq!(fixture.observe().await, before_replay, "{owner:?}");
    }
}

#[tokio::test]
async fn pending_rejection_and_binding_release_roll_back_together() {
    for owner in [
        RejectionOwner::FreshAdmission,
        RejectionOwner::ClaimAdmission,
    ] {
        let fixture = Fixture::new().await;
        let (_, mutations) = ConfigApplyTxn::assert_every_mutation_rolls_back(
            || fixture.reject(owner),
            || fixture.observe(),
        )
        .await;
        assert!(
            mutations >= 2,
            "{owner:?}: terminal and binding must share the transaction"
        );
        let observed = fixture.observe().await;
        assert_eq!(observed["AgentRequest"][0]["lifecycle_state"], "failed");
        assert_eq!(
            observed["WorkspaceBinding"][0]["lifecycle_state"],
            "released"
        );
    }
}

#[tokio::test]
async fn pending_rejection_cannot_release_a_request_that_was_claimed() {
    for owner in [
        RejectionOwner::FreshAdmission,
        RejectionOwner::ClaimAdmission,
    ] {
        let fixture = Fixture::new().await;
        let doc_id = escape_graphql_string(&fixture.request.doc_id);
        fixture
            .access
            .write(
                "test.workspace_rejection_lost_cas",
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
        if matches!(owner, RejectionOwner::FreshAdmission) {
            assert_eq!(
                fixture
                    .rejection_outcome(&fixture.request.doc_id)
                    .await
                    .unwrap(),
                gents_session::request::admission::PendingRejectionOutcome::NotMatched
            );
        }
        let result = fixture.reject(owner).await;
        assert_eq!(
            result.is_ok(),
            matches!(owner, RejectionOwner::FreshAdmission)
        );
        assert_eq!(fixture.observe().await, before, "{owner:?}");
    }
}

#[tokio::test]
async fn pending_rejection_reports_only_an_applied_update() {
    use gents_session::request::admission::PendingRejectionOutcome;

    let fixture = Fixture::new().await;
    let created = fixture.access.write(
        "test.workspace_rejection_absent_create",
        r#"mutation { create_AgentRequest(input: { request_id: "deleted-request" }) { _docID } }"#,
    )
    .await
    .unwrap();
    let absent = gents_store::graphql::created_doc_id(&created, "AgentRequest").unwrap();
    fixture
        .access
        .write(
            "test.workspace_rejection_absent_delete",
            &format!(
                r#"mutation {{ delete_AgentRequest(docID: "{}") {{ _docID }} }}"#,
                escape_graphql_string(&absent)
            ),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.rejection_outcome(&absent).await.unwrap(),
        PendingRejectionOutcome::NotMatched
    );
    assert_eq!(
        fixture
            .rejection_outcome(&fixture.request.doc_id)
            .await
            .unwrap(),
        PendingRejectionOutcome::Terminalized
    );
    let before_replay = fixture.observe().await;
    assert_eq!(
        fixture
            .rejection_outcome(&fixture.request.doc_id)
            .await
            .unwrap(),
        PendingRejectionOutcome::NotMatched
    );
    assert_eq!(fixture.observe().await, before_replay);
}

#[tokio::test]
async fn pending_rejection_reports_write_failure_and_rolls_back() {
    let fixture = Fixture::new().await;
    let before = fixture.observe().await;
    let (result, mutations) = ConfigApplyTxn::with_successful_mutation_failure_at(
        Some(1),
        gents_session::request::admission::terminalize_pending_request_rejection(
            &fixture.node,
            &fixture.request.doc_id,
            &fixture.request.node_did,
            &fixture.request.agent_did,
            fixture.agent.as_ref(),
            "workspace placement is unavailable",
            "test.workspace_rejection_write_error",
        ),
    )
    .await;
    assert_eq!(mutations, 1);
    assert!(matches!(
        result,
        Err(gents_session::request::admission::PendingRejectionError::Transaction(_))
    ));
    assert_eq!(fixture.observe().await, before);
}

#[tokio::test]
async fn pending_interrupt_and_expiry_release_workspace_bindings_atomically() {
    use gents_toolcalls::test_support::workspace_rejection::Fixture;

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
    use gents_toolcalls::test_support::workspace_rejection::Fixture;

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
