use std::sync::Arc;

use gents_store::config_client::{
    decide_grant, CompletionDecision, ConfigAccess, ConfigClientError, ConfigRelationshipGrant,
    GrantOrigin,
};
use gents_store::identity::NodeIdentity as _;
use gents_store::node_extension::Extensions;
use serde::{Deserialize, Serialize};
use tokio::sync::Barrier;

use crate::test_support::{AdmitAllProfiles, SignedHome};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ModelOrigin {
    Detached,
    Explicit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ModelDecision {
    Grant,
    SkipRevoked,
    RefuseActorMismatch,
}

fn map_origin(origin: ModelOrigin) -> GrantOrigin {
    match origin {
        ModelOrigin::Detached => GrantOrigin::Detached,
        ModelOrigin::Explicit => GrantOrigin::Explicit,
    }
}

fn map_decision(decision: CompletionDecision) -> ModelDecision {
    match decision {
        CompletionDecision::Grant => ModelDecision::Grant,
        CompletionDecision::SkipRevoked => ModelDecision::SkipRevoked,
        CompletionDecision::RefuseActorMismatch => ModelDecision::RefuseActorMismatch,
    }
}

async fn agent_doc_id(home: &SignedHome, agent_did: &str) -> String {
    let query = format!(
        r#"{{ Agent(filter: {{ node_did: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
        gents_store::graphql::escape_graphql_string(home.node_did()),
        gents_store::graphql::escape_graphql_string(agent_did),
    );
    let response = home.user_access.execute(&query).await.unwrap();
    response["data"]["Agent"][0]["_docID"]
        .as_str()
        .expect("created Agent document id")
        .to_owned()
}

async fn fresh_target_did(label: &str) -> identity::Did {
    let label = label.to_owned();
    let key = tokio::task::spawn_blocking(move || gents_store::test_support::test_identity(&label))
        .await
        .unwrap();
    identity::Did::new(key.did()).unwrap()
}

#[tokio::test]
async fn detached_grant_fence_conformance_on_signed_home() {
    let home = SignedHome::start("detached-grant-fence-conformance", Extensions::none()).await;
    let binding = home.create_agent("Fenced Agent", &AdmitAllProfiles).await;
    let agent_doc = agent_doc_id(&home, &binding.agent_did).await;
    let foreign_user = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("detached-grant-fence-foreign")
    })
    .await
    .unwrap();
    gents_store::test_support::grant_operator_user(
        &home.store.node,
        home.store.node_identity.as_ref(),
        &foreign_user,
    )
    .await;
    let foreign_access = home.store.client_access(&foreign_user);

    for (index, case) in
        gents_lean_vocab::families::identity_detached_grant_fence_decide_grant::cases::<
            ModelDecision,
            ModelOrigin,
        >()
        .into_iter()
        .enumerate()
    {
        let origin = map_origin(case.input.origin);
        let creator_matches = case.input.creator_matches;
        let revoked_since_queue = case.input.revoked_since_queue;
        let target = fresh_target_did(&format!("detached-grant-case-{index}")).await;

        if revoked_since_queue {
            home.user_access
                .add_actor_relationship("Agent", &agent_doc, "session_writer", &target)
                .await
                .unwrap();
            home.user_access
                .delete_actor_relationship("Agent", &agent_doc, "session_writer", &target)
                .await
                .unwrap();
            assert!(
                !home
                    .user_access
                    .observe_actor_relationship("Agent", &agent_doc, "session_writer", &target)
                    .await
                    .unwrap(),
                "revocation must clear the relationship before replay"
            );
        }

        let caller = if creator_matches {
            &home.user_access
        } else {
            &foreign_access
        };

        let observed = match origin {
            GrantOrigin::Detached => {
                let grant = ConfigRelationshipGrant::Actor {
                    collection: "Agent".to_owned(),
                    doc_id: agent_doc.clone(),
                    relation: "session_writer".to_owned(),
                    target: target.to_string(),
                };
                match caller
                    .complete_relationship_grants(home.user.did(), &[grant])
                    .await
                {
                    Err(ConfigClientError::TransactionActorMismatch) => {
                        ModelDecision::RefuseActorMismatch
                    }
                    Ok(()) => {
                        let granted = home
                            .user_access
                            .observe_actor_relationship(
                                "Agent",
                                &agent_doc,
                                "session_writer",
                                &target,
                            )
                            .await
                            .unwrap();
                        if granted {
                            ModelDecision::Grant
                        } else {
                            ModelDecision::SkipRevoked
                        }
                    }
                    Err(other) => {
                        panic!(
                            "unexpected detached grant completion error for case {index}: {other}"
                        )
                    }
                }
            }
            GrantOrigin::Explicit => match caller
                .add_actor_relationship("Agent", &agent_doc, "session_writer", &target)
                .await
            {
                Err(error)
                    if error.is_permission_denied()
                        || matches!(
                            error,
                            ConfigClientError::DocumentRelationshipStatus { .. }
                        ) =>
                {
                    ModelDecision::RefuseActorMismatch
                }
                Ok(()) => {
                    assert!(
                        home.user_access
                            .observe_actor_relationship(
                                "Agent",
                                &agent_doc,
                                "session_writer",
                                &target,
                            )
                            .await
                            .unwrap(),
                        "explicit grant must establish the relationship"
                    );
                    ModelDecision::Grant
                }
                Err(other) => {
                    panic!("unexpected explicit grant error for case {index}: {other}")
                }
            },
        };

        let pure = map_decision(decide_grant(origin, creator_matches, revoked_since_queue));
        assert_eq!(
            observed, pure,
            "real SignedHome behavior must match the pure production owner decision"
        );
        case.check(observed);
    }

    home.store.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn detached_grant_completion_fenced_under_concurrent_revocation() {
    let home = SignedHome::start("detached-grant-fence-contention", Extensions::none()).await;
    let binding = home
        .create_agent("Contended Agent", &AdmitAllProfiles)
        .await;
    let agent_doc = agent_doc_id(&home, &binding.agent_did).await;
    let creator = home.user.did().to_owned();

    // Direct HTTP revocation racing detached completion.
    for round in 0..16 {
        let target = fresh_target_did(&format!("contention-http-{round}")).await;
        let grant = ConfigRelationshipGrant::Actor {
            collection: "Agent".to_owned(),
            doc_id: agent_doc.clone(),
            relation: "session_writer".to_owned(),
            target: target.to_string(),
        };
        let barrier = Arc::new(Barrier::new(2));
        let complete_access = home.user_access.clone();
        let revoke_access = home.user_access.clone();
        let complete_creator = creator.clone();
        let complete_grant = grant.clone();
        let complete_barrier = Arc::clone(&barrier);
        let revoke_doc = agent_doc.clone();
        let revoke_target = target.clone();
        let revoke_barrier = Arc::clone(&barrier);

        let complete_task = tokio::spawn(async move {
            complete_barrier.wait().await;
            complete_access
                .complete_relationship_grants(&complete_creator, &[complete_grant])
                .await
        });
        let revoke_task = tokio::spawn(async move {
            revoke_barrier.wait().await;
            revoke_access
                .delete_actor_relationship("Agent", &revoke_doc, "session_writer", &revoke_target)
                .await
        });

        let (complete_result, revoke_result) = tokio::join!(complete_task, revoke_task);
        complete_result.unwrap().unwrap();
        revoke_result.unwrap().unwrap();

        assert!(
            !home
                .user_access
                .observe_actor_relationship("Agent", &agent_doc, "session_writer", &target)
                .await
                .unwrap(),
            "concurrent revocation must not be resurrected in round {round}"
        );

        // A late retry of the same detached grant suffix must also stay fenced.
        home.user_access
            .complete_relationship_grants(&creator, &[grant])
            .await
            .unwrap();
        assert!(
            !home
                .user_access
                .observe_actor_relationship("Agent", &agent_doc, "session_writer", &target)
                .await
                .unwrap(),
            "late detached completion retry must not resurrect revoked grant in round {round}"
        );
    }

    // Transactional HTTP revocation racing detached completion.
    for round in 0..8 {
        let target = fresh_target_did(&format!("contention-http-txn-{round}")).await;
        let grant = ConfigRelationshipGrant::Actor {
            collection: "Agent".to_owned(),
            doc_id: agent_doc.clone(),
            relation: "session_writer".to_owned(),
            target: target.to_string(),
        };
        let barrier = Arc::new(Barrier::new(2));
        let complete_access = home.user_access.clone();
        let revoke_access = home.user_access.clone();
        let complete_creator = creator.clone();
        let complete_grant = grant.clone();
        let complete_barrier = Arc::clone(&barrier);
        let revoke_doc = agent_doc.clone();
        let revoke_target = target.clone();
        let revoke_barrier = Arc::clone(&barrier);

        let complete_task = tokio::spawn(async move {
            complete_barrier.wait().await;
            complete_access
                .complete_relationship_grants(&complete_creator, &[complete_grant])
                .await
        });
        let revoke_task = tokio::spawn(async move {
            revoke_barrier.wait().await;
            revoke_access
                .transact_write("test.detached_fence.revoke", |txn| {
                    let revoke_doc = revoke_doc.clone();
                    let revoke_target = revoke_target.clone();
                    Box::pin(async move {
                        txn.delete_actor_relationship(
                            "Agent",
                            &revoke_doc,
                            "session_writer",
                            &revoke_target,
                        )
                        .await
                    })
                })
                .await
        });

        let (complete_result, revoke_result) = tokio::join!(complete_task, revoke_task);
        complete_result.unwrap().unwrap();
        revoke_result.unwrap().unwrap();

        assert!(
            !home
                .user_access
                .observe_actor_relationship("Agent", &agent_doc, "session_writer", &target)
                .await
                .unwrap(),
            "transactional revocation must not be resurrected in round {round}"
        );
    }

    // Embedded local revocation racing detached Resource grant completion on the same node.
    let node_access = home.node_access();
    let nodes = home
        .user_access
        .execute(&format!(
            r#"{{ Node(filter: {{ node_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            gents_store::graphql::escape_graphql_string(home.node_did()),
        ))
        .await
        .unwrap();
    let node_doc = nodes["data"]["Node"][0]["_docID"]
        .as_str()
        .expect("node document id")
        .to_owned();
    let local_creator = home.node_did().to_owned();
    for round in 0..8 {
        let local_node_did = format!("{}-local-{round}", home.node_did());
        let created = node_access
            .write(
                "test.detached_fence.local_doc",
                &format!(
                    r#"mutation {{ create_NodeRuntime(input: {{ node_did: "{}", reconcile_phase: "ready", updated_at: "2026-01-01T00:00:00Z" }}) {{ _docID }} }}"#,
                    gents_store::graphql::escape_graphql_string(&local_node_did),
                ),
            )
            .await
            .unwrap();
        let local_doc = gents_store::graphql::created_doc_id(&created, "NodeRuntime").unwrap();
        let grant = ConfigRelationshipGrant::Resource {
            collection: "NodeRuntime".to_owned(),
            doc_id: local_doc.clone(),
            relation: "parent".to_owned(),
            target_resource: "nodes".to_owned(),
            target_doc_id: node_doc.clone(),
        };
        let barrier = Arc::new(Barrier::new(2));
        let complete_access = ConfigAccess::local_as(
            home.store.node.clone(),
            identity::Did::new(home.node_did()).unwrap(),
        );
        let revoke_access = ConfigAccess::local_as(
            home.store.node.clone(),
            identity::Did::new(home.node_did()).unwrap(),
        );
        let complete_creator = local_creator.clone();
        let complete_grant = grant.clone();
        let complete_barrier = Arc::clone(&barrier);
        let revoke_doc = local_doc.clone();
        let revoke_node_doc = node_doc.clone();
        let revoke_barrier = Arc::clone(&barrier);

        let complete_task = tokio::spawn(async move {
            complete_barrier.wait().await;
            complete_access
                .complete_relationship_grants(&complete_creator, &[complete_grant])
                .await
        });
        let revoke_task = tokio::spawn(async move {
            revoke_barrier.wait().await;
            revoke_access
                .delete_resource_relationship(
                    "NodeRuntime",
                    &revoke_doc,
                    "parent",
                    "nodes",
                    &revoke_node_doc,
                )
                .await
        });

        let (complete_result, revoke_result) = tokio::join!(complete_task, revoke_task);
        complete_result.unwrap().unwrap();
        revoke_result.unwrap().unwrap();

        let relations = node_access
            .list_document_relationships("NodeRuntime", &local_doc)
            .await
            .unwrap();
        assert!(
            !relations.iter().any(|entry| entry.relation == "parent"),
            "local revocation must not be resurrected in round {round}: {relations:?}"
        );
    }

    home.store.node.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_revocation_does_not_fence_and_fresh_transaction_proceeds() {
    let home = SignedHome::start("detached-grant-fence-lifecycle", Extensions::none()).await;
    let binding = home
        .create_agent("Lifecycle Agent", &AdmitAllProfiles)
        .await;
    let agent_doc = agent_doc_id(&home, &binding.agent_did).await;
    let foreign_user = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("detached-grant-fence-lifecycle-foreign")
    })
    .await
    .unwrap();
    gents_store::test_support::grant_operator_user(
        &home.store.node,
        home.store.node_identity.as_ref(),
        &foreign_user,
    )
    .await;
    let foreign_access = home.store.client_access(&foreign_user);

    // A denied revocation attempt must not fence a legitimate detached grant.
    let target_failed = fresh_target_did("detached-grant-failed-revoke").await;
    let denied = foreign_access
        .delete_actor_relationship("Agent", &agent_doc, "session_writer", &target_failed)
        .await;
    assert!(
        denied.is_err(),
        "non-owner revocation must be denied by DefraDB DAC"
    );
    let grant = ConfigRelationshipGrant::Actor {
        collection: "Agent".to_owned(),
        doc_id: agent_doc.clone(),
        relation: "session_writer".to_owned(),
        target: target_failed.to_string(),
    };
    home.user_access
        .complete_relationship_grants(home.user.did(), &[grant])
        .await
        .unwrap();
    assert!(
        home.user_access
            .observe_actor_relationship("Agent", &agent_doc, "session_writer", &target_failed)
            .await
            .unwrap(),
        "denied revocation must not block a subsequent detached completion"
    );

    // A new transaction started after an earlier revocation captures the newer
    // fence epoch and completes its newly queued detached grant.
    let target_fresh = fresh_target_did("detached-grant-fresh-txn").await;
    home.user_access
        .add_actor_relationship("Agent", &agent_doc, "session_writer", &target_fresh)
        .await
        .unwrap();
    home.user_access
        .delete_actor_relationship("Agent", &agent_doc, "session_writer", &target_fresh)
        .await
        .unwrap();
    assert!(!home
        .user_access
        .observe_actor_relationship("Agent", &agent_doc, "session_writer", &target_fresh)
        .await
        .unwrap());

    let fresh_grant = ConfigRelationshipGrant::Actor {
        collection: "Agent".to_owned(),
        doc_id: agent_doc.clone(),
        relation: "session_writer".to_owned(),
        target: target_fresh.to_string(),
    };
    home.user_access
        .transact_write("test.detached_fence.fresh_after_revoke", |txn| {
            let fresh_grant = fresh_grant.clone();
            Box::pin(async move { txn.queue_relationship_grant(fresh_grant) })
        })
        .await
        .unwrap();
    assert!(
        home.user_access
            .observe_actor_relationship("Agent", &agent_doc, "session_writer", &target_fresh)
            .await
            .unwrap(),
        "a fresh transaction started after an earlier revocation must complete its new grant"
    );

    home.store.node.shutdown().await.unwrap();
}
