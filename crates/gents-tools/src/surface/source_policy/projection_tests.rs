use std::sync::Arc;

use acp::{NodePermission, Subject, ZanzibarStore};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_store::local::tool_permission_projection::{
    decode_source_for_tests, ToolPermissionProjection,
};
use storage::namespace::Namespace;

async fn projection_node() -> anyhow::Result<(
    tempfile::TempDir,
    gents_store::identity::KeyIdentity,
    Arc<defra_node::Node>,
)> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime");
    let runtime = gents_store::identity::KeyIdentity::load_or_create(&path, None)?;
    let key = gents_store::identity::load_file_identity(&path)?.private_key_bytes();
    let projector = Arc::new(
        gents_store::local::runtime_document_projection::RuntimeDocumentProjector::new(
            Arc::new(gents_store::identity::KeyIdentity::load_or_create(
                &path, None,
            )?),
            Arc::new(crate::surface::ToolCeiling::meta_only()),
        )?,
    );
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(key))
            .with_node_acp_enabled()
            .with_document_projector(projector)
            .build()
            .await?,
    );
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    Ok((directory, runtime, node))
}

async fn observed_plugin_input(
    owner: &gents_store::config_client::ConfigAccess,
    runtime: &gents_store::identity::KeyIdentity,
    installed: &[gents_store::local::ProjectedPlugin],
    actor: &identity::Did,
    direct: bool,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    let snapshot = owner.execute(r#"{ Agent { node_did agent_did creation_nonce context_id enabled } AgentContext { _docID node_did context_id tools_id } Tools { _docID node_did tools_id integrations } }"#).await?;
    let rows = |kind: &str| -> anyhow::Result<Vec<serde_json::Value>> {
        Ok(snapshot["data"][kind]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing observed {kind}"))?
            .clone())
    };
    let mut agents = Vec::new();
    for agent in rows("Agent")? {
        let text = |field: &str| agent[field].as_str().unwrap_or_default().to_owned();
        let binding = gents_protocol::agent_identity::AgentIdentityBinding {
            agent_did: text("agent_did"),
            creation_nonce: text("creation_nonce"),
        };
        let derived = match gents_store::identity::resolve_agent_identity(runtime, &binding) {
            Ok(signer) => signer.did().to_owned(),
            Err(gents_store::identity::IdentityError::AgentIdentityMismatch {
                derived_did,
                ..
            }) => derived_did,
            Err(_) => String::new(),
        };
        agents.push(
            json!({"nodeDid":text("node_did"), "agentDid":binding.agent_did,
            "derived":derived, "contextId":text("context_id"),
            "enabled":agent["enabled"].as_bool().unwrap_or(true)}),
        );
    }
    let contexts = rows("AgentContext")?
        .into_iter()
        .map(|context| {
            json!({
                "nodeDid":context["node_did"].as_str().unwrap_or_default(),
                "docId":context["_docID"].as_str().unwrap_or_default(),
                "contextId":context["context_id"].as_str().unwrap_or_default(),
                "toolsId":context["tools_id"].as_str().unwrap_or_default(),
            })
        })
        .collect::<Vec<_>>();
    let tools = rows("Tools")?.into_iter().map(|tools| {
        let plugins = tools["integrations"]["plugins"].as_array().into_iter().flatten().map(|plugin| json!({
            "coordinate":plugin["plugin"].as_str().unwrap_or_default(), "pin":plugin["digest"].as_str(),
        })).collect::<Vec<_>>();
        json!({"nodeDid":tools["node_did"].as_str().unwrap_or_default(),
            "docId":tools["_docID"].as_str().unwrap_or_default(),
            "toolsId":tools["tools_id"].as_str().unwrap_or_default(), "plugins":plugins})
    }).collect::<Vec<_>>();
    let installed = installed
        .iter()
        .map(|plugin| {
            json!({
                "coordinate":plugin.coordinate, "digest":plugin.digest,
                "operations":if plugin.deletes { vec!["delete"] } else { Vec::new() },
            })
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"node":runtime.did(), "installed":installed, "agents":agents,
        "contexts":contexts, "tools":tools, "actor":actor.as_str(), "direct":direct}),
    )
}

async fn observe_plugin_permission(
    observations: &mut Vec<(serde_json::Value, bool)>,
    owner: &gents_store::config_client::ConfigAccess,
    runtime: &gents_store::identity::KeyIdentity,
    node: &defra_node::Node,
    installed: &[gents_store::local::ProjectedPlugin],
    actor: &identity::Did,
    direct: bool,
) -> anyhow::Result<()> {
    let input = observed_plugin_input(owner, runtime, installed, actor, direct).await?;
    let observed = node
        .nac_manager()
        .check_permission(actor, NodePermission::DocumentDelete)
        .await?;
    observations.push((input, observed));
    Ok(())
}

#[tokio::test]
async fn selected_datastore_query_grants_collection_access_until_removed() -> anyhow::Result<()> {
    let (_directory, runtime, node) = projection_node().await?;
    let binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let sibling_binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let agent_signer = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
    let node_value = escape_graphql_string(runtime.did());
    let agent_value = escape_graphql_string(&binding.agent_did);
    let sibling_value = escape_graphql_string(&sibling_binding.agent_did);
    let nonce = escape_graphql_string(&binding.creation_nonce);
    let sibling_nonce = escape_graphql_string(&sibling_binding.creation_nonce);
    let owner = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did())?,
    );
    let entries = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "entries": [{"kind":"query", "tool_name":"find_probe", "collection":"Probe",
            "description":"Find a probe", "fields":["name"]}]
    }))?;
    let datastore = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "datastore_tool_surface_ids": ["surface"]
    }))?;
    for mutation in [
        format!(
            r#"mutation {{ create_DatastoreToolSurface(input: {{node_did:"{node_value}",surface_id:"surface",enabled:true,entries:{entries}}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"tools",datastore:{datastore}}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"context",tools_id:"tools"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"context",enabled:true}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{sibling_value}",creation_nonce:"{sibling_nonce}",enabled:true}}) {{_docID}} }}"#
        ),
    ] {
        owner
            .write("test.tool_projection.create", &mutation)
            .await?;
    }
    let agent = identity::Did::new(&binding.agent_did)?;
    let sibling = identity::Did::new(&sibling_binding.agent_did)?;
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.clone(),
        agent_signer.as_ref(),
    )?;
    let mut granted = std::collections::BTreeMap::new();
    granted.insert(
        "agent-query",
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionGet)
            .await?,
    );
    assert!(access.collection_version("Node").await?.is_some());
    granted.insert(
        "sibling",
        node.nac_manager()
            .check_permission(&sibling, NodePermission::CollectionGet)
            .await?,
    );
    owner.write("test.tool_projection.schema_enable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"tools"}}}},input:{{built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#)).await?;
    owner.write("test.tool_projection.surface_disable", &format!(r#"mutation {{ update_DatastoreToolSurface(filter: {{node_did:{{_eq:"{node_value}"}},surface_id:{{_eq:"surface"}}}},input:{{enabled:false}}) {{_docID}} }}"#)).await?;
    granted.insert(
        "agent-schema",
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionGet)
            .await?,
    );
    owner.write("test.tool_projection.schema_disable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"tools"}}}},input:{{built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#)).await?;
    granted.insert(
        "agent-disabled",
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionGet)
            .await?,
    );
    assert!(access.collection_version("Node").await.is_err());
    for case in gents_lean_vocab::families::household_tool_operation_collection_get::cases() {
        let observed = granted[case.input.step.as_str()];
        case.check(observed);
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn http_non_admin_tools_writer_cannot_grant_managed_agent_schema_authority(
) -> anyhow::Result<()> {
    use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
    use serde_json::json;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime");
    let runtime = gents_store::identity::KeyIdentity::load_or_create(&path, None)?;
    let user =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let key = gents_store::identity::load_file_identity(&path)?.private_key_bytes();
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let projector = Arc::new(
        gents_store::local::runtime_document_projection::RuntimeDocumentProjector::new(
            Arc::new(gents_store::identity::KeyIdentity::load_or_create(
                &path, None,
            )?),
            Arc::new(crate::surface::ToolCeiling::meta_only()),
        )?,
    );
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(key))
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_document_projector(projector)
            .build()
            .await?,
    );
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    let user_did = identity::Did::new(user.did())?;
    let agent_did = identity::Did::new(&binding.agent_did)?;
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
    ] {
        owner.grant_node_permission(&user_did, permission).await?;
    }
    let node_value = escape_graphql_string(runtime.did());
    let agent_value = escape_graphql_string(&binding.agent_did);
    let nonce = escape_graphql_string(&binding.creation_nonce);
    let response = owner.write("test.http_projection.tools", &format!(
        r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"http-tools",display_name:"before",built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#
    )).await?;
    let tools_doc = gents_store::graphql::created_doc_id(&response, "Tools")?;
    for mutation in [
        format!(
            r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"http-context",tools_id:"http-tools"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"http-context",enabled:true}}) {{_docID}} }}"#
        ),
    ] {
        owner.write("test.http_projection.setup", &mutation).await?;
    }
    owner
        .add_actor_relationship("Tools", &tools_doc, "writer", &user_did)
        .await?;
    assert!(!node.nac_manager().is_admin(&user_did).await?);
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent_did, NodePermission::CollectionPatch)
            .await?
    );
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    let mutation = format!(
        r#"mutation($input: ToolsMutationInputArg!) {{ update_Tools(docID:"{}", input:$input) {{_docID}} }}"#,
        escape_graphql_string(&tools_doc)
    );
    let harmless = json!({"input":{"display_name":"reviewed"}});
    let response = access
        .transact_write("test.http_projection.harmless", |txn| {
            let (mutation, harmless) = (&mutation, &harmless);
            Box::pin(async move {
                txn.execute_desired_state_mutation(
                    mutation,
                    harmless,
                    gents_store::collection::Collection::Tools,
                    false,
                )
                .await
            })
        })
        .await?;
    assert_eq!(response["data"]["update_Tools"][0]["_docID"], tools_doc);
    let elevated = json!({"input":{"built_ins":{"enable_schema_tool":true}}});
    let failure = access
        .transact_write("test.http_projection.denied", |txn| {
            let (mutation, elevated) = (&mutation, &elevated);
            Box::pin(async move {
                txn.execute_desired_state_mutation(
                    mutation,
                    elevated,
                    gents_store::collection::Collection::Tools,
                    false,
                )
                .await
            })
        })
        .await
        .expect_err("a non-admin cannot create the required native source grant");
    assert!(failure.is_permission_denied(), "{failure:?}");
    assert!(!failure.is_retryable_database_conflict());
    let persisted = owner
        .execute(&format!(
            r#"{{ Tools(filter: {{_docID: {{_eq: "{}"}}}}) {{ _docID display_name built_ins }} }}"#,
            escape_graphql_string(&tools_doc)
        ))
        .await?;
    assert_eq!(persisted["data"]["Tools"][0]["_docID"], tools_doc);
    assert_eq!(persisted["data"]["Tools"][0]["display_name"], "reviewed");
    assert_eq!(
        persisted["data"]["Tools"][0]["built_ins"]["enable_schema_tool"],
        false
    );
    let granted = node
        .nac_manager()
        .check_permission(&agent_did, NodePermission::CollectionPatch)
        .await?;
    for case in gents_lean_vocab::families::household_tool_grantor_http::cases() {
        anyhow::ensure!(
            case.input.grantor == "writer" && case.input.committed,
            "the HTTP scenario drives only the committed Tools writer: {:?}",
            case.input
        );
        case.check(granted);
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn user_signed_configuration_cannot_grant_schema_to_foreign_identity() -> anyhow::Result<()> {
    use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
    use serde_json::json;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime");
    let runtime = gents_store::identity::KeyIdentity::load_or_create(&path, None)?;
    let user =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user"), None)?;
    let key = gents_store::identity::load_file_identity(&path)?.private_key_bytes();
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let projector = Arc::new(
        gents_store::local::runtime_document_projection::RuntimeDocumentProjector::new(
            Arc::new(gents_store::identity::KeyIdentity::load_or_create(
                &path, None,
            )?),
            Arc::new(crate::surface::ToolCeiling::meta_only()),
        )?,
    );
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .with_signing_key(defra_node::SigningKey::Ed25519(key))
            .with_node_acp_enabled()
            .with_http(defra_node::HttpConfig::with_listener(listener)?)
            .with_document_projector(projector)
            .build()
            .await?,
    );
    gents_store::schema::ensure_runtime_schemas(&node).await?;
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(runtime.did())?);
    let user_did = identity::Did::new(user.did())?;
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
    ] {
        owner.grant_node_permission(&user_did, permission).await?;
    }
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    for (collection, mutation, input) in [
        (gents_store::collection::Collection::Tools, "mutation($input: ToolsMutationInputArg!) { create_Tools(input: $input) { _docID _version { cid } } }", json!({"node_did":runtime.did(),"tools_id":"tools","built_ins":{"enable_schema_tool":true}})),
        (gents_store::collection::Collection::AgentContext, "mutation($input: AgentContextMutationInputArg!) { create_AgentContext(input: $input) { _docID _version { cid } } }", json!({"node_did":runtime.did(),"context_id":"context","tools_id":"tools"})),
    ] {
        access.transact_write("test.user_signed_tool_configuration", |txn| {
            let input = &input;
            Box::pin(async move { txn.execute_desired_state_mutation(mutation, &json!({"input":input}), collection, true).await })
        }).await?;
    }
    let key = |name: &str| match name {
        "runtime" => gents_store::identity::KeyIdentity::load_or_create(&path, None),
        other => {
            gents_store::identity::KeyIdentity::load_or_create(directory.path().join(other), None)
        }
    };
    let projected = |subjects: &[identity::Did]| {
        let node = node.clone();
        let subjects = subjects.to_vec();
        async move {
            let mut granted = Vec::new();
            for subject in &subjects {
                granted.push(
                    node.nac_manager()
                        .check_permission(subject, NodePermission::CollectionPatch)
                        .await?,
                );
            }
            anyhow::Ok(granted)
        }
    };
    let create_agent = "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID _version { cid } } }";
    let node_did = identity::Did::new(runtime.did())?;
    let mut legacy_installed = false;
    let mut observed = Vec::new();
    for case in gents_lean_vocab::families::household_managed_tool_identity::cases() {
        let input = &case.input;
        anyhow::ensure!(
            input.node_key == "runtime",
            "projecting Node key: {input:?}"
        );
        let claimed = match (
            &input.claimed_key,
            &input.derived_node_key,
            &input.derived_creation,
        ) {
            (Some(name), _, _) => key(name)?.did().to_owned(),
            (None, Some(node_key), Some(creation_nonce)) => {
                gents_store::identity::derive_agent_identity(&key(node_key)?, creation_nonce)?
                    .did()
                    .to_owned()
            }
            _ => String::new(),
        };
        let derived = input.creation_nonce.as_deref().and_then(|nonce| {
            gents_store::identity::derive_agent_identity(&runtime, nonce)
                .ok()
                .map(|identity| identity.did().to_owned())
        });
        let mut agent_input = json!({"node_did":runtime.did(),"agent_did":claimed,
            "context_id":"context","enabled":true});
        if let Some(nonce) = &input.creation_nonce {
            agent_input["creation_nonce"] = json!(nonce);
        }
        let subjects = std::iter::once(claimed)
            .chain(derived)
            .filter(|did| !did.is_empty())
            .map(identity::Did::new)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let user_create = || {
            access.transact_write("test.user_signed_agent", |txn| {
                let agent_input = &agent_input;
                Box::pin(async move {
                    txn.execute_desired_state_mutation(
                        create_agent,
                        &json!({"input":agent_input}),
                        gents_store::collection::Collection::Agent,
                        true,
                    )
                    .await
                })
            })
        };
        let user_create_refused = match user_create().await {
            Ok(_) => false,
            Err(denied) if denied.is_permission_denied() => true,
            Err(error) => return Err(error.into()),
        };
        if user_create_refused {
            if !legacy_installed {
                legacy_installed = true;
                let projector = ToolPermissionProjection::new(
                    Arc::new(gents_store::identity::KeyIdentity::load_or_create(
                        &path, None,
                    )?),
                    Arc::new(crate::surface::ToolCeiling::meta_only()),
                )?;
                let legacy_source = format!("{}legacy", projector.source_prefix());
                let txn = node.database().new_txn(false).await?;
                let view = txn.datastore()?.sibling(Namespace::Acpstore);
                for (parent_source, subject_kind, subject) in [
                    (Some(legacy_source.as_str()), "actor", user.did()),
                    (None, "source", legacy_source.as_str()),
                ] {
                    assert!(node
                        .nac_manager()
                        .change_source_permission_in_txn(
                            view.clone(),
                            &node_did,
                            &db::nac::SourcePermissionChange {
                                permission: NodePermission::CollectionPatch,
                                parent_source,
                                subject_kind,
                                subject,
                                delete: false,
                            },
                        )
                        .await?
                        .is_some());
                }
                drop(view);
                txn.commit().await?;
                assert!(
                    node.nac_manager()
                        .check_permission(&user_did, NodePermission::CollectionPatch)
                        .await?
                );
                owner
                    .grant_node_permission(&user_did, NodePermission::CollectionPatch)
                    .await?;
            }
            let denied = user_create().await.expect_err(
                "a non-admin cannot revoke the legacy source or grant the managed Agent",
            );
            assert!(denied.is_permission_denied(), "{denied:?}");
            assert!(projected(&subjects).await?.iter().all(|granted| !granted));
            let stored = owner
                .execute(&format!(
                    r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                    escape_graphql_string(agent_input["agent_did"].as_str().unwrap_or_default())
                ))
                .await?;
            assert!(
                stored["data"]["Agent"]
                    .as_array()
                    .is_some_and(Vec::is_empty),
                "{stored}"
            );
            owner
                .transact_write("test.owner_managed_agent", |txn| {
                    let agent_input = &agent_input;
                    Box::pin(async move {
                        txn.execute_desired_state_mutation(
                            create_agent,
                            &json!({"input":agent_input}),
                            gents_store::collection::Collection::Agent,
                            true,
                        )
                        .await
                    })
                })
                .await?;
        }
        let granted = projected(&subjects).await?;
        let uniform = granted.first().copied().unwrap_or_default();
        assert!(
            granted.iter().all(|value| *value == uniform),
            "subjects of one Agent disagree: {input:?}"
        );
        observed.push((uniform, subjects));
        case.check(
            gents_lean_vocab::families::household_managed_tool_identity::ManagedIdentityOutcome {
                granted: uniform,
                user_create_refused,
            },
        );
    }
    assert!(legacy_installed, "no Agent create was refused");
    assert!(
        node.nac_manager()
            .check_permission(&user_did, NodePermission::CollectionPatch)
            .await?
    );
    owner
        .revoke_node_permission(&user_did, NodePermission::CollectionPatch)
        .await?;
    assert_eq!(
        node.nac_manager()
            .check_permission(&user_did, NodePermission::CollectionPatch)
            .await?,
        observed
            .iter()
            .any(|(granted, subjects)| *granted && subjects.contains(&user_did))
    );
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn configured_delete_sources_grant_revoke_and_preserve_independent_permissions(
) -> anyhow::Result<()> {
    let (_directory, runtime, node) = projection_node().await?;
    let binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let signer = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
    let agent = identity::Did::new(&binding.agent_did)?;
    let owner = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did())?,
    );
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
    ] {
        owner.grant_node_permission(&agent, permission).await?;
    }
    node.add_collection(&format!(
        "type DeleteProjectionProbe @policy(id: \"{}\", resource: \"configuration\") {{ name: String }}",
        gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
    )).await?;
    let node_value = escape_graphql_string(runtime.did());
    let agent_value = escape_graphql_string(&binding.agent_did);
    let nonce = escape_graphql_string(&binding.creation_nonce);
    for mutation in [
        format!(
            r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"delete-tools",self_config:{{enable_self_config:true,self_config_categories:["agent"],enable_pack_install:false}}}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"delete-context",tools_id:"delete-tools"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"delete-context",enabled:true}}) {{_docID}} }}"#
        ),
    ] {
        owner
            .write("test.delete_projection.create", &mutation)
            .await?;
    }
    let actor = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.clone(),
        signer.as_ref(),
    )?;
    let mut granted = std::collections::BTreeMap::new();
    granted.insert(
        "agent",
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?,
    );
    let created = actor
        .write(
            "test.delete_projection.create_probe",
            r#"mutation { create_DeleteProjectionProbe(input: {name:"probe"}) {_docID} }"#,
        )
        .await?;
    let doc = gents_store::graphql::created_doc_id(&created, "DeleteProjectionProbe")?;
    let delete = format!(
        r#"mutation {{ delete_DeleteProjectionProbe(docID:"{}") {{_docID}} }}"#,
        escape_graphql_string(&doc)
    );
    let denied = actor.write("test.delete_projection.denied", &delete).await;
    assert!(!denied
        .as_ref()
        .is_ok_and(|value| value["data"]["delete_DeleteProjectionProbe"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())));
    let update = |category: &str, pack: bool| {
        format!(
            r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"delete-tools"}}}},input:{{self_config:{{enable_self_config:true,self_config_categories:["{category}"],enable_pack_install:{pack}}}}}) {{_docID}} }}"#
        )
    };
    owner
        .write(
            "test.delete_projection.enable_overlap",
            &update("tools", true),
        )
        .await?;
    granted.insert(
        "tools-pack",
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?,
    );
    let deleted = actor
        .write("test.delete_projection.allowed", &delete)
        .await?;
    assert_eq!(
        deleted["data"]["delete_DeleteProjectionProbe"][0]["_docID"],
        doc
    );
    owner
        .write(
            "test.delete_projection.remove_pack",
            &update("tools", false),
        )
        .await?;
    granted.insert(
        "tools",
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?,
    );
    owner
        .write("test.delete_projection.pack_only", &update("agent", true))
        .await?;
    granted.insert(
        "agent-pack",
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?,
    );
    owner
        .write("test.delete_projection.remove_all", &update("agent", false))
        .await?;
    granted.insert(
        "agent-only",
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?,
    );
    let second = actor
        .write(
            "test.delete_projection.second_probe",
            r#"mutation { create_DeleteProjectionProbe(input: {name:"second"}) {_docID} }"#,
        )
        .await?;
    let second_doc = gents_store::graphql::created_doc_id(&second, "DeleteProjectionProbe")?;
    let second_delete = format!(
        r#"mutation {{ delete_DeleteProjectionProbe(docID:"{}") {{_docID}} }}"#,
        escape_graphql_string(&second_doc)
    );
    let denied = actor
        .write("test.delete_projection.revoked", &second_delete)
        .await;
    assert!(!denied
        .as_ref()
        .is_ok_and(|value| value["data"]["delete_DeleteProjectionProbe"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())));
    let remaining = actor
        .execute(&format!(
            r#"{{ DeleteProjectionProbe(filter: {{_docID:{{_eq:"{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(&second_doc)
        ))
        .await?;
    assert_eq!(
        remaining["data"]["DeleteProjectionProbe"][0]["_docID"],
        second_doc
    );
    owner
        .grant_node_permission(&agent, NodePermission::DocumentDelete)
        .await?;
    owner
        .write(
            "test.delete_projection.enable_again",
            &update("tools", false),
        )
        .await?;
    owner
        .write(
            "test.delete_projection.disable_again",
            &update("agent", false),
        )
        .await?;
    assert!(
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?
    );
    owner
        .revoke_node_permission(&agent, NodePermission::DocumentDelete)
        .await?;
    granted.insert(
        "direct-revoked",
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?,
    );
    for case in gents_lean_vocab::families::household_tool_operation_document_delete::cases() {
        let observed = granted[case.input.step.as_str()];
        case.check(observed);
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn selected_schema_tool_grants_and_revokes_only_its_native_source() -> anyhow::Result<()> {
    let (_directory, runtime, node) = projection_node().await?;
    let binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let agent_signer = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
    let node_value = escape_graphql_string(runtime.did());
    let agent_value = escape_graphql_string(agent_signer.did());
    let nonce = escape_graphql_string(&binding.creation_nonce);
    let owner = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did())?,
    );
    for mutation in [
        format!(
            r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"tools",built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"context",tools_id:"tools"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"context",enabled:true}}) {{_docID}} }}"#
        ),
    ] {
        owner
            .write("test.tool_projection.create", &mutation)
            .await?;
    }
    let agent = identity::Did::new(agent_signer.did())?;
    assert!(
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    owner
        .grant_node_permission(&agent, NodePermission::CollectionPatch)
        .await?;
    owner.write("test.tool_projection.schema_disable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"tools"}}}},input:{{built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#)).await?;
    assert!(
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    owner
        .revoke_node_permission(&agent, NodePermission::CollectionPatch)
        .await?;
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn non_admin_tools_writer_cannot_project_schema_authority() -> anyhow::Result<()> {
    let (_directory, runtime, node) = projection_node().await?;
    let binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let signer = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
    let agent = identity::Did::new(&binding.agent_did)?;
    let owner = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did())?,
    );
    for permission in [
        NodePermission::CollectionGet,
        NodePermission::DocumentRead,
        NodePermission::DocumentUpdate,
    ] {
        owner.grant_node_permission(&agent, permission).await?;
    }
    let node_value = escape_graphql_string(runtime.did());
    let agent_value = escape_graphql_string(&binding.agent_did);
    let nonce = escape_graphql_string(&binding.creation_nonce);
    let response = owner.write("test.projection.writer_tools", &format!(
        r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"writer-tools",built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#
    )).await?;
    let tools_doc = gents_store::graphql::created_doc_id(&response, "Tools")?;
    for mutation in [
        format!(
            r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"writer-context",tools_id:"writer-tools"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"writer-context",enabled:true}}) {{_docID}} }}"#
        ),
    ] {
        owner
            .write("test.projection.writer_setup", &mutation)
            .await?;
    }
    owner
        .add_actor_relationship("Tools", &tools_doc, "writer", &agent)
        .await?;
    assert!(!node.nac_manager().is_admin(&agent).await?);
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    let mut changed = std::collections::BTreeMap::new();
    let mut granted = std::collections::BTreeMap::new();

    let writer = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.clone(),
        signer.as_ref(),
    )?;
    let mutation = format!(
        r#"mutation($input: ToolsMutationInputArg!) {{ update_Tools(docID:"{}", input:$input) {{_docID}} }}"#,
        escape_graphql_string(&tools_doc)
    );
    let harmless = serde_json::json!({"input":{"built_ins":{"enable_schema_tool":false}}});
    let control = writer
        .transact_write("test.projection.writer_control", |txn| {
            let mutation = &mutation;
            let harmless = &harmless;
            Box::pin(async move {
                txn.execute_client_signed_update(mutation, harmless, gents_protocol::schemas::TOOLS)
                    .await
            })
        })
        .await;
    if let Ok(response) = &control {
        assert_eq!(response["data"]["update_Tools"][0]["_docID"], tools_doc);
    }
    changed.insert("writer-control", control.is_ok());

    let elevated = serde_json::json!({"input":{"built_ins":{"enable_schema_tool":true}}});
    let attempt = writer
        .transact_write("test.projection.writer_schema", |txn| {
            let mutation = &mutation;
            let elevated = &elevated;
            Box::pin(async move {
                txn.execute_client_signed_update(mutation, elevated, gents_protocol::schemas::TOOLS)
                    .await
            })
        })
        .await;
    changed.insert("writer-elevated", attempt.is_ok());
    let persisted = owner
        .execute(&format!(
            r#"{{ Tools(filter: {{_docID: {{_eq: "{}"}}}}) {{ _docID built_ins }} }}"#,
            escape_graphql_string(&tools_doc)
        ))
        .await?;
    assert_eq!(persisted["data"]["Tools"][0]["_docID"], tools_doc);
    assert_eq!(
        persisted["data"]["Tools"][0]["built_ins"]["enable_schema_tool"],
        false
    );
    granted.insert(
        "writer",
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?,
    );
    let owner_schema = format!(
        r#"mutation {{ update_Tools(docID:"{}", input:{{built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#,
        escape_graphql_string(&tools_doc)
    );
    let (rolled_back, mutations) =
        gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            owner.transact_write("test.projection.owner_rolled_back", |txn| {
                let owner_schema = &owner_schema;
                Box::pin(
                    async move { txn.execute(owner_schema).await.map_err(anyhow::Error::from) },
                )
            }),
        )
        .await;
    assert_eq!(mutations, 1);
    assert!(rolled_back.is_err());
    granted.insert(
        "owner-rolled-back",
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?,
    );
    changed.insert(
        "owner-enable",
        owner
            .write("test.projection.owner_schema", &owner_schema)
            .await
            .is_ok(),
    );
    granted.insert(
        "owner",
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?,
    );
    let unchanged = writer
        .transact_write("test.projection.writer_noop", |txn| {
            let mutation = &mutation;
            let elevated = &elevated;
            Box::pin(async move {
                txn.execute_client_signed_update(mutation, elevated, gents_protocol::schemas::TOOLS)
                    .await
            })
        })
        .await;
    changed.insert("writer-noop", unchanged.is_ok());
    let disable = writer
        .transact_write("test.projection.writer_disable", |txn| {
            let mutation = &mutation;
            let harmless = &harmless;
            Box::pin(async move {
                txn.execute_client_signed_update(mutation, harmless, gents_protocol::schemas::TOOLS)
                    .await
            })
        })
        .await;
    changed.insert("writer-disable", disable.is_ok());
    assert!(
        node.nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    changed.insert(
        "owner-disable",
        owner
            .write("test.projection.owner_disable", &format!(
                r#"mutation {{ update_Tools(docID:"{}", input:{{built_ins:{{enable_schema_tool:false}}}}) {{_docID}} }}"#,
                escape_graphql_string(&tools_doc)
            ))
            .await
            .is_ok(),
    );
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    let dormant = writer
        .transact_write("test.projection.writer_dormant", |txn| {
            let mutation = &mutation;
            let elevated = &elevated;
            Box::pin(async move {
                txn.execute_client_signed_update(mutation, elevated, gents_protocol::schemas::TOOLS)
                    .await
            })
        })
        .await;
    changed.insert("writer-dormant", dormant.is_ok());
    for case in gents_lean_vocab::families::household_tool_source_change_schema::cases() {
        let observed = changed[case.input.step.as_str()];
        case.check(observed);
    }
    for case in gents_lean_vocab::families::household_tool_grantor_embedded::cases() {
        let observed = granted[case.input.step.as_str()];
        case.check(observed);
    }
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    owner.write("test.projection.owner_restore", &format!(
        r#"mutation {{ update_Tools(docID:"{}", input:{{built_ins:{{enable_schema_tool:true}}}}) {{_docID}} }}"#,
        escape_graphql_string(&tools_doc)
    )).await?;
    node.database().reconcile_document_projector().await?;
    let txn = node.database().new_txn(false).await?;
    let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
        txn.datastore()?.sibling(Namespace::Acpstore),
    ));
    let roots = store
        .get_relation_subjects(
            acp::NODE_POLICY_ID,
            acp::NODE_RESOURCE_NAME,
            acp::NODE_OBJECT_ID,
            NodePermission::CollectionPatch.as_str(),
        )
        .await?;
    let source = roots
        .into_iter()
        .filter_map(|subject| match subject {
            Subject::EntitySet {
                resource,
                object_id,
                relation,
            } if resource == acp::NODE_RESOURCE_NAME
                && relation == NodePermission::CollectionPatch.as_str() =>
            {
                decode_source_for_tests(&object_id)
                    .filter(|source| source.starts_with("gents-tool-v1:"))
            }
            _ => None,
        })
        .next()
        .expect("schema source root");
    let nac = acp::NodeACP::new(store.clone());
    nac.load().await?;
    nac.change_source_permission(
        &identity::Did::new(runtime.did())?,
        NodePermission::CollectionPatch,
        None,
        "source",
        &source,
        true,
    )
    .await?;
    drop(nac);
    drop(store);
    txn.commit().await?;
    assert!(
        node.database()
            .reconcile_document_projector()
            .await
            .is_err(),
        "actorless reconciliation must not restore a missing source"
    );
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent, NodePermission::CollectionPatch)
            .await?
    );
    node.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn reviewed_plugin_delete_source_tracks_selection_and_reinstall() -> anyhow::Result<()> {
    let (directory, runtime, node) = projection_node().await?;
    let outsider = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("outsider"),
        None,
    )?;
    let outsider_did = identity::Did::new(outsider.did())?;
    let binding = gents_store::identity::provision_agent_identity(&runtime)?;
    let agent = identity::Did::new(&binding.agent_did)?;
    let owner = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(runtime.did())?,
    );
    let digest = format!("sha256:{}", "a".repeat(64));
    let record = gents_store::local::ProjectedPlugin {
        coordinate: "team/worker".into(),
        digest: digest.clone(),
        deletes: true,
    };
    let integration = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "plugins": [{"plugin":"team/worker", "digest": digest}]
    }))?;
    let node_value = escape_graphql_string(runtime.did());
    let agent_value = escape_graphql_string(&binding.agent_did);
    let nonce = escape_graphql_string(&binding.creation_nonce);
    for mutation in [
        format!(
            r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"plugin-tools",integrations:{integration}}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"plugin-context",tools_id:"plugin-tools"}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"plugin-context",enabled:true}}) {{_docID}} }}"#
        ),
        format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{}",creation_nonce:"{nonce}",context_id:"plugin-context",enabled:true}}) {{_docID}} }}"#,
            escape_graphql_string(outsider.did())
        ),
    ] {
        owner.write("test.plugin_delete.create", &mutation).await?;
    }
    let mut observations = Vec::new();
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        &[],
        &agent,
        false,
    )
    .await?;
    owner
        .transact_write("test.plugin_delete.install", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(&[], std::slice::from_ref(record))
                    .await
            })
        })
        .await?;
    let signer = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
    let non_admin = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent.clone(),
        signer.as_ref(),
    )?;
    assert!(!node.nac_manager().is_admin(&agent).await?);
    let mut changed = std::collections::BTreeMap::new();
    let repeat = non_admin
        .transact_write("test.plugin_delete.same", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(
                    std::slice::from_ref(record),
                    std::slice::from_ref(record),
                )
                .await
            })
        })
        .await;
    changed.insert("repeat", repeat.is_ok());

    let txn = node.database().new_txn(false).await?;
    let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
        txn.datastore()?.sibling(Namespace::Acpstore),
    ));
    let nac = acp::NodeACP::new(store.clone());
    nac.load().await?;
    let index = gents_store::local::plugin_delete_source::index(runtime.did());
    let root = gents_store::local::plugin_delete_source::root(runtime.did(), "team/worker", None);
    nac.change_source_permission(
        &identity::Did::new(runtime.did())?,
        NodePermission::DocumentDelete,
        Some(&index),
        "source",
        &root,
        true,
    )
    .await?;
    drop(nac);
    drop(store);
    txn.commit().await?;
    let missing = non_admin
        .transact_write("test.plugin_delete.missing", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(
                    std::slice::from_ref(record),
                    std::slice::from_ref(record),
                )
                .await
            })
        })
        .await;
    changed.insert("missing-root", missing.is_ok());
    let repaired = owner
        .transact_write("test.plugin_delete.repair", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(
                    std::slice::from_ref(record),
                    std::slice::from_ref(record),
                )
                .await
            })
        })
        .await;
    changed.insert("repair-root", repaired.is_ok());
    repaired?;
    let txn = node.database().new_txn(false).await?;
    let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
        txn.datastore()?.sibling(Namespace::Acpstore),
    ));
    let pinned_root = gents_store::local::plugin_delete_source::root(
        runtime.did(),
        "team/worker",
        Some(&record.digest),
    );
    let children = store
        .get_relation_subjects(
            acp::NODE_POLICY_ID,
            acp::NODE_RESOURCE_NAME,
            &gents_store::local::plugin_delete_source::object(&pinned_root),
            NodePermission::DocumentDelete.as_str(),
        )
        .await?;
    let member = children
        .into_iter()
        .filter_map(|subject| match subject {
            Subject::EntitySet {
                resource,
                object_id,
                relation,
            } if resource == acp::NODE_RESOURCE_NAME
                && relation == NodePermission::DocumentDelete.as_str() =>
            {
                decode_source_for_tests(&object_id).filter(|source| {
                    source.starts_with(&gents_store::local::plugin_delete_source::member_prefix(
                        runtime.did(),
                    ))
                })
            }
            _ => None,
        })
        .next()
        .expect("selected plugin membership");
    let nac = acp::NodeACP::new(store.clone());
    nac.load().await?;
    nac.change_source_permission(
        &identity::Did::new(runtime.did())?,
        NodePermission::DocumentDelete,
        Some(&member),
        "actor",
        agent.as_str(),
        true,
    )
    .await?;
    drop(nac);
    drop(store);
    txn.commit().await?;
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?
    );
    let missing_member = non_admin
        .transact_write("test.plugin_delete.missing_member", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(
                    std::slice::from_ref(record),
                    std::slice::from_ref(record),
                )
                .await
            })
        })
        .await;
    changed.insert("missing-member", missing_member.is_ok());
    assert!(
        !node
            .nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?
    );
    let repaired = owner
        .transact_write("test.plugin_delete.repair_member", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(
                    std::slice::from_ref(record),
                    std::slice::from_ref(record),
                )
                .await
            })
        })
        .await;
    changed.insert("repair-member", repaired.is_ok());
    repaired?;
    assert!(
        node.nac_manager()
            .check_permission(&agent, NodePermission::DocumentDelete)
            .await?
    );
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        std::slice::from_ref(&record),
        &agent,
        false,
    )
    .await?;
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        std::slice::from_ref(&record),
        &outsider_did,
        false,
    )
    .await?;
    let mut replacement = record.clone();
    replacement.digest = format!("sha256:{}", "b".repeat(64));
    owner
        .transact_write("test.plugin_delete.upgrade", |txn| {
            let (old, new) = (&record, &replacement);
            Box::pin(async move {
                txn.stage_plugin_delete_roots(std::slice::from_ref(old), std::slice::from_ref(new))
                    .await
            })
        })
        .await?;
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        std::slice::from_ref(&replacement),
        &agent,
        false,
    )
    .await?;
    let unpinned = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
        "plugins": [{"plugin":"team/worker"}]
    }))?;
    owner.write("test.plugin_delete.unpinned", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"plugin-tools"}}}},input:{{integrations:{unpinned}}}) {{_docID}} }}"#)).await?;
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        std::slice::from_ref(&replacement),
        &agent,
        false,
    )
    .await?;
    owner.write("test.plugin_delete.restore_pin", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"plugin-tools"}}}},input:{{integrations:{integration}}}) {{_docID}} }}"#)).await?;
    owner
        .transact_write("test.plugin_delete.downgrade", |txn| {
            let (old, new) = (&replacement, &record);
            Box::pin(async move {
                txn.stage_plugin_delete_roots(std::slice::from_ref(old), std::slice::from_ref(new))
                    .await
            })
        })
        .await?;
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        std::slice::from_ref(&record),
        &agent,
        false,
    )
    .await?;
    owner
        .transact_write("test.plugin_delete.uninstall", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(std::slice::from_ref(record), &[])
                    .await
            })
        })
        .await?;
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        &[],
        &agent,
        false,
    )
    .await?;
    owner.write("test.plugin_delete.disable", &format!(r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"plugin-tools"}}}},input:{{integrations:null}}) {{_docID}} }}"#)).await?;
    owner
        .transact_write("test.plugin_delete.reinstall", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(&[], std::slice::from_ref(record))
                    .await
            })
        })
        .await?;
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        std::slice::from_ref(&record),
        &agent,
        false,
    )
    .await?;
    owner
        .grant_node_permission(&agent, NodePermission::DocumentDelete)
        .await?;
    owner
        .transact_write("test.plugin_delete.remove_again", |txn| {
            let record = &record;
            Box::pin(async move {
                txn.stage_plugin_delete_roots(std::slice::from_ref(record), &[])
                    .await
            })
        })
        .await?;
    observe_plugin_permission(
        &mut observations,
        &owner,
        &runtime,
        &node,
        &[],
        &agent,
        true,
    )
    .await?;
    let inputs = observations
        .iter()
        .map(|(input, _)| input.clone())
        .collect::<Vec<_>>();
    let serialized = serde_json::to_string(&inputs)?;
    let output = tokio::task::spawn_blocking(move || {
        gents_lean_contract::run_prepared_generator_with_input(
            &gents_lean_contract::proofs_dir()?,
            "Proofs.Conformance.PluginDelete:olean",
            "Proofs/Conformance/PluginDelete.lean",
            &serialized,
        )
    })
    .await??;
    let modeled: Vec<bool> = serde_json::from_str(&output)?;
    assert_eq!(modeled.len(), observations.len());
    for (expected, (input, actual)) in modeled.into_iter().zip(observations) {
        assert_eq!(actual, expected, "observed fixture: {input}");
    }
    for case in gents_lean_vocab::families::household_tool_source_change_plugin_delete::cases() {
        let observed = changed[case.input.step.as_str()];
        case.check(observed);
    }
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn plugin_delete_sources_follow_installed_roots_and_selections() -> anyhow::Result<()> {
    let digest = |name: &str| match name {
        "digest-a" => Ok(format!("sha256:{}", "a".repeat(64))),
        "digest-b" => Ok(format!("sha256:{}", "b".repeat(64))),
        other => Err(anyhow::anyhow!("unmodeled plugin digest {other}")),
    };
    for case in gents_lean_vocab::families::household_plugin_delete::cases() {
        let (directory, runtime, node) = projection_node().await?;
        let outsider = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("outsider"),
            None,
        )?;
        let binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let owner = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime.did())?,
        );
        if case.input.installed {
            let record = gents_store::local::ProjectedPlugin {
                coordinate: "team/worker".into(),
                digest: digest(&case.input.root_digest)?,
                deletes: true,
            };
            owner
                .transact_write("test.plugin_delete_sources.install", |txn| {
                    let record = &record;
                    Box::pin(async move {
                        txn.stage_plugin_delete_roots(&[], std::slice::from_ref(record))
                            .await
                    })
                })
                .await?;
        }
        let plugin = match &case.input.pin {
            Some(pin) => serde_json::json!({"plugin": "team/worker", "digest": digest(pin)?}),
            None => serde_json::json!({"plugin": "team/worker"}),
        };
        let integration = gents_protocol::graphql::graphql_input_literal(
            &serde_json::json!({ "plugins": [plugin] }),
        )?;
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(if case.input.managed {
            &binding.agent_did
        } else {
            outsider.did()
        });
        let nonce = escape_graphql_string(&binding.creation_nonce);
        let enabled = case.input.selected;
        for mutation in [
            format!(
                r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"plugin-tools",integrations:{integration}}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"plugin-context",tools_id:"plugin-tools"}}) {{_docID}} }}"#
            ),
            format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"plugin-context",enabled:{enabled}}}) {{_docID}} }}"#
            ),
        ] {
            owner
                .write("test.plugin_delete_sources.create", &mutation)
                .await?;
        }
        let txn = node.database().new_txn(false).await?;
        let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
            txn.datastore()?.sibling(Namespace::Acpstore),
        ));
        let relation = NodePermission::DocumentDelete.as_str();
        let index = gents_store::local::plugin_delete_source::object(
            &gents_store::local::plugin_delete_source::index(runtime.did()),
        );
        let active = store
            .get_relation_subjects(
                acp::NODE_POLICY_ID,
                acp::NODE_RESOURCE_NAME,
                acp::NODE_OBJECT_ID,
                relation,
            )
            .await?;
        let member_prefix = gents_store::local::plugin_delete_source::member_prefix(runtime.did());
        let mut sources = 0u64;
        for root in store
            .get_relation_subjects(
                acp::NODE_POLICY_ID,
                acp::NODE_RESOURCE_NAME,
                &index,
                relation,
            )
            .await?
        {
            let Subject::EntitySet { object_id, .. } = &root else {
                continue;
            };
            if !active.contains(&root) {
                continue;
            }
            for member in store
                .get_relation_subjects(
                    acp::NODE_POLICY_ID,
                    acp::NODE_RESOURCE_NAME,
                    object_id,
                    relation,
                )
                .await?
            {
                let Subject::EntitySet {
                    object_id: member_object,
                    ..
                } = member
                else {
                    continue;
                };
                if !decode_source_for_tests(&member_object)
                    .is_some_and(|source| source.starts_with(&member_prefix))
                {
                    continue;
                }
                let actors = store
                    .get_relation_subjects(
                        acp::NODE_POLICY_ID,
                        acp::NODE_RESOURCE_NAME,
                        &member_object,
                        relation,
                    )
                    .await?;
                if actors
                    .iter()
                    .any(|actor| matches!(actor, Subject::Entity(_)))
                {
                    sources += 1;
                }
            }
        }
        drop(store);
        txn.commit().await?;
        node.shutdown().await?;
        case.check(sources);
    }
    Ok(())
}

#[tokio::test]
async fn session_tool_reads_follow_the_node_grant_and_session_acp() -> anyhow::Result<()> {
    for case in gents_lean_vocab::families::household_session_tool_access::cases() {
        let (_directory, runtime, node) = projection_node().await?;
        node.add_collection(&format!(
            "type ToolSessionProbe @policy(id: \"{}\", resource: \"sessions\") {{ name: String }}",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID,
        ))
        .await?;
        let owner = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime.did())?,
        );
        let node_value = escape_graphql_string(runtime.did());
        let mut readers = Vec::new();
        for (name, tools) in [("agent", case.input.granted), ("sibling", false)] {
            let binding = gents_store::identity::provision_agent_identity(&runtime)?;
            let signer = gents_store::identity::resolve_agent_identity(&runtime, &binding)?;
            let agent_value = escape_graphql_string(&binding.agent_did);
            let nonce = escape_graphql_string(&binding.creation_nonce);
            for mutation in [
                format!(
                    r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"{name}-tools",built_ins:{{enable_session_history_tool:{tools}}}}}) {{_docID}} }}"#
                ),
                format!(
                    r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"{name}-context",tools_id:"{name}-tools"}}) {{_docID}} }}"#
                ),
                format!(
                    r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"{name}-context",enabled:true}}) {{_docID}} }}"#
                ),
            ] {
                owner.write("test.session_tool.setup", &mutation).await?;
            }
            let did = identity::Did::new(&binding.agent_did)?;
            let access = gents_store::config_client::ConfigAccess::local_managed_agent(
                node.clone(),
                did.clone(),
                signer.as_ref(),
            )?;
            readers.push((did, access, agent_value));
        }
        let create = |name: &str| {
            format!(
                r#"mutation {{ create_ToolSessionProbe(input: {{name:"{name}"}}) {{_docID}} }}"#
            )
        };
        let (agent, agent_access, agent_value) = &readers[0];
        let owned = gents_store::graphql::created_doc_id(
            &agent_access
                .write("test.session_tool.owned", &create("owned"))
                .await?,
            "ToolSessionProbe",
        )?;
        let shared = gents_store::graphql::created_doc_id(
            &owner
                .write("test.session_tool.shared", &create("shared"))
                .await?,
            "ToolSessionProbe",
        )?;
        owner
            .add_actor_relationship("ToolSessionProbe", &shared, "reader", agent)
            .await?;
        let foreign = gents_store::graphql::created_doc_id(
            &readers[1]
                .1
                .write("test.session_tool.foreign", &create("foreign"))
                .await?,
            "ToolSessionProbe",
        )?;
        if case.input.admin {
            owner.grant_node_admin(agent).await?;
        }
        if !case.input.granted {
            owner.write("test.session_tool.withdraw", &format!(
                r#"mutation {{ update_Agent(filter: {{node_did:{{_eq:"{node_value}"}},agent_did:{{_eq:"{agent_value}"}}}},input:{{enabled:false}}) {{_docID}} }}"#
            )).await?;
        }
        let doc = match case.input.session.as_str() {
            "owned" => &owned,
            "shared" => &shared,
            "foreign" => &foreign,
            other => anyhow::bail!("unmodeled session {other}"),
        };
        anyhow::ensure!(
            !case.input.native_bypass,
            "a managed Agent's signed read never resolves DAC bypass: {:?}",
            case.input
        );
        let read = agent_access
            .execute(&format!(
                r#"{{ ToolSessionProbe(filter: {{_docID: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                escape_graphql_string(doc)
            ))
            .await;
        let visible = read.is_ok_and(|response| {
            response["data"]["ToolSessionProbe"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["_docID"] == doc.as_str()))
        });
        node.shutdown().await?;
        case.check(visible);
    }
    Ok(())
}

fn schema_permission(name: &str) -> anyhow::Result<NodePermission> {
    [
        NodePermission::CollectionGet,
        NodePermission::CollectionPatch,
        NodePermission::CollectionTruncate,
        NodePermission::MigrationSet,
        NodePermission::ViewAdd,
    ]
    .into_iter()
    .find(|permission| permission.as_str() == name)
    .ok_or_else(|| anyhow::anyhow!("unmodeled schema permission {name}"))
}

async fn source_grants(
    node: &defra_node::Node,
    source: &str,
    permission: NodePermission,
    actor: &identity::Did,
) -> anyhow::Result<bool> {
    let txn = node.database().new_txn(false).await?;
    let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
        txn.datastore()?.sibling(Namespace::Acpstore),
    ));
    let object = gents_store::local::plugin_delete_source::object(source);
    let roots = store
        .get_relation_subjects(
            acp::NODE_POLICY_ID,
            acp::NODE_RESOURCE_NAME,
            acp::NODE_OBJECT_ID,
            permission.as_str(),
        )
        .await?;
    let members = store
        .get_relation_subjects(
            acp::NODE_POLICY_ID,
            acp::NODE_RESOURCE_NAME,
            &object,
            permission.as_str(),
        )
        .await?;
    let granted = roots.contains(&Subject::entity_set(
        acp::NODE_RESOURCE_NAME,
        object.clone(),
        permission.as_str(),
    )) && members.contains(&Subject::Entity(actor.clone()));
    drop(store);
    txn.commit().await?;
    Ok(granted)
}

async fn unauthorized_tools_edit(
    owner: &gents_store::config_client::ConfigAccess,
    node: &Arc<defra_node::Node>,
    runtime: &gents_store::identity::KeyIdentity,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    tools_doc: &str,
    input: serde_json::Value,
) -> anyhow::Result<bool> {
    let agent = identity::Did::new(&binding.agent_did)?;
    let signer = gents_store::identity::resolve_agent_identity(runtime, binding)?;
    owner
        .add_actor_relationship("Tools", tools_doc, "writer", &agent)
        .await?;
    anyhow::ensure!(!node.nac_manager().is_admin(&agent).await?);
    let writer = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        agent,
        signer.as_ref(),
    )?;
    let mutation = format!(
        r#"mutation($input: ToolsMutationInputArg!) {{ update_Tools(docID:"{}", input:$input) {{_docID}} }}"#,
        escape_graphql_string(tools_doc)
    );
    let variables = serde_json::json!({ "input": input });
    Ok(writer
        .transact_write("test.tool_grant.unauthorized", |txn| {
            let (mutation, variables) = (&mutation, &variables);
            Box::pin(async move {
                txn.execute_client_signed_update(
                    mutation,
                    variables,
                    gents_protocol::schemas::TOOLS,
                )
                .await
            })
        })
        .await
        .is_ok())
}

async fn rolled_back_write(
    owner: &gents_store::config_client::ConfigAccess,
    mutation: &str,
) -> anyhow::Result<()> {
    let (rolled_back, _) =
        gents_store::config_client::ConfigApplyTxn::with_successful_mutation_failure_at(
            Some(1),
            owner.transact_write("test.tool_grant.rolled_back", |txn| {
                Box::pin(async move { txn.execute(mutation).await.map_err(anyhow::Error::from) })
            }),
        )
        .await;
    anyhow::ensure!(rolled_back.is_err(), "the failed commit must not publish");
    Ok(())
}

async fn schema_tool_enabled(
    owner: &gents_store::config_client::ConfigAccess,
    tools_doc: &str,
) -> anyhow::Result<bool> {
    let response = owner
        .execute(&format!(
            r#"{{ Tools(filter: {{_docID: {{_eq: "{}"}}}}) {{ built_ins }} }}"#,
            escape_graphql_string(tools_doc)
        ))
        .await?;
    Ok(response["data"]["Tools"][0]["built_ins"]["enable_schema_tool"] == true)
}

#[tokio::test]
async fn schema_tool_source_changes_follow_the_modeled_projection() -> anyhow::Result<()> {
    for case in gents_lean_vocab::families::household_tool_grant_projection::cases() {
        let input = &case.input;
        let (_directory, runtime, node) = projection_node().await?;
        let owner = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime.did())?,
        );
        let binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let agent = identity::Did::new(&binding.agent_did)?;
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(&binding.agent_did);
        let nonce = escape_graphql_string(&binding.creation_nonce);
        let physical = !input.source.tools_doc.is_empty();
        let tools_id = if physical { "tools" } else { "missing-tools" };
        let selected = !input.initial.enabled.is_empty();
        let tools_doc = if physical {
            Some(gents_store::graphql::created_doc_id(
                &owner.write("test.tool_grant.tools", &format!(
                    r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"tools",built_ins:{{enable_schema_tool:{selected}}}}}) {{_docID}} }}"#
                )).await?,
                "Tools",
            )?)
        } else {
            None
        };
        let context_doc = gents_store::graphql::created_doc_id(
            &owner.write("test.tool_grant.context", &format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"context",tools_id:"{tools_id}"}}) {{_docID}} }}"#
            )).await?,
            "AgentContext",
        )?;
        owner.write("test.tool_grant.agent", &format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"context",enabled:true}}) {{_docID}} }}"#
        )).await?;
        for grant in &input.initial.direct {
            anyhow::ensure!(
                grant.actor == input.source.agent,
                "unmodeled direct grantee"
            );
            owner
                .grant_node_permission(&agent, schema_permission(&grant.permission)?)
                .await?;
        }
        if input.initial.admins.contains(&input.source.agent) {
            owner.grant_node_admin(&agent).await?;
        }
        let enable = input.enable;
        let update = format!(
            r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"{tools_id}"}}}},input:{{built_ins:{{enable_schema_tool:{enable}}}}}) {{_docID}} }}"#
        );
        if !input.authorized {
            let doc = tools_doc
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("an unauthorized edit needs the Tools document"))?;
            unauthorized_tools_edit(
                &owner,
                &node,
                &runtime,
                &binding,
                doc,
                serde_json::json!({"built_ins": {"enable_schema_tool": enable}}),
            )
            .await?;
        } else if input.committed {
            owner.write("test.tool_grant.change", &update).await?;
        } else {
            rolled_back_write(&owner, &update).await?;
        }
        let permission = schema_permission(&input.source.permission)?;
        let (enabled, grants) = match &tools_doc {
            Some(doc) => (
                schema_tool_enabled(&owner, doc).await?,
                source_grants(
                    &node,
                    &gents_store::local::tool_permission_projection::tool_source(
                        runtime.did(),
                        doc,
                        &context_doc,
                        agent.as_str(),
                        &input.source.purpose,
                    ),
                    permission,
                    &agent,
                )
                .await?,
            ),
            None => (false, false),
        };
        let observed = |present: bool| {
            if present {
                vec![input.source.clone()]
            } else {
                Vec::new()
            }
        };
        let effective = node
            .nac_manager()
            .check_permission(&agent, permission)
            .await?;
        node.shutdown().await?;
        case.check(
            gents_lean_vocab::families::household_tool_grant_projection::ProjectionOutput {
                enabled: observed(enabled),
                grants: observed(grants),
                effective,
            },
        );
    }
    Ok(())
}

#[tokio::test]
async fn schema_tool_group_changes_commit_every_permission_together() -> anyhow::Result<()> {
    for case in gents_lean_vocab::families::household_tool_grant_group::cases() {
        let input = &case.input;
        let (_directory, runtime, node) = projection_node().await?;
        let owner = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime.did())?,
        );
        let binding = gents_store::identity::provision_agent_identity(&runtime)?;
        let agent = identity::Did::new(&binding.agent_did)?;
        let node_value = escape_graphql_string(runtime.did());
        let agent_value = escape_graphql_string(&binding.agent_did);
        let nonce = escape_graphql_string(&binding.creation_nonce);
        let selected = !input.initial.enabled.is_empty();
        let tools_doc = gents_store::graphql::created_doc_id(
            &owner.write("test.tool_group.tools", &format!(
                r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"tools",built_ins:{{enable_schema_tool:{selected}}}}}) {{_docID}} }}"#
            )).await?,
            "Tools",
        )?;
        let context_doc = gents_store::graphql::created_doc_id(
            &owner.write("test.tool_group.context", &format!(
                r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"context",tools_id:"tools"}}) {{_docID}} }}"#
            )).await?,
            "AgentContext",
        )?;
        owner.write("test.tool_group.agent", &format!(
            r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{agent_value}",creation_nonce:"{nonce}",context_id:"context",enabled:true}}) {{_docID}} }}"#
        )).await?;
        let enable = input.enable;
        let update = format!(
            r#"mutation {{ update_Tools(filter: {{node_did:{{_eq:"{node_value}"}},tools_id:{{_eq:"tools"}}}},input:{{built_ins:{{enable_schema_tool:{enable}}}}}) {{_docID}} }}"#
        );
        if !input.authorized {
            unauthorized_tools_edit(
                &owner,
                &node,
                &runtime,
                &binding,
                &tools_doc,
                serde_json::json!({"built_ins": {"enable_schema_tool": enable}}),
            )
            .await?;
        } else if input.committed {
            owner.write("test.tool_group.change", &update).await?;
        } else {
            rolled_back_write(&owner, &update).await?;
        }
        let tool_enabled = schema_tool_enabled(&owner, &tools_doc).await?;
        let mut enabled = Vec::new();
        let mut grants = Vec::new();
        for source in &input.sources {
            if tool_enabled {
                enabled.push(source.clone());
            }
            let projected = source_grants(
                &node,
                &gents_store::local::tool_permission_projection::tool_source(
                    runtime.did(),
                    &tools_doc,
                    &context_doc,
                    agent.as_str(),
                    &source.purpose,
                ),
                schema_permission(&source.permission)?,
                &agent,
            )
            .await?;
            if projected {
                grants.push(source.clone());
            }
        }
        let mut permissions = Vec::new();
        for source in &input.sources {
            permissions.push(
                gents_lean_vocab::families::household_tool_grant_group::PermissionAllowed {
                    permission: source.permission.clone(),
                    allowed: node
                        .nac_manager()
                        .check_permission(&agent, schema_permission(&source.permission)?)
                        .await?,
                },
            );
        }
        node.shutdown().await?;
        case.check(
            gents_lean_vocab::families::household_tool_grant_group::GroupOutput {
                enabled,
                grants,
                permissions,
            },
        );
    }
    Ok(())
}

#[tokio::test]
async fn shared_tool_hierarchy_changes_follow_the_modeled_edges() -> anyhow::Result<()> {
    use gents_store::local::ToolSourcePolicy;
    for case in gents_lean_vocab::families::household_tool_grant_hierarchy::cases() {
        let input = &case.input;
        let (_directory, runtime, node) = projection_node().await?;
        let owner = gents_store::config_client::ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(runtime.did())?,
        );
        let node_value = escape_graphql_string(runtime.did());
        let mut bindings = std::collections::BTreeMap::new();
        for modeled in [&input.agent, &input.sibling] {
            bindings.insert(
                modeled.clone(),
                gents_store::identity::provision_agent_identity(&runtime)?,
            );
        }
        let modeled_did = |did: &str| {
            bindings
                .iter()
                .find(|(_, binding)| binding.agent_did == did)
                .map(|(modeled, _)| modeled.clone())
        };
        let mut tools_docs = std::collections::BTreeMap::new();
        for group in [&input.group, &input.next_group] {
            let enabled = input
                .initial
                .permissions
                .iter()
                .any(|edge| edge.group.tools_doc == group.tools_doc);
            let tools_id = escape_graphql_string(&group.tools_doc);
            tools_docs.insert(
                group.tools_doc.clone(),
                gents_store::graphql::created_doc_id(
                    &owner.write("test.tool_hierarchy.tools", &format!(
                        r#"mutation {{ create_Tools(input: {{node_did:"{node_value}",tools_id:"{tools_id}",built_ins:{{enable_schema_tool:{enabled}}}}}) {{_docID}} }}"#
                    )).await?,
                    "Tools",
                )?,
            );
        }
        let mut context_docs = std::collections::BTreeMap::new();
        for context in [&input.context_doc, &input.next_context_doc] {
            let context_id = escape_graphql_string(context);
            let selection = input
                .initial
                .contexts
                .iter()
                .find(|edge| &edge.context_doc == context)
                .map(|edge| {
                    format!(
                        ",tools_id:\"{}\"",
                        escape_graphql_string(&edge.group.tools_doc)
                    )
                })
                .unwrap_or_default();
            context_docs.insert(
                context.clone(),
                gents_store::graphql::created_doc_id(
                    &owner.write("test.tool_hierarchy.context", &format!(
                        r#"mutation {{ create_AgentContext(input: {{node_did:"{node_value}",context_id:"{context_id}"{selection}}}) {{_docID}} }}"#
                    )).await?,
                    "AgentContext",
                )?,
            );
        }
        for edge in &input.initial.agents {
            let binding = &bindings[&edge.agent];
            owner.write("test.tool_hierarchy.agent", &format!(
                r#"mutation {{ create_Agent(input: {{node_did:"{node_value}",agent_did:"{}",creation_nonce:"{}",context_id:"{}",enabled:true}}) {{_docID}} }}"#,
                escape_graphql_string(&binding.agent_did),
                escape_graphql_string(&binding.creation_nonce),
                escape_graphql_string(&edge.context_doc),
            )).await?;
        }
        for grant in &input.initial.direct {
            owner
                .grant_node_permission(
                    &identity::Did::new(&bindings[&grant.actor].agent_did)?,
                    schema_permission(&grant.permission)?,
                )
                .await?;
        }
        let agent_binding = &bindings[&input.agent];
        let agent_rows = owner
            .execute(&format!(
                r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
                escape_graphql_string(&agent_binding.agent_did)
            ))
            .await?;
        let agent_doc = agent_rows["data"]["Agent"][0]["_docID"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("the modeled Agent row is missing: {agent_rows}"))?
            .to_owned();
        let (collection, doc, change) = match input.action.as_str() {
            "enable" | "disable" => (
                "Tools",
                &tools_docs[&input.group.tools_doc],
                serde_json::json!({"built_ins": {"enable_schema_tool": input.action == "enable"}}),
            ),
            "context" => (
                "AgentContext",
                &context_docs[&input.context_doc],
                serde_json::json!({"tools_id": input.next_group.tools_doc}),
            ),
            "agent" => (
                "Agent",
                &agent_doc,
                serde_json::json!({"context_id": input.next_context_doc}),
            ),
            other => anyhow::bail!("unmodeled hierarchy action {other}"),
        };
        let update = format!(
            r#"mutation {{ update_{collection}(docID:"{}", input:{}) {{_docID}} }}"#,
            escape_graphql_string(doc),
            gents_protocol::graphql::graphql_input_literal(&change)?
        );
        if !input.authorized {
            if collection == "Tools" {
                unauthorized_tools_edit(&owner, &node, &runtime, agent_binding, doc, change)
                    .await?;
            } else {
                let sibling = &bindings[&input.sibling];
                let signer = gents_store::identity::resolve_agent_identity(&runtime, sibling)?;
                let actor = gents_store::config_client::ConfigAccess::local_managed_agent(
                    node.clone(),
                    identity::Did::new(&sibling.agent_did)?,
                    signer.as_ref(),
                )?;
                let _ = actor
                    .write("test.tool_hierarchy.unauthorized", &update)
                    .await;
            }
        } else if input.committed {
            owner.write("test.tool_hierarchy.change", &update).await?;
        } else {
            rolled_back_write(&owner, &update).await?;
        }
        let snapshot = owner
            .execute(&format!(
                r#"{{ Tools(filter: {{node_did: {{_eq: "{node_value}"}}}}) {{ _docID node_did tools_id built_ins }} AgentContext(filter: {{node_did: {{_eq: "{node_value}"}}}}) {{ _docID context_id tools_id }} Agent(filter: {{node_did: {{_eq: "{node_value}"}}}}) {{ agent_did context_id }} }}"#
            ))
            .await?;
        let rows = |kind: &str| {
            snapshot["data"][kind]
                .as_array()
                .cloned()
                .unwrap_or_default()
        };
        let policy = crate::surface::ToolCeiling::meta_only();
        let mut permissions = Vec::new();
        for tools in rows("Tools") {
            let tools_id = tools["tools_id"].as_str().unwrap_or_default().to_owned();
            let mut value = tools.clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("_docID");
            }
            for source in policy.node_operation_sources(&value, &[])? {
                permissions.push(
                    gents_lean_vocab::families::household_tool_grant_hierarchy::PermissionEdgeWire {
                        group: gents_lean_vocab::families::household_tool_grant_hierarchy::GroupWire {
                            tools_doc: tools_id.clone(),
                            purpose: source.purpose.to_owned(),
                        },
                        permission: source.permission.as_str().to_owned(),
                    },
                );
            }
        }
        permissions.retain(|edge| edge.group.purpose == input.group.purpose);
        permissions
            .sort_by_key(|e| format!("{}|{}|{}", e.group.tools_doc, e.group.purpose, e.permission));
        let mut contexts = Vec::new();
        for context in rows("AgentContext") {
            if let Some(tools_id) = context["tools_id"].as_str().filter(|id| !id.is_empty()) {
                contexts.push(
                    gents_lean_vocab::families::household_tool_grant_hierarchy::ContextEdgeWire {
                        group:
                            gents_lean_vocab::families::household_tool_grant_hierarchy::GroupWire {
                                tools_doc: tools_id.to_owned(),
                                purpose: input.group.purpose.clone(),
                            },
                        context_doc: context["context_id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    },
                );
            }
        }
        contexts.sort_by_key(|e| {
            format!(
                "{}|{}|{}",
                e.group.tools_doc, e.group.purpose, e.context_doc
            )
        });
        let mut agents = Vec::new();
        for agent in rows("Agent") {
            let modeled = modeled_did(agent["agent_did"].as_str().unwrap_or_default())
                .ok_or_else(|| anyhow::anyhow!("unmodeled Agent row {agent}"))?;
            agents.push(
                gents_lean_vocab::families::household_tool_grant_hierarchy::AgentEdgeWire {
                    context_doc: agent["context_id"].as_str().unwrap_or_default().to_owned(),
                    agent: modeled,
                },
            );
        }
        agents.sort_by_key(|e| format!("{}|{}", e.context_doc, e.agent));
        let mut direct = Vec::new();
        let mut admins = Vec::new();
        let mut owners = Vec::new();
        let mut grants = Vec::new();
        let mut allowed = std::collections::BTreeMap::new();
        for (modeled, binding) in &bindings {
            let did = identity::Did::new(&binding.agent_did)?;
            let txn = node.database().new_txn(false).await?;
            let store = Arc::new(acp::zanzibar::store::TransactionZanzibarStore::new(
                txn.datastore()?.sibling(Namespace::Acpstore),
            ));
            for name in [
                "get-collection",
                "patch-collection",
                "truncate-collection",
                "set-migration",
                "add-view",
            ] {
                let subjects = store
                    .get_relation_subjects(
                        acp::NODE_POLICY_ID,
                        acp::NODE_RESOURCE_NAME,
                        acp::NODE_OBJECT_ID,
                        name,
                    )
                    .await?;
                if subjects.contains(&Subject::Entity(did.clone())) {
                    direct.push(
                        gents_lean_vocab::families::household_tool_grant_hierarchy::DirectWire {
                            actor: modeled.clone(),
                            permission: name.to_owned(),
                        },
                    );
                }
            }
            drop(store);
            txn.commit().await?;
            if node.nac_manager().is_admin(&did).await? {
                admins.push(modeled.clone());
            }
            if node.nac_manager().is_owner(&did).await {
                owners.push(modeled.clone());
            }
            for (tools_name, tools_doc) in &tools_docs {
                for context_doc in context_docs.values() {
                    for name in [
                        "get-collection",
                        "patch-collection",
                        "truncate-collection",
                        "set-migration",
                        "add-view",
                    ] {
                        let source = gents_store::local::tool_permission_projection::tool_source(
                            runtime.did(),
                            tools_doc,
                            context_doc,
                            did.as_str(),
                            &input.group.purpose,
                        );
                        if source_grants(&node, &source, schema_permission(name)?, &did).await? {
                            grants.push(
                                gents_lean_vocab::families::household_tool_grant_hierarchy::SourceWire {
                                    tools_doc: tools_name.clone(),
                                    agent: modeled.clone(),
                                    purpose: input.group.purpose.clone(),
                                    permission: name.to_owned(),
                                },
                            );
                        }
                    }
                }
            }
            allowed.insert(
                modeled.clone(),
                node.nac_manager()
                    .check_permission(&did, NodePermission::CollectionPatch)
                    .await?,
            );
        }
        direct.sort_by_key(|e| format!("{}|{}", e.actor, e.permission));
        admins.sort();
        owners.sort();
        grants
            .sort_by_key(|e| format!("{}|{}|{}|{}", e.tools_doc, e.agent, e.purpose, e.permission));
        node.shutdown().await?;
        case.check(
            gents_lean_vocab::families::household_tool_grant_hierarchy::HierarchyOutput {
                hierarchy:
                    gents_lean_vocab::families::household_tool_grant_hierarchy::HierarchyWire {
                        permissions,
                        contexts,
                        agents,
                        direct,
                        owners,
                        admins,
                    },
                grants,
                agent_allowed: allowed[&input.agent],
                sibling_allowed: allowed[&input.sibling],
            },
        );
    }
    Ok(())
}
