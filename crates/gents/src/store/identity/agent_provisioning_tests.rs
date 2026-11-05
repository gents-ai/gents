use super::*;
use crate::store::config_client::{ConfigAccess, NodePermission};

#[tokio::test]
async fn native_provisioning_follows_modeled_publication_and_revocation() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().identity_node_provisioning_cases;
    let modeled = cases
        .iter()
        .find(|case| case["name"] == "revoked-after-publication")
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    let runtime =
        super::super::KeyIdentity::load_or_create(home.path().join("runtime.key"), None).unwrap();
    let owner =
        super::super::KeyIdentity::load_or_create(home.path().join("owner.key"), None).unwrap();
    let outsider =
        super::super::KeyIdentity::load_or_create(home.path().join("outsider.key"), None).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_node_acp_enabled()
            .build()
            .await
            .unwrap(),
    );
    crate::store::schema::ensure_runtime_schemas(&node)
        .await
        .unwrap();
    let admin = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did()).unwrap());
    for subject in [owner.did(), outsider.did()] {
        let subject = identity::Did::new(subject).unwrap();
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
        ] {
            admin
                .grant_node_permission(&subject, permission)
                .await
                .unwrap();
        }
    }
    let owner_access =
        ConfigAccess::local_as(node.clone(), identity::Did::new(owner.did()).unwrap());
    let response = owner_access
        .write(
            "test.node_provisioning",
            &format!(
                r#"mutation {{ create_Node(input: {{ node_did: "{}" }}) {{ _docID }} }}"#,
                runtime.did()
            ),
        )
        .await
        .unwrap();
    let doc = crate::store::graphql::created_doc_id(&response, "Node").unwrap();
    owner_access
        .add_actor_relationship(
            "Node",
            &doc,
            "runtime_reader",
            &identity::Did::new(runtime.did()).unwrap(),
        )
        .await
        .unwrap();
    assert!(provision_agent_identity_authorized(
        &node,
        &runtime,
        &identity::Did::new(outsider.did()).unwrap()
    )
    .await
    .is_err());
    let binding = provision_agent_identity_authorized(
        &node,
        &runtime,
        &identity::Did::new(owner.did()).unwrap(),
    )
    .await
    .unwrap();
    let agent = ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(&binding.agent_did).unwrap(),
    );
    let readable = agent.execute("{ Node { _docID } }").await.unwrap();
    assert_eq!(
        readable["data"]["Node"],
        serde_json::json!([]),
        "node permission must not grant document access"
    );
    let published = modeled["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["action"]["action"] == "finish")
        .unwrap();
    assert_eq!(published["after"]["revealed"], true);
    for permission in ["read-document", "update-document", "add-dac-relation"] {
        assert!(published["after"]["granted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == permission));
    }
    let own = agent
        .write(
            "test.agent_node",
            &format!(
                r#"mutation {{ create_Node(input: {{ node_did: "{}" }}) {{ _docID }} }}"#,
                binding.agent_did
            ),
        )
        .await
        .unwrap();
    let own_doc = crate::store::graphql::created_doc_id(&own, "Node").unwrap();
    agent.write("test.agent_node_update", &format!(r#"mutation {{ update_Node(docID: "{own_doc}", input: {{ display_name: "Agent" }}) {{ _docID }} }}"#)).await.unwrap();
    agent
        .add_actor_relationship(
            "Node",
            &own_doc,
            "runtime_reader",
            &identity::Did::new(outsider.did()).unwrap(),
        )
        .await
        .unwrap();
    let context =
        defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned()));
    node.delete_nac_actor_relationship_as(&context, "read-document", &binding.agent_did)
        .await
        .unwrap();
    resolve_agent_identity(&runtime, &binding).unwrap();
    let response = agent.execute("{ Node { _docID } }").await.unwrap();
    let last = modeled["steps"].as_array().unwrap().last().unwrap();
    assert!(!last["after"]["granted"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "read-document"));
    assert!(
        response["data"].get("Node").is_none(),
        "resolving an agent must not restore revoked node permission"
    );
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn modeled_agent_creator_and_node_owner_control_native_agent() -> anyhow::Result<()> {
    use acp::policy_yaml::{build_policy, parse_policy_yaml};
    use acp::{
        DocumentACP, DocumentPermission, Identity, MemoryZanzibarStore, ZanzibarDocumentACP,
        ZanzibarStore,
    };
    let cases = tokio::task::spawn_blocking(|| {
        crate::lean_vocab_test::lean_contract_snapshot()
            .identity_created_agent_access_cases
            .clone()
    })
    .await?;
    let dir = tempfile::tempdir()?;
    let user = KeyIdentity::load_or_create(dir.path().join("user"), None)?;
    let creator = KeyIdentity::load_or_create(dir.path().join("creator"), None)?;
    let sibling = KeyIdentity::load_or_create(dir.path().join("sibling"), None)?;
    let policy = build_policy(
        &parse_policy_yaml(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY)
            .map_err(anyhow::Error::msg)?,
        0,
    )?;
    let store = Arc::new(MemoryZanzibarStore::new());
    store.store_policy(&policy).await?;
    let acp = ZanzibarDocumentACP::new(store);
    let user_did = identity::Did::new(user.did())?;
    let creator_did = identity::Did::new(creator.did())?;
    let sibling_did = identity::Did::new(sibling.did())?;
    acp.register_doc_object(&user_did, &policy.id, "nodes", "node")
        .await?;
    acp.register_doc_object(&creator_did, &policy.id, "agents", "child")
        .await?;
    assert!(
        acp.add_relationship(
            &creator_did,
            acp::Subject::entity_set("nodes", "node", ""),
            &policy.id,
            "agents",
            "child",
            "parent",
            &[]
        )
        .await?
    );
    for case in &cases {
        let actor = match case["actor"].as_str().unwrap() {
            "user" => &user_did,
            "creator" => &creator_did,
            "sibling" => &sibling_did,
            actor => panic!("unknown modeled actor {actor}"),
        };
        let permission = match case["permission"].as_str().unwrap() {
            "read" => DocumentPermission::Read,
            "update" => DocumentPermission::Update,
            permission => panic!("unknown modeled permission {permission}"),
        };
        let actual = acp
            .check_doc_access(
                &Identity::Authenticated(actor.clone()),
                permission,
                &policy.id,
                "agents",
                "child",
            )
            .await?;
        assert_eq!(actual, case["allowed"].as_bool().unwrap(), "{case}");
    }
    Ok(())
}
