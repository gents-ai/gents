use super::*;
use crate::config_client::ConfigAccess;

#[tokio::test]
async fn modeled_agent_creator_and_node_owner_control_native_agent() -> anyhow::Result<()> {
    use acp::policy_yaml::{build_policy, parse_policy_yaml};
    use acp::{
        DocumentACP, DocumentPermission, Identity, MemoryZanzibarStore, ZanzibarDocumentACP,
        ZanzibarStore,
    };
    let cases = gents_lean_vocab::families::created_agent_access_cases::cases();
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
    for case in cases {
        let actor = match case.input.actor.as_str() {
            "user" => &user_did,
            "creator" => &creator_did,
            "sibling" => &sibling_did,
            actor => panic!("unknown modeled actor {actor}"),
        };
        let permission = match case.input.permission.as_str() {
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
        case.check(actual);
    }
    Ok(())
}

#[tokio::test]
async fn modeled_application_pairing_access_controls_native_overlay() -> anyhow::Result<()> {
    use acp::policy_yaml::{build_policy, parse_policy_yaml};
    use acp::{
        DocumentACP, DocumentPermission, Identity, MemoryZanzibarStore, ZanzibarDocumentACP,
        ZanzibarStore,
    };
    let cases = gents_lean_vocab::families::application_pairing_access_cases::cases();
    let dir = tempfile::tempdir()?;
    let mut actors = Vec::new();
    for name in [
        "user", "author", "engineer", "runtime", "manager", "sibling",
    ] {
        let key = KeyIdentity::load_or_create(dir.path().join(name), None)?;
        actors.push((name, identity::Did::new(key.did())?));
    }
    let did = |name: &str| -> identity::Did {
        actors
            .iter()
            .find(|(actor, _)| *actor == name)
            .map(|(_, did)| did.clone())
            .unwrap_or_else(|| panic!("unknown modeled actor {name}"))
    };
    let policy = build_policy(
        &parse_policy_yaml(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY)
            .map_err(anyhow::Error::msg)?,
        0,
    )?;
    let store = Arc::new(MemoryZanzibarStore::new());
    store.store_policy(&policy).await?;
    let acp = ZanzibarDocumentACP::new(store);
    acp.register_doc_object(&did("user"), &policy.id, "nodes", "node")
        .await?;
    acp.register_doc_object(
        &did("author"),
        &policy.id,
        "application_pairings",
        "overlay",
    )
    .await?;
    for (relation, actor) in [
        ("administrator", "engineer"),
        ("runtime_reader", "runtime"),
        ("agent_manager", "manager"),
    ] {
        assert!(
            acp.add_relationship(
                &did("user"),
                acp::Subject::Entity(did(actor)),
                &policy.id,
                "nodes",
                "node",
                relation,
                &[]
            )
            .await?
        );
    }
    assert!(
        acp.add_relationship(
            &did("author"),
            acp::Subject::entity_set("nodes", "node", ""),
            &policy.id,
            "application_pairings",
            "overlay",
            "parent",
            &[]
        )
        .await?
    );
    for case in cases {
        let permission = match case.input.permission.as_str() {
            "read" => DocumentPermission::Read,
            "update" => DocumentPermission::Update,
            "delete" => DocumentPermission::Delete,
            permission => panic!("unknown modeled permission {permission}"),
        };
        let actual = acp
            .check_doc_access(
                &Identity::Authenticated(did(&case.input.actor)),
                permission,
                &policy.id,
                "application_pairings",
                "overlay",
            )
            .await?;
        case.check(actual);
    }
    Ok(())
}
