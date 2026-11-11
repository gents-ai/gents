use super::{ConfigAccess, GraphqlEndpoint};
use crate::identity::NodeIdentity;
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

fn client(endpoint: &str, key: &crate::identity::KeyIdentity) -> anyhow::Result<ConfigAccess> {
    Ok(ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(endpoint, key.did())
            .with_client_signing_capability(key.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    ))
}

/// This schema remains dormant in production. This exercises a native
/// ownership boundary without treating payload identity fields as grants.
#[tokio::test]
async fn native_node_scoped_file_requests_match_modeled_access() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let user =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
    let runtime =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("runtime.key"), None)?;
    let agent =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("agent.key"), None)?;
    let other =
        crate::identity::KeyIdentity::load_or_create(directory.path().join("other.key"), None)?;
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
    let node_doc = crate::graphql::created_doc_id(&node_response, "Node")?;
    let cases = gents_lean_vocab::families::node_file_requests_access::cases();
    let schemas = [(
        "FileInventoryRequest",
        gents_protocol::schemas::FILE_INVENTORY_REQUEST,
    )];
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
        let input = json!({"request_key": format!("request-{name}"), "requester_did": user.did(), "node_did":runtime.did(), "session_id":"session", "status":"pending"});
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
                    let doc = crate::graphql::created_doc_id(&result, name)?;
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
        let doc = crate::graphql::created_doc_id(&response, name)?;
        let rows =
            crate::graphql::mutation_documents(&response, &format!("create_{name}"))?.unwrap();
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
                    let doc = crate::graphql::created_doc_id(&result, name)?;
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
                crate::graphql::escape_graphql_string(&abort_key)
            ))
            .await?;
        let aborted_document = !envelope_rows(&aborted_rows, name)?.is_empty();
        let key_of = |actor: &str| match actor {
            "user" => &user,
            "runtime" => &runtime,
            "agent" => &agent,
            "other" => &other,
            actor => panic!("unknown modeled actor {actor}"),
        };
        let permission_of = |permission: &str| match permission {
            "read" => defra_node::DocumentPermission::Read,
            "update" => defra_node::DocumentPermission::Update,
            "delete" => defra_node::DocumentPermission::Delete,
            permission => panic!("unknown modeled permission {permission}"),
        };
        let mut granted = Vec::new();
        for case in &cases {
            let key = key_of(&case.input.actor);
            let allowed = node
                .document_acp()
                .check_doc_access(
                    &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                    permission_of(&case.input.permission),
                    gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                    "configuration",
                    &doc,
                )
                .await?;
            granted.push(allowed);
            if case.input.permission == "read" {
                let response = client(&url, key)?
                    .execute(&format!(
                        "{{ {name}(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ _docID status }} }}",
                        crate::graphql::escape_graphql_string(&doc)
                    ))
                    .await?;
                let rows = envelope_rows(&response, name)?;
                assert_eq!(
                    !rows.is_empty(),
                    allowed,
                    "native HTTP visibility for {name}: {} read",
                    case.input.actor
                );
            }
        }
        let granted_to = |granted: &[bool], actor: &str, permission: &str| {
            let case = cases
                .iter()
                .position(|case| case.input.actor == actor && case.input.permission == permission)
                .expect("modeled access case");
            granted[case]
        };
        let runtime_access = client(&url, &runtime)?;
        let update = format!("mutation($input: {name}MutationInputArg!) {{ update_{name}(docID: \"{}\", input: $input) {{ _docID }} }}", crate::graphql::escape_graphql_string(&doc));
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
                !crate::graphql::mutation_documents(&response, &format!("update_{name}"))?
                    .unwrap_or_default()
                    .is_empty()
            }
            Err(_) => false,
        };
        assert_eq!(outsider_changed, granted_to(&granted, "other", "update"));
        let owner_observation = user_access
            .execute(&format!(
                "{{ {name}(filter: {{_docID: {{_eq: \"{}\"}}}}) {{ status }} }}",
                crate::graphql::escape_graphql_string(&doc)
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
            crate::graphql::mutation_documents(&update_result, &format!("update_{name}"))?.unwrap();
        assert_eq!(updated.len() == 1, granted_to(&granted, "runtime", "update"));
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
                crate::graphql::escape_graphql_string(&doc)
            ))
            .await?;
        let mut granted_after_revocation = Vec::new();
        for case in &cases {
            let key = key_of(&case.input.actor);
            granted_after_revocation.push(
                node.document_acp()
                    .check_doc_access(
                        &defra_node::AcpIdentity::Authenticated(key.did().parse()?),
                        permission_of(&case.input.permission),
                        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
                        "configuration",
                        &doc,
                    )
                    .await?,
            );
        }
        assert_eq!(
            !envelope_rows(&denied, name)?.is_empty(),
            granted_to(&granted_after_revocation, "runtime", "read")
        );
        for (index, case) in cases.iter().enumerate() {
            case.check(
                gents_lean_vocab::families::node_file_requests_access::AccessOutput {
                    allowed: granted[index],
                    revoked_allowed: granted_after_revocation[index],
                    aborted_document,
                },
            );
        }
    }
    node.shutdown().await?;
    Ok(())
}
