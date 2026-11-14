use anyhow::Context;
use gents_store::identity::NodeIdentity;
use gents_store::local::backend_protection::{FreshBackendStore, ProtectedBackendStore};
use gents_store::local::{home, store_key::StoreKey};

async fn locked_home(
    directory: &tempfile::TempDir,
) -> anyhow::Result<(std::path::PathBuf, home::StoreLock)> {
    let home = directory.path().to_owned();
    let data = home.join("data");
    let lock_data = data.clone();
    let lock = tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&lock_data)?;
        Ok::<_, anyhow::Error>(home::lock_store(&home, &lock_data)?)
    })
    .await??;
    Ok((data, lock))
}

async fn create_unbound_legacy_store(
    data: &std::path::Path,
    key: &StoreKey,
    did: &str,
) -> anyhow::Result<()> {
    let unbound = gents_protocol::schemas::INFERENCE_BACKEND.replace(
        &format!(
            " @policy(id: \"{}\", resource: \"secret_configuration\")",
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY_ID
        ),
        "",
    );
    let node = gents_store::local::store_key::persistent_builder(data, key)?
        .with_node_identity_did(did.to_owned())
        .build()
        .await?;
    node.add_collection(&unbound).await?;
    let created = node.execute(r#"mutation { create_InferenceBackend(input:{node_did:"legacy", backend_id:"b", name:"old", provider_kind:"OpenAiCompatible", endpoint:"https://provider.example", auth:{kind:"api_key",key:"legacy-secret"}}) {_docID} }"#).await;
    anyhow::ensure!(
        !created.has_errors(),
        "legacy fixture create failed: {:?}",
        created.errors
    );
    node.shutdown().await?;
    Ok(())
}

async fn assert_legacy_store_untouched(
    data: &std::path::Path,
    key: &StoreKey,
    did: &str,
) -> anyhow::Result<()> {
    let node = gents_store::local::store_key::persistent_builder(data, key)?
        .with_node_identity_did(did.to_owned())
        .build()
        .await?;
    let retained = node.execute("{ InferenceBackend { auth } }").await;
    anyhow::ensure!(
        !retained.has_errors(),
        "legacy fixture read failed: {:?}",
        retained.errors
    );
    let rows = retained.data.context("legacy fixture data missing")?;
    assert_eq!(rows["InferenceBackend"][0]["auth"]["key"], "legacy-secret");
    assert!(node
        .get_collection(gents_protocol::schemas::NODE_NAME)?
        .is_none());
    node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn only_the_fresh_store_and_its_key_can_reopen_protected_backend() -> anyhow::Result<()> {
    for case in gents_lean_vocab::families::household_fresh_backend_reopen::cases() {
        let directory = tempfile::tempdir()?;
        let (data, lock) = locked_home(&directory).await?;
        let key = StoreKey::from_host(&[7; 32])?;
        let other_key = StoreKey::from_host(&[8; 32])?;
        let identity_path = directory.path().join("node.key");
        let runtime = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(std::sync::Arc::new(
                gents_store::identity::KeyIdentity::load_or_create(identity_path, None)?,
            ))
        })
        .await??;
        let did = runtime.did().to_owned();
        if case.input.binding {
            FreshBackendStore::observe(&data, &lock)
                .await?
                .initialize(&key, Some(&did))
                .await?;
        } else {
            create_unbound_legacy_store(&data, &key, &did).await?;
            if case.input.marker {
                let witness = tempfile::tempdir()?;
                let (witness_data, witness_lock) = locked_home(&witness).await?;
                FreshBackendStore::observe(&witness_data, &witness_lock)
                    .await?
                    .initialize(&key, Some(&did))
                    .await?;
                tokio::fs::copy(
                    witness_data.join(gents_store::local::backend_protection::PROVENANCE_MARKER),
                    data.join(gents_store::local::backend_protection::PROVENANCE_MARKER),
                )
                .await?;
            }
        }
        let open_key = if case.input.marker || !case.input.binding {
            &key
        } else {
            &other_key
        };
        let allowed = match ProtectedBackendStore::open(&data, open_key, &lock).await {
            Err(_) => false,
            Ok(opened) => match opened.preflight(&key, Some(&did)).await {
                Err(_) => false,
                Ok(protected) => {
                    assert!(FreshBackendStore::observe(&data, &lock).await.is_err());
                    let (node, admission) = protected
                        .start_node_with_projection(
                            &key,
                            runtime,
                            None,
                            None,
                            std::sync::Arc::new(crate::surface::ToolCeiling::meta_only()),
                            Vec::new(),
                            &gents_session::test_support::no_lifecycle_plugins(),
                        )
                        .await?;
                    gents_store::schema::ensure_runtime_schemas(&node).await?;
                    for entry in gents_migration::DEFAULT_BASELINE.iter() {
                        let collection = node.get_collection(entry.name)?.with_context(|| {
                            format!("missing fresh baseline collection {}", entry.name)
                        })?;
                        assert_eq!(
                            Some(collection.version_id.as_str()),
                            entry.expected_version,
                            "fresh protected baseline lineage for {}",
                            entry.name
                        );
                    }
                    let validated = if case.input.store {
                        admission.validate(&node).await.is_ok()
                    } else {
                        let foreign = std::sync::Arc::new(
                            defra_node::Node::builder()
                                .with_node_identity_did(did.clone())
                                .with_node_acp_enabled()
                                .build()
                                .await?,
                        );
                        let validated = admission.validate(&foreign).await.is_ok();
                        foreign.shutdown().await?;
                        validated
                    };
                    node.shutdown().await?;
                    validated
                }
            },
        };
        if !case.input.marker && !case.input.binding {
            assert_legacy_store_untouched(&data, &key, &did).await?;
        }
        case.check(allowed);
    }
    Ok(())
}

#[tokio::test]
async fn a_node_reopened_with_nac_disabled_is_refused_at_startup() -> anyhow::Result<()> {
    for case in gents_lean_vocab::families::node_provisioning_runtime_nac::cases::<String>() {
        let directory = tempfile::tempdir()?;
        let home = directory.path().to_owned();
        let data = home.join("data");
        let lock_home = home.clone();
        let lock_data = data.clone();
        let lock = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&lock_data)?;
            Ok::<_, anyhow::Error>(home::lock_store(&lock_home, &lock_data)?)
        })
        .await??;
        let key = StoreKey::from_host(&[9; 32])?;
        let identity_path = home.join("node.key");
        let runtime = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(std::sync::Arc::new(
                gents_store::identity::KeyIdentity::load_or_create(identity_path, None)?,
            ))
        })
        .await??;
        let did = runtime.did().to_owned();
        FreshBackendStore::observe(&data, &lock)
            .await?
            .initialize(&key, Some(&did))
            .await?;
        let (node, admission) = ProtectedBackendStore::open(&data, &key, &lock)
            .await?
            .preflight(&key, Some(&did))
            .await?
            .start_node_with_projection(
                &key,
                runtime,
                None,
                None,
                std::sync::Arc::new(crate::surface::ToolCeiling::meta_only()),
                Vec::new(),
                &gents_session::test_support::no_lifecycle_plugins(),
            )
            .await?;
        match case.input.status.as_str() {
            "enabled" => {}
            "disabled_temporarily" => {
                let owner = defra_core::RequestContext::default().with_identity(Some(did.clone()));
                node.disable_nac_as(&owner).await?;
            }
            other => panic!("no production start reaches NAC status {other}"),
        }
        let admitted = match admission.validate(&node).await {
            Ok(()) => true,
            Err(
                gents_store::local::backend_protection::BackendProtectionError::NodeAcpNotEnabled {
                    ..
                },
            ) => false,
            Err(error) => panic!("{:?} refused for another reason: {error}", case.input),
        };
        node.shutdown().await?;
        drop(node);
        let restarted = match ProtectedBackendStore::open(&data, &key, &lock)
            .await?
            .preflight(&key, Some(&did))
            .await
        {
            Ok(_) => true,
            Err(
                error @ gents_store::local::backend_protection::BackendProtectionError::NodeAcpNotEnabled {
                    ..
                },
            ) => {
                assert!(error.to_string().contains("re-enable NAC"), "{error}");
                false
            }
            Err(error) => panic!("{:?} preflight refused for another reason: {error}", case.input),
        };
        assert_eq!(
            admitted, restarted,
            "{:?}: the running node and its restart disagree",
            case.input
        );
        case.check(
            gents_lean_vocab::families::node_provisioning_runtime_nac::NacOutput {
                may_start: admitted,
            },
        );
    }
    Ok(())
}
