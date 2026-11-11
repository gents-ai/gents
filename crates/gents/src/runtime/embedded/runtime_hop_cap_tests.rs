use anyhow::Context as _;

use gents_documents::apply_desired_state_plan;
use gents_documents::{
    read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::identity::NodeIdentity;

async fn set_node_hop_cap(owner: &ConfigAccess, node_did: &str, cap: u32) -> anyhow::Result<()> {
    owner
        .transact_write("test.runtime_hop_cap.bound", |txn| {
            Box::pin(async move {
                let (_, mut node) = read_desired_state_record_in_txn(
                    txn,
                    gents_store::collection::Collection::Node,
                    node_did,
                    node_did,
                )
                .await?
                .context("owned Node is unavailable")?;
                node["max_request_hop"] = serde_json::json!(cap);
                let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
                    collection: gents_store::collection::Collection::Node,
                    add: node.clone(),
                    update: node,
                }])?;
                apply_desired_state_plan(
                    txn,
                    &plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn agent_transaction_reads_hidden_node_cap_as_runtime_and_fails_closed() -> anyhow::Result<()>
{
    let (db, user, user_access, binding) =
        crate::test_support::root::signed_test_node("runtime-hop-cap").await;
    let runtime = db.node_identity.as_ref();
    let signer = gents_store::identity::resolve_agent_identity(runtime, &binding)?;
    let agent = ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(&binding.agent_did)?,
        signer.as_ref(),
    )?;
    set_node_hop_cap(&user_access, runtime.did(), 1).await?;
    let observe = |node_did: String| {
        agent.transact_readonly("test.runtime_hop_cap.observe", move |txn| {
            let node_did = node_did.clone();
            Box::pin(async move {
                Ok::<_, anyhow::Error>(txn.runtime_max_request_hop(&node_did).await?)
            })
        })
    };
    let hidden = agent.execute("{ Node { _docID } }").await?;
    assert!(hidden["data"]["Node"].as_array().is_some_and(Vec::is_empty));
    assert_eq!(observe(runtime.did().to_owned()).await?, Some(Some(1)));
    assert!(observe("did:key:foreign".to_owned()).await.is_err());

    let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did())?);
    admin
        .grant_node_permission(
            &identity::Did::new(user.did())?,
            NodePermission::DacRelationDelete,
        )
        .await?;
    let node = user_access.execute("{ Node { _docID } }").await?;
    let node_doc = node["data"]["Node"][0]["_docID"].as_str().unwrap();
    user_access
        .delete_actor_relationship(
            "Node",
            node_doc,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    assert_eq!(observe(runtime.did().to_owned()).await?, None);
    Ok(())
}
