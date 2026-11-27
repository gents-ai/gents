use anyhow::Context as _;
use gents_documents::apply_desired_state_plan;
use gents_documents::{
    read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use gents_store::config_client::authored_document::*;
use gents_store::config_client::{ConfigAccess, ConfigClientError, NodePermission};
use gents_store::identity::grant_sets::test_support::*;
use gents_store::identity::grant_sets::*;
use gents_store::identity::NodeIdentity;
use gents_store::node_scope::ContextCalls as _;
use serde_json::json;

#[tokio::test]
async fn authored_application_document_is_signed_by_the_held_user() -> anyhow::Result<()> {
    const SDL: &str = "type AuthoredDocumentFixture { name: String }";
    let signed_home = gents_documents::test_support::SignedHome::start(
        "authored-document",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    crate::test_support::default_agent(&signed_home).await;
    let (db, user, access) = signed_home.into_parts();
    // Schema installation is node administration; the held User authors documents only.
    let admin = ConfigAccess::local_as(
        db.node.clone(),
        identity::Did::new(db.node_identity.did()).unwrap(),
    );
    let plan = gents_store::config_client::preview_schema_install(&admin, SDL).await?;
    gents_store::config_client::apply_schema_install(&admin, SDL, &plan.artifact_digest).await?;
    let doc = create_authored_document(
        &access,
        "AuthoredDocumentFixture",
        &json!({"name": "created"}),
        DocumentSchemaSource::UserAuthored,
        SDL,
    )
    .await?;
    let escaped = gents_store::graphql::escape_graphql_string(&doc);
    let response = access.execute(&format!(
        r#"{{ _commits(docID: "{escaped}", filter: {{fieldName: {{_eq: "_C"}}}}, order: {{height: ASC}}, limit: 1) {{cid}} }}"#,
    )).await?;
    let cid = response["data"]["_commits"][0]["cid"]
        .as_str()
        .expect("genesis CID");
    let (block, signature) = db
        .node
        .authorized_signed_block_bytes_by(cid, user.did())
        .await?;
    let block = defra_core::block::Block::from_dag_cbor(&block)?;
    let signature = defra_core::block::Signature::from_dag_cbor(&signature)?;
    let signer = db::block::verify::verified_signature_signer_did(&block, &signature)
        .map_err(anyhow::Error::msg)?;
    assert_eq!(signer, user.did());
    assert_ne!(user.did(), db.node_identity.did());
    let response = access
        .execute("{ AuthoredDocumentFixture { name } }")
        .await?;
    assert_eq!(
        response["data"]["AuthoredDocumentFixture"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    Ok(())
}

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
                apply_desired_state_plan(txn, &plan, &crate::resolved::ProviderProfilePolicy).await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn agent_transaction_uses_the_node_cap_only_while_the_runtime_may_use_its_node(
) -> anyhow::Result<()> {
    let signed_home = gents_documents::test_support::SignedHome::start(
        "runtime-hop-cap",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let binding = crate::test_support::default_agent(&signed_home).await;
    let (db, user, user_access) = signed_home.into_parts();
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
    let usable = {
        let (runtime, doc) = (identity::Did::new(runtime.did())?, node_doc.to_owned());
        admin
            .transact_readonly("test.runtime_use.hop_cap_node", |txn| {
                let (runtime, doc) = (&runtime, &doc);
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        txn.runtime_may_use(runtime, "Node", std::slice::from_ref(doc))
                            .await?,
                    )
                })
            })
            .await?
    };
    assert_eq!(usable, vec![false]);
    assert_eq!(
        admin.execute("{ Node { _docID } }").await?["data"]["Node"]
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    assert_eq!(single_doc_id(&user_access, "Node", "{}").await?, node_doc);
    assert_eq!(observe(runtime.did().to_owned()).await?, None);
    admin
        .grant_node_permission(
            &identity::Did::new(user.did())?,
            NodePermission::DocumentDelete,
        )
        .await?;
    user_access
        .transact_write("test.runtime_hop_cap.remove_node", |txn| {
            Box::pin(async move { txn.delete_configuration_document("Node", node_doc).await })
        })
        .await?;
    assert_eq!(observe(runtime.did().to_owned()).await?, None);
    Ok(())
}

async fn single_doc_id(
    access: &ConfigAccess,
    collection: &str,
    filter: &str,
) -> anyhow::Result<String> {
    let response = access
        .execute(&format!(
            "{{ {collection}(filter: {filter}) {{ _docID }} }}"
        ))
        .await?;
    match response["data"][collection].as_array().map(Vec::as_slice) {
        Some([row]) => Ok(row["_docID"]
            .as_str()
            .context("row has no _docID")?
            .to_owned()),
        rows => anyhow::bail!("expected one {collection}, found {rows:?}"),
    }
}

/// Each runtime use after revoking the Node's `runtime_reader`, the
/// credential's `runtime_writer`, both or neither, on a separately owned store.
#[tokio::test]
async fn runtime_uses_follow_document_acp_as_the_lean_model_says() -> anyhow::Result<()> {
    use gents_lean_vocab::families::runtime_use_revocation::RevocationObserved;

    let signed_home = gents_documents::test_support::SignedHome::start(
        "runtime-use",
        gents_store::node_extension::Extensions::none(),
    )
    .await;
    let binding = crate::test_support::default_agent(&signed_home).await;
    let (db, user, user_access) = signed_home.into_parts();
    let runtime = db.node_identity.as_ref();
    let did = runtime.did();
    let runtime_did = identity::Did::new(did)?;
    let user_did = identity::Did::new(user.did())?;
    let signer = gents_store::identity::resolve_agent_identity(runtime, &binding)?;
    let agent = ConfigAccess::local_managed_agent(
        db.node.clone(),
        identity::Did::new(&binding.agent_did)?,
        signer.as_ref(),
    )?;
    let admin = ConfigAccess::local_as(db.node.clone(), runtime_did.clone());
    admin
        .grant_node_permission(&user_did, NodePermission::DacRelationDelete)
        .await?;
    set_node_hop_cap(&user_access, did, 1).await?;

    let provider = "runtime-use-test";
    let credential = crate::auth::credential::OAuthCredential {
        doc_id: None,
        credential_id: "runtime-use".into(),
        node_did: did.into(),
        provider: provider.into(),
        access_token: "private-access-token".into(),
        refresh_token: "private-refresh-token".into(),
        id_token: None,
        account_id: None,
        chatgpt_plan_type: None,
        is_fedramp: false,
        access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        last_refresh: None,
        enabled: true,
        account_ref: None,
        connected_at: None,
        provider_account_key: None,
        label: None,
    };
    let credential_doc =
        crate::auth::credential::upsert_oauth_credential_on(&user_access, &credential).await?;
    let backend = gents_documents::InferenceBackend {
        physical_doc_id: None,
        node_did: did.to_owned(),
        backend_id: "runtime-use".to_owned(),
        name: "runtime-use".to_owned(),
        provider_kind: crate::backends::provider::BackendProviderKind::OpenAiCompatible,
        openai_wire_api: None,
        endpoint: "http://127.0.0.1:9/v1".to_owned(),
        claude_platform: None,
        auth: gents_documents::BackendAuth::Unauthenticated,
        client_version: None,
        connect_timeout_secs: None,
        discovery_timeout_secs: None,
        request_timeout_secs: None,
        max_concurrent: Some(1),
        max_queue_depth: Some(gents_documents::inference_defaults::DEFAULT_MAX_QUEUE_DEPTH),
        enabled: true,
        tags: Vec::new(),
    };
    gents_documents::write_inference_backend_document(
        &user_access,
        &backend,
        &crate::resolved::ProviderProfilePolicy,
    )
    .await?;
    let escaped = gents_store::graphql::escape_graphql_string(did);
    let node_doc = single_doc_id(
        &user_access,
        "Node",
        &format!(r#"{{node_did: {{_eq: "{escaped}"}}}}"#),
    )
    .await?;
    let backend_doc = single_doc_id(
        &user_access,
        "InferenceBackendMetadata",
        r#"{backend_id: {_eq: "runtime-use"}}"#,
    )
    .await?;

    let cases =
        tokio::task::spawn_blocking(gents_lean_vocab::families::runtime_use_revocation::cases)
            .await?;
    assert_eq!(cases.len(), 16);
    for case in cases {
        let input = &case.input;
        if input.revoke_node_reader {
            user_access
                .delete_actor_relationship("Node", &node_doc, "runtime_reader", &runtime_did)
                .await?;
        }
        if input.revoke_credential_writer {
            user_access
                .delete_actor_relationship(
                    "OAuthCredential",
                    &credential_doc,
                    "runtime_writer",
                    &runtime_did,
                )
                .await?;
        }
        let (collection, doc) = match input.runtime_use.as_str() {
            "oauth_credential" => ("OAuthCredential", &credential_doc),
            "backend" => ("InferenceBackendMetadata", &backend_doc),
            "hop_cap" | "agent_creation" => ("Node", &node_doc),
            other => panic!("unmodeled runtime use {other}"),
        };
        let hosting_read = single_doc_id(
            &admin,
            collection,
            &format!(
                r#"{{_docID: {{_eq: "{}"}}}}"#,
                gents_store::graphql::escape_graphql_string(doc)
            ),
        )
        .await
        .is_ok();
        let may_use = match input.runtime_use.as_str() {
            "oauth_credential" => crate::auth::credential::resolve_oauth_credential(
                &crate::auth::credential::runtime_credential_access(&db.node, did)?,
                did,
                provider,
                crate::auth::credential::AccountPick::ProviderDefault,
            )
            .await?
            .is_some(),
            "backend" => {
                crate::backends::registry::list_enabled_backends_for_runtime(&db.node, did)
                    .await?
                    .iter()
                    .any(|listed| listed.backend_id == backend.backend_id)
            }
            "hop_cap" => {
                agent
                    .transact_readonly("test.runtime_use.hop_cap", |txn| {
                        Box::pin(async move {
                            Ok::<_, anyhow::Error>(txn.runtime_max_request_hop(did).await?)
                        })
                    })
                    .await?
                    == Some(Some(1))
            }
            _ => {
                gents_store::identity::observe_agent_admission(&db.node, did, &user_did, &[])
                    .await?
                    .catalog
            }
        };
        case.check(RevocationObserved {
            hosting_read,
            may_use,
        });
        if input.revoke_node_reader {
            user_access
                .add_actor_relationship("Node", &node_doc, "runtime_reader", &runtime_did)
                .await?;
        }
        if input.revoke_credential_writer {
            user_access
                .add_actor_relationship(
                    "OAuthCredential",
                    &credential_doc,
                    "runtime_writer",
                    &runtime_did,
                )
                .await?;
        }
    }
    Ok(())
}

pub(crate) async fn agent_step(
    access: &ConfigAccess,
    node_did: &str,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    action: &str,
) -> anyhow::Result<()> {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    if action == "delete" {
        let response = access
            .execute(&format!(
                r#"{{ Agent(filter: {{ agent_did: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                gents_store::graphql::escape_graphql_string(&binding.agent_did)
            ))
            .await?;
        let doc = response["data"]["Agent"][0]["_docID"]
            .as_str()
            .expect("published Agent is visible to its actor")
            .to_owned();
        access
            .transact_write("test.grant_sets.agent_delete", |txn| {
                let doc = &doc;
                Box::pin(async move { txn.delete_configuration_document("Agent", doc).await })
            })
            .await?;
        return Ok(());
    }
    let documents =
        gents_documents::test_support::test_agent_documents(node_did, "Baseline agent", binding)
            .into_iter()
            .filter(|(collection, _)| {
                action == "publish" || *collection == gents_store::collection::Collection::Agent
            })
            .map(|(collection, mut value)| {
                if collection == gents_store::collection::Collection::Agent {
                    value["enabled"] = serde_json::json!(action != "disable");
                }
                DesiredStateApplyDocument {
                    collection,
                    add: value.clone(),
                    update: value,
                }
            })
            .collect();
    let plan = DesiredStateApplyPlan::new(documents).expect("Agent plan");
    access
        .transact_write("test.grant_sets.agent", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &crate::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn agent_baseline_projection_follows_modeled_lifecycle() {
    use gents_lean_vocab::families::agent_baseline_lifecycle as wire;

    for (index, case) in gents_lean_vocab::families::agent_baseline_lifecycle::cases::<String>()
        .into_iter()
        .enumerate()
    {
        let case_name = format!("agent-baseline-{index}");
        let (db, owner, owner_access) = gents_documents::test_support::SignedHome::start(
            &case_name,
            gents_store::node_extension::Extensions::none(),
        )
        .await
        .into_parts();
        let runtime = db.node_identity.clone();
        let runtime_did = runtime.did().to_owned();
        let admin =
            ConfigAccess::local_as(db.node.clone(), identity::Did::new(runtime.did()).unwrap());
        let actor_name = format!("{case_name}-actor");
        let actor = tokio::task::spawn_blocking(move || {
            gents_store::test_support::test_identity(&actor_name)
        })
        .await
        .unwrap();
        let actor_did = identity::Did::new(actor.did()).unwrap();
        for set in recipient_sets(&case.input.actor) {
            set.grant(&admin, &actor_did).await.unwrap();
        }
        let node_doc = owner_access.execute("{ Node { _docID } }").await.unwrap()["data"]["Node"]
            [0]["_docID"]
            .as_str()
            .expect("fixture Node")
            .to_owned();
        owner_access
            .add_actor_relationship("Node", &node_doc, "administrator", &actor_did)
            .await
            .unwrap();
        drop(owner);
        let actor_access = db.client_access(&actor);
        let binding = gents_store::identity::provision_agent_identity(runtime.as_ref()).unwrap();
        let agent = identity::Did::new(&binding.agent_did).unwrap();
        let mut steps = Vec::new();
        for action in &case.input.actions {
            let result = agent_step(&actor_access, &runtime_did, &binding, action).await;
            if let Err(error) = &result {
                assert!(
                    gents_protocol::error_chain::find_map_source(error.as_ref(), |cause| {
                        cause
                            .downcast_ref::<ConfigClientError>()
                            .filter(|error| error.is_permission_denied())
                    })
                    .is_some(),
                    "{case_name} {action}: {error:#}"
                );
            }
            let mut installed = Vec::new();
            for permission in GrantSet::AgentBaseline.permissions() {
                if db
                    .node
                    .nac_manager()
                    .check_permission(&agent, *permission)
                    .await
                    .unwrap()
                {
                    installed.push(permission.as_str().to_owned());
                }
            }
            steps.push(wire::StepObservation {
                action: action.clone(),
                committed: result.is_ok(),
                installed,
            });
        }
        case.check(wire::LifecycleOutput { steps });
        db.node.shutdown().await.unwrap();
    }
}
