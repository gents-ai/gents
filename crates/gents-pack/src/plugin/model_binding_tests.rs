use super::*;
use gents_store::identity::NodeIdentity;

#[tokio::test]
async fn plugin_profile_follows_the_immediate_callers_selection() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let key_dir = directory.path().to_owned();
    let (root, agent, sibling) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        Ok((
            gents_store::identity::KeyIdentity::load_or_create(key_dir.join("node"), None)?,
            gents_store::identity::KeyIdentity::load_or_create(key_dir.join("agent"), None)?,
            gents_store::identity::KeyIdentity::load_or_create(key_dir.join("sibling"), None)?,
        ))
    })
    .await??;
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(root.did())
            .with_node_acp_owner(root.did().to_owned())
            .build()
            .await?,
    );
    let context = defra_core::RequestContext::default().with_identity(Some(root.did().to_owned()));
    let policy = node
        .add_dac_policy_with_counter_as(
            &context,
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
    for did in [agent.did(), sibling.did()] {
        for permission in [
            acp::nac::NodePermission::DocumentRead,
            acp::nac::NodePermission::CollectionGet,
        ] {
            node.add_nac_actor_relationship_as(&context, permission.as_str(), did)
                .await?;
        }
    }
    for schema in [
        gents_protocol::schemas::INFERENCE_PROFILE,
        gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
    ] {
        node.add_collection(
            &schema.replace(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID, &policy),
        )
        .await?;
    }
    node.add_collection(&format!(
        "type PluginProfileAgent @policy(id: \"{policy}\", resource: \"agents\") {{ label: String }}"
    ))
    .await?;
    let owner = ConfigAccess::local_as(node.clone(), identity::Did::new(root.did())?);
    let node_did = gents_store::graphql::escape_graphql_string(root.did());
    let agent_doc = gents_store::graphql::created_doc_id(
        &owner
            .write(
                "test.plugin_profile.agent",
                r#"mutation { create_PluginProfileAgent(input: {label: "agent"}) {_docID} }"#,
            )
            .await?,
        "PluginProfileAgent",
    )?;
    let profile_doc = gents_store::graphql::created_doc_id(
        &owner
            .write(
                "test.plugin_profile.profile",
                &format!(
                    r#"mutation {{ create_InferenceProfile(input: {{node_did: "{node_did}", profile_id: "profile", backend_id: "backend", model_name: "model"}}) {{_docID}} }}"#
                ),
            )
            .await?,
        "InferenceProfile",
    )?;
    let backend_doc = gents_store::graphql::created_doc_id(
        &owner
            .write(
                "test.plugin_profile.backend",
                &format!(
                    r#"mutation {{ create_InferenceBackendMetadata(input: {{node_did: "{node_did}", backend_id: "backend", name: "backend", provider_kind: "OpenAiCompatible", endpoint: "http://127.0.0.1:1/v1", auth_kind: "unauthenticated", enabled: true}}) {{_docID}} }}"#
                ),
            )
            .await?,
        "InferenceBackendMetadata",
    )?;
    let agent_did = identity::Did::new(agent.did())?;
    owner
        .add_actor_relationship("PluginProfileAgent", &agent_doc, "executor", &agent_did)
        .await?;
    let selection = |add: bool| {
        let (profile_doc, agent_doc) = (&profile_doc, &agent_doc);
        owner.transact_write("test.plugin_profile.selection", move |txn| {
            let (profile_doc, agent_doc) = (profile_doc.clone(), agent_doc.clone());
            Box::pin(async move {
                if add {
                    txn.add_entity_set_relationship(
                        "InferenceProfile",
                        &profile_doc,
                        "selected_by",
                        "agents",
                        &agent_doc,
                        "executor",
                    )
                    .await
                } else {
                    txn.delete_entity_set_relationship(
                        "InferenceProfile",
                        &profile_doc,
                        "selected_by",
                        "agents",
                        &agent_doc,
                        "executor",
                    )
                    .await
                }
            })
        })
    };
    selection(true).await?;
    owner
        .transact_write("test.plugin_profile.backend_selection", |txn| {
            let (backend_doc, profile_doc) = (&backend_doc, &profile_doc);
            Box::pin(async move {
                txn.add_entity_set_relationship(
                    "InferenceBackendMetadata",
                    backend_doc,
                    "selected_by",
                    "configuration",
                    profile_doc,
                    "selected_by",
                )
                .await
            })
        })
        .await?;
    let models = AccessModels(ConfigAccess::local(node.clone()));
    let binding = ModelBinding {
        node_did: root.did().to_owned(),
        profile_id: "profile".into(),
    };
    let sibling_did = identity::Did::new(sibling.did())?;
    for case in gents_lean_vocab::families::household_plugin_profile::cases() {
        if case.input.revoked {
            selection(false).await?;
        }
        let actor = match case.input.actor.as_deref() {
            Some("agent") => Some(&agent_did),
            Some("sibling") => Some(&sibling_did),
            None => None,
            Some(other) => anyhow::bail!("unmodeled plugin model caller {other}"),
        };
        let resolved = models.resolve(&binding, actor).await;
        if case.input.revoked {
            selection(true).await?;
        }
        case.check(resolved.is_ok());
    }
    node.shutdown().await?;
    Ok(())
}
