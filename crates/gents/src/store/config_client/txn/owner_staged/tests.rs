use std::sync::Arc;

use super::OwnerStagedAuthority;
use crate::store::config_client::{
    ConfigAccess, ConfigApplyTxn, ConfigClientError, GraphqlEndpoint,
};

const POLICY: &str = "name: OwnerStaged\nresources:\n  - name: rows\n    relations:\n      - name: writer\n      - name: editor\n    permissions:\n      - name: read\n        expr: writer + editor\n      - name: update\n        expr: editor\n      - name: delete\n";

fn rows(response: &serde_json::Value) -> usize {
    response["data"]["OwnerStagedRow"]
        .as_array()
        .map_or(0, Vec::len)
}

/// Lean `Identity.OwnerStaged.casesJson`: a requester's own change to a
/// relation that only the document's runtime owner manages is refused on the
/// public route, including for the node's NAC owner; the runtime stages the
/// same change as the owner for a requester that the modeled authority admits
/// and stages nothing for one it refuses.
#[tokio::test]
async fn owner_staged_relationships_follow_the_document_owner() -> anyhow::Result<()> {
    use crate::store::identity::NodeIdentity;
    let directory = tempfile::tempdir()?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime"), None)?;
    // An embedded configuration transaction acts as the runtime or as one of
    // its managed Agents, so every modeled requester is a managed Agent.
    let managed = || -> anyhow::Result<Arc<dyn NodeIdentity>> {
        let binding = crate::store::identity::provision_agent_identity(&runtime)?;
        Ok(crate::store::identity::resolve_agent_identity(
            &runtime, &binding,
        )?)
    };
    let (user, admin, outsider, target) = (managed()?, managed()?, managed()?, managed()?);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr()?);
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let owner_context =
        defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    for actor in [admin.did(), outsider.did(), runtime.did(), target.did()] {
        for permission in [
            acp::nac::NodePermission::CollectionGet,
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::DacRelationAdd,
            acp::nac::NodePermission::DacRelationDelete,
        ] {
            node.add_nac_actor_relationship_as(&owner_context, permission.as_str(), actor)
                .await?;
        }
    }
    let household = crate::lean_vocab_test::lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .expect("configuration policy model");
    node.add_dac_policy_with_counter_as(&owner_context, &household.policy_yaml, 0)
        .await?;
    let policy = node.add_dac_policy_as(&owner_context, POLICY).await?;
    node.add_collection_as(&owner_context, gents_protocol::schemas::NODE)
        .await?;
    node.add_collection_as(
        &owner_context,
        &format!(
            "type OwnerStagedRow @policy(id: \"{policy}\", resource: \"rows\") {{ label: String }}"
        ),
    )
    .await?;
    let client = |key: &Arc<dyn NodeIdentity>| -> anyhow::Result<ConfigAccess> {
        Ok(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(&endpoint, key.did())
                .with_client_signing_capability(key.client_signing_capability()?)?,
        ))
    };
    let runtime_did = identity::Did::new(runtime.did())?;
    let user_did = identity::Did::new(user.did())?;
    let target_did = identity::Did::new(target.did())?;
    let user_access = client(&user)?;
    let parent = user_access
        .write(
            "test.owner_staged.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did: "{}"}}) {{_docID}} }}"#,
                crate::store::graphql::escape_graphql_string(runtime.did())
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &parent,
            "administrator",
            &identity::Did::new(admin.did())?,
        )
        .await?;
    let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
    let created = runtime_access
        .write(
            "test.owner_staged.row",
            "mutation { add_OwnerStagedRow(input: {label: \"row\"}) { _docID } }",
        )
        .await?;
    let doc = created["data"]["add_OwnerStagedRow"][0]["_docID"]
        .as_str()
        .expect("row ID")
        .to_owned();
    runtime_access
        .transact_write("test.owner_staged.editor", |txn| {
            let (doc, user_did) = (&doc, &user_did);
            Box::pin(async move {
                txn.add_actor_relationship("OwnerStagedRow", doc, "editor", user_did)
                    .await
            })
        })
        .await?;
    let target_access = ConfigAccess::local_as(node.clone(), target_did.clone());
    let visible = || async {
        anyhow::Ok(rows(
            &target_access
                .execute("{ OwnerStagedRow { _docID } }")
                .await?,
        ))
    };

    let staging = |key: &Arc<dyn NodeIdentity>, did: identity::Did| {
        let signer = key.managed_agent_signer().expect("managed Agent signer");
        ConfigApplyTxn::begin_local_with_signer(&node, did, signer)
    };

    let cases = &crate::lean_vocab_test::lean_contract_snapshot().owner_staged_relation_cases;
    assert!(!cases.is_empty());
    for case in cases {
        let (key, did) = match case["requester"].as_str().unwrap() {
            "user" => (&user, user_did.clone()),
            "admin" => (&admin, identity::Did::new(admin.did())?),
            "outsider" => (&outsider, identity::Did::new(outsider.did())?),
            other => panic!("unknown modeled requester {other}"),
        };
        let authority = match case["path"].as_str().unwrap() {
            "event_cursor_rebind" | "declared_placement_readers" | "workspace_selection" => {
                OwnerStagedAuthority::DocumentUpdate
            }
            "pairing_reader" => OwnerStagedAuthority::NodeUpdate,
            other => panic!("unknown modeled path {other}"),
        };
        assert_eq!(visible().await?, 0, "{case}");
        let direct = client(key)?
            .transact_write("test.owner_staged.direct", |txn| {
                let (doc, target_did) = (&doc, &target_did);
                Box::pin(async move {
                    txn.add_actor_relationship("OwnerStagedRow", doc, "writer", target_did)
                        .await
                })
            })
            .await;
        assert_eq!(
            direct.is_ok(),
            case["direct"].as_bool().unwrap(),
            "{case}: {direct:?}"
        );
        assert_eq!(
            visible().await? == 1,
            case["direct"].as_bool().unwrap(),
            "{case}"
        );
        let txn = staging(key, did).await?;
        let staged = txn
            .change_owner_staged_actor_relationship(
                "OwnerStagedRow",
                &doc,
                "writer",
                &target_did,
                false,
                authority,
            )
            .await;
        let expected = case["staged"].as_bool().unwrap();
        assert_eq!(staged.is_ok(), expected, "{case}: {staged:?}");
        if expected {
            txn.commit().await?;
        } else {
            assert!(
                matches!(
                    staged,
                    Err(ConfigClientError::OwnerStagedRelationshipDenied { .. })
                ),
                "{case}: {staged:?}"
            );
            txn.discard().await?;
        }
        assert_eq!(visible().await? == 1, expected, "{case}");
        if expected {
            let txn = staging(&user, user_did.clone()).await?;
            txn.change_owner_staged_actor_relationship(
                "OwnerStagedRow",
                &doc,
                "writer",
                &target_did,
                true,
                OwnerStagedAuthority::DocumentUpdate,
            )
            .await?;
            txn.commit().await?;
        }
    }
    node.shutdown().await?;
    Ok(())
}
