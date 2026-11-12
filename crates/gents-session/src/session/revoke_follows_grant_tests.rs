use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};

/// Replays Lean `Conformance.RevokeFollowsGrant.steps` against a served node:
/// a live Agent shares its own session with an enrolled User and withdraws the
/// share as itself, while another baseline holder can do neither.
#[tokio::test]
async fn an_agent_revokes_the_share_it_granted() {
    let (db, _user, _user_access, binding) = gents_inference::test_support::signed_test_node(
        "revoke-follows-grant",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let node = db.node.clone();
    let root = gents_store::test_support::held_runtime_identity(db.node_identity.did());
    let node_did = root.did().to_owned();
    let agent = gents_store::identity::resolve_agent_identity(&root, &binding).unwrap();
    let agent_did = identity::Did::new(agent.did()).unwrap();
    for permission in GrantSet::AgentBaseline.permissions() {
        assert!(
            node.nac_manager()
                .check_permission(&agent_did, *permission)
                .await
                .unwrap(),
            "the published Agent's projected baseline holds {}",
            permission.as_str()
        );
    }
    let agent_access = ConfigAccess::local_with_agent_signer(
        node.clone(),
        agent_did.clone(),
        agent.managed_agent_signer().unwrap(),
    )
    .unwrap();
    agent_access
        .transact_write("test.revoke_follows_grant.session", |txn| {
            let node_did = &node_did;
            let agent = &agent;
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    "shared-session",
                    node_did,
                    agent.did(),
                    None,
                    None,
                    None,
                    "2026-01-01T00:00:00Z",
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
    let session = agent_access
        .execute("{ AgentSession { _docID } }")
        .await
        .unwrap()["data"]["AgentSession"][0]["_docID"]
        .as_str()
        .expect("the Agent reads its own session")
        .to_owned();

    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(&node_did).unwrap());
    let key = |name: &str| {
        gents_store::identity::KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap()
    };
    let sibling = key("revoke-follows-grant-sibling");
    GrantSet::AgentBaseline
        .grant(&admin, &identity::Did::new(sibling.did()).unwrap())
        .await
        .unwrap();
    let sibling_access = db.client_access(&sibling);
    let grantee = key("revoke-follows-grant-grantee");
    crate::test_support::grant_operator_user(&node, &root, &grantee).await;
    let grantee_did = identity::Did::new(grantee.did()).unwrap();
    let grantee_access = ConfigAccess::local_as(node.clone(), grantee_did.clone());

    let cases = gents_lean_vocab::families::revoke_follows_grant_steps::cases();
    for (index, case) in cases.iter().enumerate() {
        assert_eq!(case.input.index, index as u64, "steps replay in order");
        let actual = match (case.input.actor.as_str(), case.input.action.as_str()) {
            ("grantee", "read") => {
                let rows = grantee_access
                    .execute("{ AgentSession { _docID } }")
                    .await
                    .unwrap();
                rows["data"]["AgentSession"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["_docID"] == session.as_str())
            }
            (actor, action @ ("grant" | "revoke")) => {
                let access = match actor {
                    "agent" => &agent_access,
                    "sibling" => &sibling_access,
                    other => panic!("unmodeled actor {other}"),
                };
                let result = if action == "grant" {
                    access
                        .add_actor_relationship("AgentSession", &session, "reader", &grantee_did)
                        .await
                } else {
                    access
                        .delete_actor_relationship("AgentSession", &session, "reader", &grantee_did)
                        .await
                };
                if let Err(error) = &result {
                    // A client-signed relationship change reports the
                    // native refusal as the endpoint's client-error status.
                    let refused = error.is_permission_denied()
                        || matches!(
                            error,
                            gents_store::config_client::ConfigClientError::DocumentRelationshipStatus {
                                status,
                                ..
                            } if status.is_client_error()
                        );
                    assert!(refused, "step {index} {actor} {action}: {error:#}");
                }
                result.is_ok()
            }
            (actor, action) => panic!("unmodeled step {actor} {action}"),
        };
        case.check(actual);
    }
    node.shutdown().await.unwrap();
}
