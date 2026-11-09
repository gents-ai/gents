/// The data-plane pairing overlay is an Agent-owned document parented by the
/// runtime Node: its native ACP follows the Lean `pairing_overlay_cases` for
/// every actor, before and after the parent relationship is revoked, and the
/// revocation survives a restart.
#[tokio::test]
async fn data_plane_overlay_acp_follows_the_configuration_model() -> anyhow::Result<()> {
    use crate::lean_vocab_test::lean_identity_household_cases;
    use crate::store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
    use crate::NodeIdentity as _;
    use crate::store::graphql::escape_graphql_string;
    use anyhow::Context as _;

    let model = lean_identity_household_cases()
        .iter()
        .find(|case| case.name == "configuration-explicit-grants")
        .context("missing configuration model")?;
    let directory = tempfile::tempdir()?;
    let runtime_path = directory.path().join("runtime");
    let runtime = Arc::new(crate::KeyIdentity::load_or_create(&runtime_path, None)?);
    let runtime_key =
        crate::store::identity::load_file_identity(&runtime_path)?.private_key_bytes();
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let outsider = crate::KeyIdentity::load_or_create(directory.path().join("outsider"), None)?;
    let binding = crate::store::identity::provision_agent_identity(runtime.as_ref())?;
    let agent = crate::store::identity::resolve_agent_identity(runtime.as_ref(), &binding)?;
    let sibling_binding = crate::store::identity::provision_agent_identity(runtime.as_ref())?;
    let sibling =
        crate::store::identity::resolve_agent_identity(runtime.as_ref(), &sibling_binding)?;
    let open = |key: Vec<u8>| {
        let runtime = runtime.clone();
        let data_path = directory.path().join("database");
        async move {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let address = listener.local_addr()?;
            let node = Arc::new(
                EmbeddedNode::builder()
                    .with_node_identity_did(runtime.did())
                    .with_signing_key(defra_node::SigningKey::Ed25519(key))
                    .with_node_acp_enabled()
                    .with_http(defra_node::HttpConfig::with_listener(listener)?)
                    .with_storage_backend(defra_node::StorageBackend::Regolith)
                    .data_path(data_path)
                    .build()
                    .await?,
            );
            Ok::<_, anyhow::Error>((node, address))
        }
    };
    let (node, address) = open(runtime_key.clone()).await?;
    node.add_dac_policy_with_counter_as(
        &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
        0,
    )
    .await?;
    node.add_collection(gents_protocol::schemas::NODE).await?;
    node.add_collection(gents_protocol::schemas::DATA_PLANE_PAIRING_DESIRED)
        .await?;
    let runtime_access = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    for actor in [user.did(), agent.did(), sibling.did(), outsider.did()] {
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
            NodePermission::CollectionGet,
        ] {
            runtime_access
                .grant_node_permission(&identity::Did::new(actor)?, permission)
                .await?;
        }
    }
    let http = |actor: &crate::KeyIdentity, address: std::net::SocketAddr| {
        Ok::<_, anyhow::Error>(ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), actor.did())
                .with_client_signing_capability(actor.client_signing_capability()?)?,
        ))
    };
    let user_access = http(&user, address)?;
    let outsider_access = http(&outsider, address)?;
    let parent = user_access
        .write(
            "test.overlay.node",
            &format!(
                r#"mutation {{ create_Node(input: {{node_did:"{}"}}) {{_docID}} }}"#,
                escape_graphql_string(runtime.did())
            ),
        )
        .await?;
    let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
    user_access
        .add_actor_relationship(
            "Node",
            &parent,
            "runtime_reader",
            &identity::Did::new(runtime.did())?,
        )
        .await?;
    let agent_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(agent.did())?,
        agent.as_ref(),
    )?;
    let sibling_access = ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(sibling.did())?,
        sibling.as_ref(),
    )?;
    let doc = agent_access
        .transact_write("test.overlay.create", |txn| {
            let create = format!(
                r#"mutation {{ create_DataPlanePairingDesired(input: {{peer_id: "peer", node_did: "{}", actor_did: "{}", template: "machine", source: "engineer"}}) {{_docID}} }}"#,
                escape_graphql_string(runtime.did()),
                escape_graphql_string(agent.did())
            );
            let parent = parent.clone();
            Box::pin(async move {
                let response = txn.execute(&create).await?;
                let doc = gents_protocol::graphql::extract_mutation_doc_id(
                    &response,
                    "DataPlanePairingDesired",
                )?;
                txn.add_resource_relationship(
                    "DataPlanePairingDesired",
                    &doc,
                    "parent",
                    "nodes",
                    &parent,
                )
                .await?;
                Ok::<_, anyhow::Error>(doc)
            })
        })
        .await?;
    let query = format!(
        r#"{{ DataPlanePairingDesired(filter: {{_docID: {{_eq:"{}"}}}}) {{_docID template}} }}"#,
        escape_graphql_string(&doc)
    );
    for revoked in [false, true] {
        if revoked {
            agent_access
                .delete_resource_relationship(
                    "DataPlanePairingDesired",
                    &doc,
                    "parent",
                    "nodes",
                    &parent,
                )
                .await?;
        }
        for case in model
            .pairing_overlay_cases
            .iter()
            .filter(|case| case["revoked"] == revoked)
        {
            let access = match case["actor"].as_str().unwrap() {
                "user" => &user_access,
                "runtime" => &runtime_access,
                "agent" => &agent_access,
                "sibling" => &sibling_access,
                "outsider" => &outsider_access,
                _ => unreachable!(),
            };
            let expected = case["expected"].as_bool().unwrap();
            let actual = if case["permission"] == "read" {
                !gents_protocol::graphql::graphql_rows_from_response(
                    &access.execute(&query).await?,
                    "DataPlanePairingDesired",
                )
                .is_empty()
            } else {
                let before = agent_access.execute(&query).await?;
                let mutation = format!(
                    r#"mutation {{ update_DataPlanePairingDesired(docID:"{}", input: {{template:"{}"}}) {{_docID}} }}"#,
                    escape_graphql_string(&doc),
                    escape_graphql_string(&format!(
                        "{}-{revoked}",
                        case["actor"].as_str().unwrap()
                    ))
                );
                let updated = match access.write("test.overlay.update", &mutation).await {
                    Ok(value) => !gents_protocol::graphql::graphql_rows_from_response(
                        &value,
                        "update_DataPlanePairingDesired",
                    )
                    .is_empty(),
                    Err(error) if error.is_permission_denied() || error.is_graphql_rejection() => {
                        false
                    }
                    Err(error) => return Err(error.into()),
                };
                if !expected {
                    assert_eq!(agent_access.execute(&query).await?, before);
                }
                updated
            };
            assert_eq!(actual, expected, "{case}");
        }
    }
    drop(agent_access);
    drop(sibling_access);
    drop(runtime_access);
    drop(user_access);
    drop(outsider_access);
    node.shutdown().await?;
    drop(node);

    let (reopened, address) = open(runtime_key).await?;
    assert!(gents_protocol::graphql::graphql_rows_from_response(
        &http(&user, address)?.execute(&query).await?,
        "DataPlanePairingDesired"
    )
    .is_empty());
    reopened.shutdown().await?;
    Ok(())
}
