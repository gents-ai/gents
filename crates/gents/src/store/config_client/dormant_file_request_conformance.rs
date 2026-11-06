use super::{ConfigAccess, GraphqlEndpoint};
use crate::store::identity::NodeIdentity;
use serde_json::{json, Value};
use std::sync::Arc;

fn envelope_rows<'a>(response: &'a Value, name: &str) -> anyhow::Result<&'a [Value]> {
    response
        .get("data")
        .and_then(|data| data.get(name))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("{name} response omitted rows"))
}

fn client(endpoint: &str, key: &crate::KeyIdentity) -> anyhow::Result<ConfigAccess> {
    Ok(ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(endpoint, key.did())
            .with_client_signing_capability(key.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    ))
}

/// These schemas remain dormant in production. This exercises a native
/// ownership boundary without treating payload identity fields as grants.
#[tokio::test]
async fn native_node_scoped_file_requests_match_modeled_access() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let user = crate::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
    let runtime = crate::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)?;
    let agent = crate::KeyIdentity::load_or_create(directory.path().join("agent.key"), None)?;
    let other = crate::KeyIdentity::load_or_create(directory.path().join("other.key"), None)?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let node = Arc::new(
        defra_node::Node::builder()
            .enable_signing()
            .with_node_identity_did(runtime.did())
            .with_node_acp_owner(user.did().to_owned())
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(user.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
    assert_eq!(policy, gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID);
    for key in [&runtime, &agent, &other] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::DocumentUpdate,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), key.did())
                .await?;
        }
    }
    node.add_collection_as(&context, gents_protocol::schemas::NODE)
        .await?;
    let url = format!("http://{address}/api/v0/graphql");
    let user_access = client(&url, &user)?;
    let runtime_did = runtime.did().to_owned();
    let node_response = user_access
        .transact_write("test.file_request.node", |txn| {
            Box::pin(async {
                txn.execute_client_signed_create(
            "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
            &json!({"input": {"node_did":runtime.did()}}), gents_protocol::schemas::NODE,
        ).await
            })
        })
        .await?;
    let node_doc = crate::store::graphql::created_doc_id(&node_response, "Node")?;
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().node_file_request_access_cases;
    assert!(!cases.is_empty());
    let schemas = [
        (
            "SessionHydrationRequest",
            gents_protocol::schemas::SESSION_HYDRATION_REQUEST,
        ),
        (
            "FileImageHydrationRequest",
            gents_protocol::schemas::FILE_IMAGE_HYDRATION_REQUEST,
        ),
        (
            "FileInventoryImageRequest",
            gents_protocol::schemas::FILE_INVENTORY_IMAGE_REQUEST,
        ),
        (
            "FileInventoryRequest",
            gents_protocol::schemas::FILE_INVENTORY_REQUEST,
        ),
    ];
    for (name, schema) in schemas {
        let binding = format!(
            "type {name} @policy(id: \"{}\", resource: \"configuration\") {{",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
        );
        assert!(
            schema.contains(&binding),
            "production schema must carry the modeled binding"
        );
        node.add_collection_as(&context, schema).await?;
        let mut input = json!({"request_key": format!("request-{name}"), "requester_did": user.did(), "node_did":runtime.did(), "session_id":"session", "status":"pending"});
        if name != "SessionHydrationRequest" {
            input["peer_id"] = json!("peer");
        }
        if name == "FileImageHydrationRequest" || name == "FileInventoryImageRequest" {
            input["request_doc_id"] = json!("request");
            input["tool_call_doc_id"] = json!("tool");
        }
        if name == "FileInventoryImageRequest" {
            input["inventory_reference"] = json!({"fixture":true});
        }
        let mutation = format!("mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID _version {{ cid }} }} }}");
        let variables = json!({"input":input});
        let response = user_access
            .transact_write("test.file_request.create", |txn| {
                let mutation = &mutation;
                let variables = &variables;
                let node_doc = &node_doc;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    let result = txn
                        .execute_client_signed_create(mutation, variables, schema)
                        .await?;
                    let doc = crate::store::graphql::created_doc_id(&result, name)?;
                    txn.add_resource_relationship(name, &doc, "parent", "nodes", node_doc)
                        .await?;
                    txn.add_actor_relationship(
                        name,
                        &doc,
                        "writer",
                        &::identity::Did::new(runtime_did)?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(result)
                })
            })
            .await?;
        let doc = crate::store::graphql::created_doc_id(&response, name)?;
        let rows = crate::store::graphql::mutation_documents(&response, &format!("create_{name}"))?
            .unwrap();
        let cid = rows[0]["_version"][0]["cid"].as_str().unwrap();
        assert_eq!(node.verified_block_signer_did(cid).await?, user.did());
        let mut aborted_variables = variables.clone();
        let abort_key = format!("aborted-{name}");
        aborted_variables["input"]["request_key"] = json!(abort_key);
        let aborted: anyhow::Result<()> = user_access
            .transact_write("test.file_request.abort", |txn| {
                let mutation = &mutation;
                let variables = &aborted_variables;
                let node_doc = &node_doc;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    let result = txn
                        .execute_client_signed_create(mutation, variables, schema)
                        .await?;
                    let doc = crate::store::graphql::created_doc_id(&result, name)?;
                    txn.add_resource_relationship(name, &doc, "parent", "nodes", node_doc)
                        .await?;
                    txn.add_actor_relationship(
                        name,
                        &doc,
                        "writer",
                        &::identity::Did::new(runtime_did)?,
                    )
                    .await?;
                    anyhow::bail!("intentional transaction abort")
                })
            })
            .await;
        assert!(aborted.is_err());
        let aborted_rows = user_access
            .execute(&format!(
                "{{ {name}(filter: {{request_key: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                crate::store::graphql::escape_graphql_string(&abort_key)
            ))
            .await?;
        assert_eq!(
            !envelope_rows(&aborted_rows, name)?.is_empty(),
            cases[0]["aborted_document"].as_bool().unwrap()
        );
        for case in cases {
            let key = match case["actor"].as_str().unwrap() {
                "user" => &user,
                "runtime" => &runtime,
                "agent" => &agent,
                "other" => &other,
                actor => panic!("unknown modeled actor {actor}"),
            };
            let permission = match case["permission"].as_str().unwrap() {
                "read" => defra_node::DocumentPermission::Read,
                "update" => defra_node::DocumentPermission::Update,
                "delete" => defra_node::DocumentPermission::Delete,
                permission => panic!("unknown modeled permission {permission}"),
            };
            let allowed = node
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                    permission,
                    gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                    "configuration",
                    &doc,
                )
                .await?;
            assert_eq!(
                allowed,
                case["expected"].as_bool().unwrap(),
                "{name}: {case}"
            );
            if case["permission"] == "read" {
                let response = client(&url, key)?
                    .execute(&format!(
                        "{{ {name}(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID status }} }}",
                        crate::store::graphql::escape_graphql_string(&doc)
                    ))
                    .await?;
                let rows = envelope_rows(&response, name)?;
                assert_eq!(
                    !rows.is_empty(),
                    allowed,
                    "native HTTP visibility for {name}: {case}"
                );
            }
        }
        let runtime_access = client(&url, &runtime)?;
        let update = format!("mutation($input: {name}MutationInputArg!) {{ update_{name}(docID: \"{}\", input: $input) {{ _docID }} }}", crate::store::graphql::escape_graphql_string(&doc));
        let outsider = client(&url, &other)?;
        let denied_update = outsider
            .transact_write("test.file_request.denied_update", |txn| {
                let update = &update;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        update,
                        &json!({"input":{"status":"stolen"}}),
                        schema,
                    )
                    .await
                })
            })
            .await;
        let outsider_changed = match denied_update {
            Ok(response) => {
                !crate::store::graphql::mutation_documents(&response, &format!("update_{name}"))?
                    .unwrap_or_default()
                    .is_empty()
            }
            Err(_) => false,
        };
        let outsider_update = cases
            .iter()
            .find(|case| case["actor"] == "other" && case["permission"] == "update")
            .unwrap();
        assert_eq!(
            outsider_changed,
            outsider_update["expected"].as_bool().unwrap()
        );
        let owner_observation = user_access
            .execute(&format!(
                "{{ {name}(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ status }} }}",
                crate::store::graphql::escape_graphql_string(&doc)
            ))
            .await?;
        assert_eq!(owner_observation["data"][name][0]["status"], "pending");
        let update_result = runtime_access
            .transact_write("test.file_request.update", |txn| {
                let update = &update;
                Box::pin(async move {
                    txn.execute_client_signed_update(
                        update,
                        &json!({"input":{"status":"served"}}),
                        schema,
                    )
                    .await
                })
            })
            .await?;
        let updated =
            crate::store::graphql::mutation_documents(&update_result, &format!("update_{name}"))?
                .unwrap();
        let runtime_update = cases
            .iter()
            .find(|case| case["actor"] == "runtime" && case["permission"] == "update")
            .unwrap();
        assert_eq!(
            updated.len() == 1,
            runtime_update["expected"].as_bool().unwrap()
        );
        user_access
            .transact_write("test.file_request.revoke", |txn| {
                let doc = &doc;
                let runtime_did = &runtime_did;
                Box::pin(async move {
                    txn.delete_actor_relationship(
                        name,
                        doc,
                        "writer",
                        &::identity::Did::new(runtime_did)?,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let denied = runtime_access
            .execute(&format!(
                "{{ {name}(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID }} }}",
                crate::store::graphql::escape_graphql_string(&doc)
            ))
            .await?;
        let runtime_read = cases
            .iter()
            .find(|case| case["actor"] == "runtime" && case["permission"] == "read")
            .unwrap();
        assert_eq!(
            !envelope_rows(&denied, name)?.is_empty(),
            runtime_read["revoked_expected"].as_bool().unwrap()
        );
        let still_allowed = node
            .document_acp()
            .check_doc_access(
                &defra_node::AcpIdentity::Authenticated(runtime.did().parse()?),
                defra_node::DocumentPermission::Update,
                gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                "configuration",
                &doc,
            )
            .await?;
        assert_eq!(
            still_allowed,
            runtime_update["revoked_expected"].as_bool().unwrap()
        );
    }
    node.shutdown().await?;
    Ok(())
}
