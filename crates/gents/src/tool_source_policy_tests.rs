use acp::NodePermission;
use gents_store::identity::NodeIdentity;

async fn write_fixture_tools(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    agent_did: &str,
    tools: serde_json::Value,
) -> anyhow::Result<()> {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let mut value =
        serde_json::json!({"node_did": node_did, "tools_id": format!("{agent_did}:tools")});
    for (key, field) in tools.as_object().expect("Tools fields") {
        value[key] = field.clone();
    }
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Tools,
        add: value.clone(),
        update: value,
    }])?;
    access
        .transact_write("test.projection.fixture_tools", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn enrolled_user_authors_held_tools_and_only_the_household_owner_adds_schema(
) -> anyhow::Result<()> {
    use gents_store::identity::GrantSet;
    let cases = tokio::task::spawn_blocking(|| {
        gents_lean_vocab::families::identity_delegation_tools_authoring::cases()
    })
    .await?;
    let (db, _user, user_access, binding) =
        crate::test_support::root::signed_test_node("enrolled-tools-authoring").await;
    let node_did = db.node_identity.did().to_owned();
    let agent = identity::Did::new(&binding.agent_did)?;
    let self_config =
        serde_json::to_value(gents_tools::builtin::self_config::engineer_self_config())?;
    let held = write_fixture_tools(
        &user_access,
        &node_did,
        &binding.agent_did,
        serde_json::json!({"self_config": self_config}),
    )
    .await;
    assert!(
        db.node
            .nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?
    );
    let schema = serde_json::json!({
        "self_config": self_config,
        "built_ins": {"enable_schema_tool": true},
    });
    let refused =
        write_fixture_tools(&user_access, &node_did, &binding.agent_did, schema.clone()).await;
    assert!(
        !db.node
            .nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );

    let householder = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("enrolled-tools-householder")
    })
    .await?;
    let householder_did = identity::Did::new(householder.did())?;
    let runtime = gents_store::config_client::ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(&node_did)?,
    );
    GrantSet::HouseholdOwner
        .grant(&runtime, &householder_did)
        .await?;
    let node_doc = user_access.execute("{ Node { _docID } }").await?["data"]["Node"][0]["_docID"]
        .as_str()
        .expect("fixture Node")
        .to_owned();
    user_access
        .add_actor_relationship("Node", &node_doc, "administrator", &householder_did)
        .await?;
    let owner_write = write_fixture_tools(
        &db.client_access(&householder),
        &node_did,
        &binding.agent_did,
        schema,
    )
    .await;
    assert!(
        db.node
            .nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    for case in cases {
        case.check(match case.input.name.as_str() {
            "enrolled-grants-held" => held.is_ok(),
            "enrolled-refused-schema" => refused.is_ok(),
            "owner-grants-schema" => owner_write.is_ok(),
            other => panic!("the tools-authoring scenario has no step for {other}"),
        });
    }
    db.node.shutdown().await?;
    Ok(())
}
