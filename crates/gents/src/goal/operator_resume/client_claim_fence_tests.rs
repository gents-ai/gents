use super::support::{request_rows, Fixture, PARENT, SESSION};
use super::*;
use crate::request::admission::AgentRequestAdmissionVerifier;
use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::store::config_client::{ConfigAccess, NodePermission};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_input::GoalAssignmentInput;

async fn update_goal_root(access: &ConfigAccess, doc_id: &str, root: Option<&str>) {
    let mutation = format!(
        r#"mutation($input: GoalMutationInputArg!) {{ update_Goal(docID: "{}", input: $input) {{ _docID }} }}"#,
        escape_graphql_string(doc_id),
    );
    let variables = serde_json::json!({"input": {"assignment_root_request_doc_id": root}});
    let response = access
        .transact_write("test.goal.update_root", |txn| {
            let mutation = &mutation;
            let variables = &variables;
            Box::pin(async move {
                txn.execute_client_signed_update(mutation, variables, gents_protocol::schemas::GOAL)
                    .await
            })
        })
        .await
        .unwrap();
    assert_eq!(response["data"]["update_Goal"].as_array().unwrap().len(), 1);
    assert_eq!(response["data"]["update_Goal"][0]["_docID"], doc_id);
}

#[tokio::test]
async fn user_goal_continuation_claim_fences_current_physical_binding() {
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let before = &snapshot["goal_operator_resume_cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "atomic_publication")
        .unwrap()["before"];

    for scenario in [
        "valid",
        "rootless",
        "dangling-root",
        "missing",
        "hidden",
        "stale",
        "changed-edge",
        "changed-session",
    ] {
        let f = Fixture::new_with_goal_author(before, true, None, true).await;
        let user = f.user_access();
        let agent = f.agent_access();
        let admin = ConfigAccess::local_as(
            f.node.clone(),
            ::identity::Did::new(f.identity.did()).unwrap(),
        );
        let user_did = ::identity::Did::new(f.user.did()).unwrap();
        for permission in [
            NodePermission::DocumentDelete,
            NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&user_did, permission)
                .await
                .unwrap();
        }
        let receipt =
            resume_goal_request(&user, f.user.as_ref(), f.identity.did(), SESSION, PARENT)
                .await
                .unwrap();
        let row = request_rows(&agent)
            .await
            .into_iter()
            .find(|row| row.request_id == receipt.request_id)
            .unwrap();
        let request = crate::request::watcher::AgentRequest::try_from(row).unwrap();
        let (_owner, authority) =
            crate::runtime::agent::p2p_reconcile::enrollment_authority_channel();
        let verifier =
            AgentRequestAdmissionVerifier::new(f.node.clone(), f.identity.clone(), authority);
        let (request, client_claim) = verifier
            .verify_fresh_for_claim_with_identity(&request, f.agent.as_ref())
            .await
            .unwrap();
        assert!(
            client_claim.is_some(),
            "{scenario}: User admission was not retained"
        );

        if scenario == "changed-edge" || scenario == "changed-session" {
            let fields = if scenario == "changed-edge" {
                r#"caused_by_parent_request_doc_id: "foreign-document""#
            } else {
                r#"session_id: "foreign-session""#
            };
            let changed = agent.write("test.goal.changed_edge", &format!(
                r#"mutation {{ update_AgentRequest(docID: "{}", input: {{ {fields} }}) {{ _docID }} }}"#,
                escape_graphql_string(&request.doc_id),
            )).await.unwrap();
            assert_eq!(
                changed["data"]["update_AgentRequest"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert!(verifier
                .verify_fresh_for_claim_with_identity(&request, f.agent.as_ref())
                .await
                .is_err());
            f.node.shutdown().await.unwrap();
            continue;
        }

        if scenario == "rootless" || scenario == "dangling-root" {
            let root = if scenario == "rootless" {
                None
            } else {
                Some("missing-root-document")
            };
            update_goal_root(&user, &f.goal.doc_id, root).await;
        } else if scenario == "missing" {
            let goal_doc = &f.goal.doc_id;
            user.transact_write("test.goal.delete", |txn| {
                Box::pin(async move { txn.delete_configuration_document("Goal", goal_doc).await })
            })
            .await
            .unwrap();
        } else if scenario == "hidden" {
            user.delete_resource_relationship(
                "Goal",
                &f.goal.doc_id,
                "parent",
                "sessions",
                &f.session_doc_id,
            )
            .await
            .unwrap();
            user.delete_actor_relationship(
                "Goal",
                &f.goal.doc_id,
                "working_agent",
                &::identity::Did::new(f.agent.did()).unwrap(),
            )
            .await
            .unwrap();
            let response = agent
                .execute(&format!(
                    r#"{{ Goal(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                    escape_graphql_string(&f.goal.doc_id),
                ))
                .await
                .unwrap();
            assert!(response["data"]["Goal"].as_array().unwrap().is_empty());
        } else if scenario == "stale" {
            let mut opening = AgentRequestCreate::base(
                RequestPurpose::Normal,
                "new-goal-opening",
                f.identity.did(),
                f.user.did(),
                f.agent.did(),
                SESSION,
                "new assignment",
                "interactive",
                "2030-01-01T00:00:00Z",
                AgentRequestAdmissionRecord::client(f.user.did()),
            );
            opening.session_doc_id = Some(f.session_doc_id.clone());
            opening.agent_doc_id = Some(f.agent_doc_id.clone());
            opening.input.goal_assignment = Some(GoalAssignmentInput {
                goal_id: f.goal.goal_id.clone(),
                root_request_doc_id: None,
            });
            crate::sign_agent_request_create(f.user.as_ref(), &mut opening)
                .await
                .unwrap();
            crate::request::lifecycle::submit_client_request(&user, &opening)
                .await
                .unwrap();
            let new_root = request_rows(&user)
                .await
                .into_iter()
                .find(|row| row.request_id == opening.request_id)
                .unwrap()
                .doc_id
                .unwrap();
            update_goal_root(&user, &f.goal.doc_id, Some(&new_root)).await;
        }

        let mut lifecycle = RequestLifecycle::new_with_node_did(
            f.node.clone(),
            f.agent.did(),
            f.identity.did(),
            request.clone(),
            60,
        );
        lifecycle.bind_managed_agent(f.agent.as_ref()).unwrap();
        lifecycle.set_client_claim_admission(client_claim);
        let result = lifecycle.claim_with_identity().await;
        match scenario {
            "valid" => assert_eq!(result.unwrap(), ClaimOutcome::Claimed),
            "stale" => assert_ne!(result.unwrap(), ClaimOutcome::Claimed),
            _ => assert!(result.is_err(), "{scenario}: {result:?}"),
        }
        let current = request_rows(&user)
            .await
            .into_iter()
            .find(|row| row.request_id == request.request_id)
            .unwrap();
        if scenario == "stale" {
            assert_eq!(
                current.lifecycle_state,
                Some(RequestLifecycleState::Superseded)
            );
        } else if scenario != "valid" {
            assert_eq!(current.lifecycle_state, Some(RequestLifecycleState::Failed));
        }
        drop(lifecycle);
        f.node.shutdown().await.unwrap();
    }
}
