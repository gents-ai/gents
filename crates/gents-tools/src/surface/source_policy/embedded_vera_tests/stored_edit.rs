use gents_store::node_scope::ContextCalls as _;
use std::sync::Arc;

use gents_store::config_client::{
    ClientSigningCapability, ConfigAccess, IdempotentTransactionRetry, NodePermission,
    PackEvidenceError, PackMutation,
};
use gents_store::identity::NodeIdentity;

use super::stored_create::{fixture, Fixture};
use super::{create_task_in_process, presented, tasks};

/// A stored update or delete of `signer`'s, made by the working Agent's
/// transaction with nothing presented for the User.
async fn stored_edit(
    node: &Arc<defra_node::Node>,
    agent: &Arc<dyn NodeIdentity>,
    signer: &ClientSigningCapability,
    mutation: &PackMutation,
) -> anyhow::Result<Result<(), PackEvidenceError>> {
    let access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    access
        .transact_idempotent(
            IdempotentTransactionRetry::Standard,
            "test.embedded_vera.stored_edit",
            |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        txn.apply_stored_user_mutation_for_native_fixture(signer, mutation)
                            .await
                            .map(|_| ()),
                    )
                })
            },
        )
        .await
}

fn renamed(doc_id: &str, name: &str) -> PackMutation {
    PackMutation::Update {
        collection: "Task".into(),
        doc_id: doc_id.into(),
        input: serde_json::json!({ "display_name": name }),
    }
}

fn deleted(doc_id: &str) -> PackMutation {
    PackMutation::Delete {
        collection: "Task".into(),
        doc_id: doc_id.into(),
    }
}

async fn display_names(
    node: &defra_node::Node,
    owner: &identity::Did,
) -> anyhow::Result<Vec<String>> {
    super::stored(node, owner, "Task", "display_name").await
}

#[tokio::test]
async fn a_stored_update_or_delete_runs_on_its_users_verified_permission_alone(
) -> anyhow::Result<()> {
    let Fixture {
        _directory,
        home,
        node,
        user,
        stranger,
        agent,
    } = fixture("stored-edit").await?;
    let as_node = ConfigAccess::local_as(node.clone(), identity::Did::new(home.node.did())?);
    as_node
        .grant_node_permission(
            &identity::Did::new(agent.did())?,
            NodePermission::DocumentDelete,
        )
        .await?;
    let audience = gents_store::local::document_acp::node_delegation_audience(&node)
        .expect("a Vera node names its delegation audience");
    let token = gents_store::config_client::mint_with_lifetime_for_test(
        &user.0,
        &audience,
        gents_store::config_client::VERA_DELEGATION_LIFETIME,
    )?;
    let created = create_task_in_process(
        &node,
        &home,
        &user.0,
        presented(&node, &user.0, token),
        "edited",
    )
    .await?;
    assert_eq!(tasks(&node, &home.owner).await?, ["edited"]);
    let agent_did = identity::Did::new(agent.did())?;
    let token = gents_store::config_client::mint_with_lifetime_for_test(
        &user.0,
        &audience,
        gents_store::config_client::VERA_DELEGATION_LIFETIME,
    )?;
    let context = defra_core::RequestContext::default()
        .with_identity(Some(user.did().to_owned()))
        .with_bearer(Some(user.did().to_owned()), Some(token));
    node.add_dac_actor_relationship_as(&context, "Task", &created, "reader", agent_did.as_str())
        .await?;

    assert!(
        stored_edit(
            &node,
            &agent,
            &stranger.0,
            &renamed(&created, "by-stranger")
        )
        .await
        .map_or(true, |applied| applied.is_err()),
        "a User without update on the document changes nothing"
    );
    assert_eq!(
        display_names(&node, &home.owner).await?,
        Vec::<String>::new()
    );

    stored_edit(&node, &agent, &user.0, &renamed(&created, "by-owner"))
        .await?
        .expect("the owner's stored update runs on its verified permission");
    assert_eq!(display_names(&node, &home.owner).await?, ["by-owner"]);

    assert!(
        stored_edit(&node, &agent, &stranger.0, &deleted(&created))
            .await
            .map_or(true, |applied| applied.is_err()),
        "a User without delete on the document removes nothing"
    );
    assert_eq!(tasks(&node, &home.owner).await?, ["edited"]);

    stored_edit(&node, &agent, &user.0, &deleted(&created))
        .await?
        .expect("the owner's stored delete runs on its verified permission");
    assert_eq!(tasks(&node, &home.owner).await?, Vec::<String>::new());
    assert!(defra_core::signing::get_identity(user.did()).is_none());

    node.shutdown().await?;
    Ok(())
}
